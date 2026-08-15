use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use osmdb::database::BatchData;
use osmdb::{
    Database, DatabaseConfig, DatabaseWriter, Location, LocationStoreWriter, Member, MemberType,
    Node, Relation, Way,
};
use osmdb_extract::{ExtractOptions, OutputFormat, extract};
use parquet::file::reader::{FileReader, SerializedFileReader};
use tempfile::TempDir;

fn tags(values: &[(&str, &str)]) -> HashMap<String, String> {
    values
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn fixture(root: &Path) -> PathBuf {
    let base = root.join("input.osmdb");
    fs::create_dir(&base).unwrap();
    let database =
        Database::open(&DatabaseConfig::for_bulk_load(base.join("data.rocksdb"))).unwrap();
    let writer = DatabaseWriter::new(database.inner_arc());
    writer
        .write_batch(BatchData::Nodes(vec![
            (
                1,
                Node {
                    latitude: 10_000_000,
                    longitude: 20_000_000,
                    tags: tags(&[("amenity", "cafe"), ("name", "One")]),
                    version: 1,
                },
            ),
            (
                2,
                Node {
                    latitude: 20_000_000,
                    longitude: 30_000_000,
                    tags: tags(&[("amenity", "bench")]),
                    version: 2,
                },
            ),
        ]))
        .unwrap();
    writer
        .write_batch(BatchData::Ways(vec![
            (
                10,
                Way {
                    node_ids: vec![1, 2],
                    tags: tags(&[("highway", "residential")]),
                    version: 1,
                },
            ),
            (
                11,
                Way {
                    node_ids: vec![3, 4],
                    tags: tags(&[("highway", "service")]),
                    version: 1,
                },
            ),
        ]))
        .unwrap();
    writer
        .write_batch(BatchData::Relations(vec![
            (
                20,
                Relation {
                    members: vec![
                        Member {
                            member_type: MemberType::Node,
                            id: 1,
                            role: "stop".into(),
                        },
                        Member {
                            member_type: MemberType::Way,
                            id: 10,
                            role: "".into(),
                        },
                        Member {
                            member_type: MemberType::Relation,
                            id: 21,
                            role: "child".into(),
                        },
                    ],
                    tags: tags(&[("type", "route")]),
                    version: 1,
                },
            ),
            (
                21,
                Relation {
                    members: vec![
                        Member {
                            member_type: MemberType::Node,
                            id: 2,
                            role: "stop".into(),
                        },
                        Member {
                            member_type: MemberType::Way,
                            id: 11,
                            role: "".into(),
                        },
                    ],
                    tags: tags(&[("type", "route")]),
                    version: 1,
                },
            ),
            (
                30,
                Relation {
                    members: vec![Member {
                        member_type: MemberType::Way,
                        id: 999,
                        role: "".into(),
                    }],
                    tags: tags(&[("type", "route")]),
                    version: 1,
                },
            ),
            (
                40,
                Relation {
                    members: vec![Member {
                        member_type: MemberType::Relation,
                        id: 41,
                        role: "".into(),
                    }],
                    tags: tags(&[("type", "route")]),
                    version: 1,
                },
            ),
            (
                41,
                Relation {
                    members: vec![Member {
                        member_type: MemberType::Relation,
                        id: 40,
                        role: "".into(),
                    }],
                    tags: tags(&[("type", "route")]),
                    version: 1,
                },
            ),
        ]))
        .unwrap();
    drop(writer);
    drop(database);

    let mut locations = LocationStoreWriter::create(base.join("locations.bin")).unwrap();
    locations
        .write_batch(&[
            (1, Location::from_degrees(1.0, 2.0, 1)),
            (2, Location::from_degrees(2.0, 3.0, 1)),
            (3, Location::from_degrees(3.0, 4.0, 1)),
            (4, Location::from_degrees(4.0, 5.0, 1)),
        ])
        .unwrap();
    locations.finish().unwrap();
    base
}

fn script(root: &Path, body: &str) -> PathBuf {
    let path = root.join("extract.lua");
    fs::write(&path, body).unwrap();
    path
}

const HAPPY_SCRIPT: &str = r#"
local pois = osmdb.define_layer({ name = 'pois', source = 'node', columns = {
    { name = 'osm_id', type = 'int64', required = true },
    { name = 'tags', type = 'json' },
    { name = 'geometry', type = 'point', required = true },
} })
local node_attributes = osmdb.define_layer({ name = 'node_attributes', source = 'node', columns = {
    { name = 'osm_id', type = 'int64', required = true },
    { name = 'name', type = 'string' },
} })
local roads = osmdb.define_layer({ name = 'roads', source = 'way', columns = {
    { name = 'osm_id', type = 'int64', required = true },
    { name = 'closed', type = 'boolean', required = true },
    { name = 'geometry', type = 'linestring', required = true },
} })
local relations = osmdb.define_layer({ name = 'relations', source = 'relation', columns = {
    { name = 'osm_id', type = 'int64', required = true },
    { name = 'geometry', type = 'geometrycollection', required = true },
} })
function osmdb.process_node(object)
    node_attributes:insert({ osm_id = object.id, name = object.tags.name })
    if object.tags.amenity == 'cafe' then
        pois:insert({ osm_id = object.id, tags = object.tags, geometry = object:as_point() })
    end
end
function osmdb.process_way(object)
    roads:insert({ osm_id = object.id, closed = object.is_closed, geometry = object:as_linestring() })
end
function osmdb.process_relation(object)
    relations:insert({ osm_id = object.id, geometry = object:as_geometrycollection() })
end
"#;

#[test]
fn writes_geopackage_and_skips_bad_geometry() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), HAPPY_SCRIPT);
    let output = temp.path().join("result.gpkg");
    let summary = extract(ExtractOptions {
        db,
        script,
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
    })
    .unwrap();
    assert_eq!(summary.nodes_processed, 2);
    assert_eq!(summary.ways_processed, 2);
    assert_eq!(summary.relations_processed, 5);
    assert_eq!(summary.geometry_skipped, 3);
    assert_eq!(summary.rows_written, 7);

    let connection = rusqlite::Connection::open(output).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pois", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM roads", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM node_attributes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let attribute_type: String = connection
        .query_row(
            "SELECT data_type FROM gpkg_contents WHERE table_name='node_attributes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attribute_type, "attributes");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM relations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let geometry_type: String = connection
        .query_row(
            "SELECT geometry_type_name FROM gpkg_geometry_columns WHERE table_name='relations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(geometry_type, "GEOMETRYCOLLECTION");
    let srs: i64 = connection
        .query_row(
            "SELECT srs_id FROM gpkg_contents WHERE table_name='roads'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(srs, 4326);
    let tags_json: String = connection
        .query_row("SELECT tags FROM pois", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&tags_json).unwrap()["amenity"],
        "cafe"
    );
}

#[test]
fn writes_one_geoparquet_file_per_layer() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), HAPPY_SCRIPT);
    let output = temp.path().join("parquet-output");
    let summary = extract(ExtractOptions {
        db,
        script,
        format: OutputFormat::Geoparquet,
        output: output.clone(),
        threads: 2,
    })
    .unwrap();
    assert_eq!(summary.rows_written, 7);
    for (name, expected_rows) in [
        ("pois", 1),
        ("roads", 2),
        ("relations", 2),
        ("node_attributes", 2),
    ] {
        let reader = SerializedFileReader::new(
            fs::File::open(output.join(format!("{name}.parquet"))).unwrap(),
        )
        .unwrap();
        assert_eq!(reader.metadata().file_metadata().num_rows(), expected_rows);
        let geo = reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .and_then(|metadata| metadata.iter().find(|entry| entry.key == "geo"));
        if name == "node_attributes" {
            assert!(geo.is_none());
        } else {
            assert!(geo.unwrap().value.as_ref().unwrap().contains("geometry"));
        }
    }
}

#[test]
fn geometry_type_mismatch_aborts_without_publishing_output() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(
        temp.path(),
        r#"
        local output = osmdb.define_layer({ name = 'bad', source = 'relation', columns = {
            { name = 'geometry', type = 'multipoint', required = true },
        } })
        function osmdb.process_relation(object)
            local ok = pcall(function()
                output:insert({ geometry = object:as_multilinestring() })
            end)
        end
    "#,
    );
    let output = temp.path().join("bad.gpkg");
    let error = extract(ExtractOptions {
        db,
        script,
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("expected multipoint geometry"),
        "{error:#}"
    );
    assert!(!output.exists());
    assert!(!fs::read_dir(temp.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bad.gpkg.tmp-")
    }));
}

#[test]
fn rejects_existing_output() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), HAPPY_SCRIPT);
    let output = temp.path().join("exists.gpkg");
    fs::write(&output, b"keep me").unwrap();
    let error = extract(ExtractOptions {
        db,
        script,
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 1,
    })
    .unwrap_err();
    assert!(error.to_string().contains("already exists"));
    assert_eq!(fs::read(output).unwrap(), b"keep me");
}
