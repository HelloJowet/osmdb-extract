use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use arrow_array::{Array, Int64Array, StringArray};
use osmdb::database::BatchData;
use osmdb::{
    Database, DatabaseConfig, DatabaseWriter, Location, LocationStoreWriter, Member, MemberType,
    Node, Relation, Way,
};
use osmdb_extract::{ExtractOptions, OutputFormat, extract};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};
use tempfile::TempDir;
use wikidata_store::create_wikidata_store;

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
                    tags: tags(&[
                        ("amenity", "cafe"),
                        ("name", "One"),
                        ("place", "city"),
                        ("wikidata", "Q42"),
                    ]),
                    version: 1,
                },
            ),
            (
                2,
                Node {
                    latitude: 20_000_000,
                    longitude: 30_000_000,
                    tags: tags(&[
                        ("amenity", "bench"),
                        ("name", "Two"),
                        ("place", "village"),
                        ("wikidata", "Q43"),
                    ]),
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
    script_named(root, "extract.lua", body)
}

fn script_named(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn wikidata_fixture(root: &Path) -> (PathBuf, serde_json::Value) {
    let entity = serde_json::json!({
        "id": "Q42",
        "type": "item",
        "labels": {
            "en": { "language": "en", "value": "Douglas Adams" }
        },
        "aliases": { "en": [] },
        "claims": {
            "P31": [{
                "id": "Q42$statement",
                "mainsnak": {
                    "snaktype": "value",
                    "property": "P31",
                    "datavalue": {
                        "type": "wikibase-entityid",
                        "value": { "entity-type": "item", "id": "Q5", "numeric-id": 5 }
                    }
                },
                "type": "statement",
                "rank": "normal",
                "qualifiers": {},
                "references": []
            }],
            "P1082": [
                {
                    "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+150", "unit": "1" } } },
                    "rank": "normal", "qualifiers": {}
                },
                {
                    "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+100", "unit": "1" } } },
                    "rank": "normal", "qualifiers": { "P585": [{ "snaktype": "value", "datavalue": { "type": "time", "value": { "time": "+00000002020-01-01T00:00:00Z" } } }] }
                },
                {
                    "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+50", "unit": "1" } } },
                    "rank": "preferred", "qualifiers": { "P585": [{ "snaktype": "value", "datavalue": { "type": "time", "value": { "time": "+00000002019-01-01T00:00:00Z" } } }] }
                },
                {
                    "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+200.5", "unit": "1" } } },
                    "rank": "normal", "qualifiers": { "P585": [{ "snaktype": "value", "datavalue": { "type": "time", "value": { "time": "+00000002023-01-01T00:00:00Z" } } }] }
                },
                {
                    "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+999", "unit": "1" } } },
                    "rank": "deprecated", "qualifiers": { "P585": [{ "snaktype": "value", "datavalue": { "type": "time", "value": { "time": "+00000002024-01-01T00:00:00Z" } } }] }
                }
            ],
            "P18": [
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "Spelterini Blüemlisalp.jpg" } }, "rank": "normal" },
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "Example image.png" } }, "rank": "preferred" },
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "Ignored.jpg" } }, "rank": "deprecated" }
            ],
            "P646": [
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "/m/old" } }, "rank": "normal" },
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "/m/new" } }, "rank": "preferred" }
            ],
            "P2671": [
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "old-id" } }, "rank": "normal" },
                { "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "new-id" } }, "rank": "preferred" }
            ]
        },
        "sitelinks": {},
        "lastrevid": null
    });
    let undated_entity = serde_json::json!({
        "id": "Q43",
        "type": "item",
        "labels": { "en": { "language": "en", "value": "Undated settlement" } },
        "aliases": { "en": [] },
        "claims": {
            "P1082": [{
                "mainsnak": { "snaktype": "value", "datavalue": { "type": "quantity", "value": { "amount": "+150", "unit": "1" } } },
                "rank": "normal", "qualifiers": {}
            }]
        },
        "sitelinks": {},
        "lastrevid": null
    });
    let dump = root.join("wikidata.json");
    fs::write(&dump, format!("{entity}\n{undated_entity}\n")).unwrap();
    let store = root.join("wikidata.store");
    create_wikidata_store(dump, &store).unwrap();
    (store, entity)
}

const WIKIDATA_SCRIPT: &str = r#"
local entities = osmdb.define_layer({ name = 'entities', source = 'node', columns = {
    { name = 'osm_id', type = 'int64', required = true },
    { name = 'wikidata', type = 'json', required = true },
} })
function osmdb.process_node(object)
    if not object.tags.wikidata then return end
    local entity = osmdb.wikidata(object.tags.wikidata)
    if entity
        and entity.labels.en.value == 'Douglas Adams'
        and entity.claims.P31[1].mainsnak.datavalue.value.id == 'Q5'
        and osmdb.wikidata('Q999999999') == nil then
        entities:insert({ osm_id = object.id, wikidata = entity })
    end
end
"#;

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
fn combines_multiple_scripts_in_one_output() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let nodes = script_named(
        temp.path(),
        "nodes.lua",
        r#"
        local nodes = osmdb.define_layer({ name = 'script_nodes', source = 'node', columns = {
            { name = 'osm_id', type = 'int64', required = true },
        } })
        function osmdb.process_node(object)
            nodes:insert({ osm_id = object.id })
        end
        "#,
    );
    let ways = script_named(
        temp.path(),
        "ways.lua",
        r#"
        local ways = osmdb.define_layer({ name = 'script_ways', source = 'way', columns = {
            { name = 'osm_id', type = 'int64', required = true },
        } })
        function osmdb.process_way(object)
            ways:insert({ osm_id = object.id })
        end
        "#,
    );

    for format in [OutputFormat::Geopackage, OutputFormat::Geoparquet] {
        let output = match format {
            OutputFormat::Geopackage => temp.path().join("multiple.gpkg"),
            OutputFormat::Geoparquet => temp.path().join("multiple-parquet"),
        };
        let summary = extract(ExtractOptions {
            db: db.clone(),
            scripts: vec![nodes.clone(), ways.clone()],
            format,
            output: output.clone(),
            threads: 2,
            wikidata_store: None,
        })
        .unwrap();

        assert_eq!(summary.nodes_processed, 2);
        assert_eq!(summary.ways_processed, 2);
        assert_eq!(summary.relations_processed, 5);
        assert_eq!(summary.rows_written, 4);
        match format {
            OutputFormat::Geopackage => {
                let connection = rusqlite::Connection::open(output).unwrap();
                for layer in ["script_nodes", "script_ways"] {
                    let count: i64 = connection
                        .query_row(&format!("SELECT COUNT(*) FROM {layer}"), [], |row| {
                            row.get(0)
                        })
                        .unwrap();
                    assert_eq!(count, 2);
                }
            }
            OutputFormat::Geoparquet => {
                assert!(output.join("script_nodes.parquet").is_file());
                assert!(output.join("script_ways.parquet").is_file());
            }
        }
    }
}

#[test]
fn rejects_duplicate_layers_across_scripts() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let first = script_named(
        temp.path(),
        "first.lua",
        "osmdb.define_layer({ name = 'shared', source = 'node', columns = {{ name = 'id', type = 'int64' }} })",
    );
    let second = script_named(
        temp.path(),
        "second.lua",
        "osmdb.define_layer({ name = 'shared', source = 'way', columns = {{ name = 'id', type = 'int64' }} })",
    );
    let output = temp.path().join("duplicate.gpkg");

    let error = extract(ExtractOptions {
        db,
        scripts: vec![first, second],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 1,
        wikidata_store: None,
    })
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("duplicate layer name 'shared'"),
        "{message}"
    );
    assert!(message.contains("first.lua"), "{message}");
    assert!(message.contains("second.lua"), "{message}");
    assert!(!output.exists());
}

#[test]
fn geometry_failure_in_one_script_discards_all_rows_for_the_object() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let metadata = script_named(
        temp.path(),
        "metadata.lua",
        r#"
        local metadata = osmdb.define_layer({ name = 'relation_metadata', source = 'relation', columns = {
            { name = 'osm_id', type = 'int64', required = true },
        } })
        function osmdb.process_relation(object)
            metadata:insert({ osm_id = object.id })
        end
        "#,
    );
    let geometry = script_named(
        temp.path(),
        "geometry.lua",
        r#"
        local geometry = osmdb.define_layer({ name = 'relation_geometry', source = 'relation', columns = {
            { name = 'geometry', type = 'multilinestring', required = true },
        } })
        function osmdb.process_relation(object)
            if object.id == 30 then
                geometry:insert({ geometry = object:as_multilinestring() })
            end
        end
        "#,
    );
    let output = temp.path().join("geometry-skip.gpkg");

    let summary = extract(ExtractOptions {
        db,
        scripts: vec![metadata, geometry],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
        wikidata_store: None,
    })
    .unwrap();

    assert_eq!(summary.geometry_skipped, 1);
    assert_eq!(summary.rows_written, 4);
    let connection = rusqlite::Connection::open(output).unwrap();
    let skipped: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM relation_metadata WHERE osm_id = 30",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(skipped, 0);
}

#[test]
fn writes_geopackage_and_skips_bad_geometry() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), HAPPY_SCRIPT);
    let output = temp.path().join("result.gpkg");
    let summary = extract(ExtractOptions {
        db,
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
        wikidata_store: None,
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
        scripts: vec![script],
        format: OutputFormat::Geoparquet,
        output: output.clone(),
        threads: 2,
        wikidata_store: None,
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
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
        wikidata_store: None,
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
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 1,
        wikidata_store: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("already exists"));
    assert_eq!(fs::read(output).unwrap(), b"keep me");
}

#[test]
fn wikidata_lookup_is_available_to_parallel_lua_workers_and_preserves_json() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), WIKIDATA_SCRIPT);
    let (wikidata_store, expected_entity) = wikidata_fixture(temp.path());

    for format in [OutputFormat::Geopackage, OutputFormat::Geoparquet] {
        let output = match format {
            OutputFormat::Geopackage => temp.path().join("wikidata.gpkg"),
            OutputFormat::Geoparquet => temp.path().join("wikidata-parquet"),
        };
        let summary = extract(ExtractOptions {
            db: db.clone(),
            scripts: vec![script.clone()],
            format,
            output: output.clone(),
            threads: 2,
            wikidata_store: Some(wikidata_store.clone()),
        })
        .unwrap();
        assert_eq!(summary.rows_written, 1);

        let json = match format {
            OutputFormat::Geopackage => {
                let connection = rusqlite::Connection::open(output).unwrap();
                connection
                    .query_row("SELECT wikidata FROM entities", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .unwrap()
            }
            OutputFormat::Geoparquet => {
                let file = fs::File::open(output.join("entities.parquet")).unwrap();
                let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
                    .unwrap()
                    .build()
                    .unwrap();
                let batch = reader.next().unwrap().unwrap();
                batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap()
                    .value(0)
                    .to_owned()
            }
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            expected_entity
        );
    }
}

#[test]
fn places_example_enriches_settlements_from_wikidata() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), include_str!("../examples/places.lua"));
    let (wikidata_store, _) = wikidata_fixture(temp.path());

    for format in [OutputFormat::Geopackage, OutputFormat::Geoparquet] {
        let output = match format {
            OutputFormat::Geopackage => temp.path().join("places.gpkg"),
            OutputFormat::Geoparquet => temp.path().join("places-parquet"),
        };
        let summary = extract(ExtractOptions {
            db: db.clone(),
            scripts: vec![script.clone()],
            format,
            output: output.clone(),
            threads: 2,
            wikidata_store: Some(wikidata_store.clone()),
        })
        .unwrap();
        assert_eq!(summary.rows_written, 2);

        let (population, images, freebase_id, knowledge_graph_id, undated_population) = match format
        {
            OutputFormat::Geopackage => {
                let connection = rusqlite::Connection::open(output).unwrap();
                let enriched = connection
                    .query_row(
                        "SELECT population, images, freebase_id, google_knowledge_graph_id FROM places WHERE osm_id = 1",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
                    )
                    .unwrap();
                let undated_population = connection
                    .query_row(
                        "SELECT population FROM places WHERE osm_id = 2",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap();
                (
                    enriched.0,
                    enriched.1,
                    enriched.2,
                    enriched.3,
                    undated_population,
                )
            }
            OutputFormat::Geoparquet => {
                let file = fs::File::open(output.join("places.parquet")).unwrap();
                let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
                    .unwrap()
                    .build()
                    .unwrap();
                let batch = reader.next().unwrap().unwrap();
                let ids = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                let index = (0..ids.len()).find(|&index| ids.value(index) == 1).unwrap();
                let undated_index = (0..ids.len()).find(|&index| ids.value(index) == 2).unwrap();
                let population = batch
                    .column(3)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                let images = batch
                    .column(4)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let freebase = batch
                    .column(5)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let knowledge_graph = batch
                    .column(6)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                (
                    population.value(index),
                    images.value(index).to_owned(),
                    freebase.value(index).to_owned(),
                    knowledge_graph.value(index).to_owned(),
                    population.value(undated_index),
                )
            }
        };

        assert_eq!(population, 100);
        assert_eq!(freebase_id, "/m/new");
        assert_eq!(knowledge_graph_id, "new-id");
        assert_eq!(undated_population, 150);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&images).unwrap(),
            vec![
                "Spelterini Blüemlisalp.jpg",
                "Example image.png",
            ]
        );
    }
}

#[test]
fn wikidata_lookup_returns_nil_when_no_store_is_configured() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(
        temp.path(),
        r#"
        local output = osmdb.define_layer({ name = 'without_wikidata', source = 'node', columns = {
            { name = 'osm_id', type = 'int64', required = true },
        } })
        function osmdb.process_node(object)
            if osmdb.wikidata('Q42') == nil then
                output:insert({ osm_id = object.id })
            end
        end
    "#,
    );
    let output = temp.path().join("without-wikidata.gpkg");
    let summary = extract(ExtractOptions {
        db,
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output,
        threads: 2,
        wikidata_store: None,
    })
    .unwrap();
    assert_eq!(summary.rows_written, 2);
}

#[test]
fn invalid_wikidata_id_aborts_without_publishing_output() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let (wikidata_store, _) = wikidata_fixture(temp.path());
    let script = script(
        temp.path(),
        r#"
        local output = osmdb.define_layer({ name = 'invalid', source = 'node', columns = {
            { name = 'osm_id', type = 'int64', required = true },
        } })
        function osmdb.process_node(object)
            pcall(function() osmdb.wikidata('q42') end)
            output:insert({ osm_id = object.id })
        end
    "#,
    );
    let output = temp.path().join("invalid-wikidata.gpkg");
    let error = extract(ExtractOptions {
        db,
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 2,
        wikidata_store: Some(wikidata_store),
    })
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("Unsupported Wikidata entity ID: q42"),
        "{error:#}"
    );
    assert!(!output.exists());
}

#[test]
fn invalid_wikidata_store_fails_before_creating_output() {
    let temp = TempDir::new().unwrap();
    let db = fixture(temp.path());
    let script = script(temp.path(), HAPPY_SCRIPT);
    let wikidata_store = temp.path().join("invalid-wikidata.store");
    fs::create_dir(&wikidata_store).unwrap();
    let output = temp.path().join("invalid-store.gpkg");
    let error = extract(ExtractOptions {
        db,
        scripts: vec![script],
        format: OutputFormat::Geopackage,
        output: output.clone(),
        threads: 1,
        wikidata_store: Some(wikidata_store),
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("failed to open Wikidata store"),
        "{error:#}"
    );
    assert!(!output.exists());
}
