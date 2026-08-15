# osmdb-extract

`osmdb-extract` is a command-line tool that turns an [osmdb](https://github.com/HelloJowet/osmdb) database into typed geospatial datasets. Define layers in Lua to select OpenStreetMap nodes, ways, and relations, retain the attributes you need, and write the result as GeoPackage or GeoParquet.

It builds way and relation geometries as it streams through the database, letting you create focused OSM extracts without loading the entire source dataset or all of its geometry into memory.

Use a small Lua script to choose which OpenStreetMap objects become rows, which attributes to keep, and which geometry to write. The result is a typed GeoPackage or GeoParquet dataset.

## Quick start

Install the CLI:

```bash
cargo install osmdb-extract
```

You need an existing osmdb database directory containing `data.rocksdb` and `locations.bin`. See the [osmdb project](https://github.com/HelloJowet/osmdb) for creating one.

Run the bundled example to extract cafés and major roads into a GeoPackage:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script examples/basic.lua \
  --format geopackage \
  --output region.gpkg \
  -v
```

The script creates a `cafes` point layer and a `roads` line layer. Use `--threads 8` to set the number of parallel Lua workers; by default, all available CPU cores are used.

## Outputs and examples

- `--format geopackage` writes one new `.gpkg` file.
- `--format geoparquet` writes a new directory containing one `<layer>.parquet` file per Lua layer. Spatial layers include GeoParquet metadata, while layers without geometry are ordinary Parquet tables.
- Existing output paths are refused. Output is first written beside the destination and renamed only after extraction succeeds.

The `examples/` directory contains scripts you can adapt:

- `basic.lua` extracts café points and major-road lines.
- `cafes.lua` extracts a small point layer for one amenity.
- `major_roads.lua` extracts motorway, trunk, and primary ways.
- `route_metadata.lua` writes route-relation attributes without building relation geometry.

Use `-v` for progress information and `-vv` for diagnostics when geometry cannot be created. The final summary reports processed objects, written rows, and skipped geometries.

## Lua scripts

Define one or more layers when the script loads, then implement callbacks for the object types you need:

```lua
local roads = osmdb.define_layer({
    name = "roads",
    source = "way",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "name", type = "string" },
        { name = "tags", type = "json" },
        { name = "geometry", type = "linestring", required = true },
    },
})

function osmdb.process_way(object)
    if object.tags.highway then
        roads:insert({
            osm_id = object.id,
            name = object.tags.name,
            tags = object.tags,
            geometry = object:as_linestring(),
        })
    end
end
```

Layers use `source = "node"`, `"way"`, or `"relation"` and may receive rows only from the matching callback. The available callbacks are `osmdb.process_node(object)`, `osmdb.process_way(object)`, and `osmdb.process_relation(object)`.

| Source     | Fields                                        | Geometry methods                                                     |
| ---------- | --------------------------------------------- | -------------------------------------------------------------------- |
| `node`     | `id`, `version`, `tags`, `lat`, `lon`         | `as_point()`                                                         |
| `way`      | `id`, `version`, `tags`, `nodes`, `is_closed` | `as_linestring()`                                                    |
| `relation` | `id`, `version`, `tags`, `members`            | `as_multipoint()`, `as_multilinestring()`, `as_geometrycollection()` |

Relation members have `type`, `id`, and `role`. Relation geometry follows nested relations, detects cycles, and uses EPSG:4326 coordinates.

Supported column types are `string`, `int64`, `double`, `boolean`, `json`, `point`, `linestring`, `multipoint`, `multilinestring`, and `geometrycollection`. Columns are nullable unless `required = true`; each layer can have one geometry column. Layer and column names must be unique ASCII identifiers, and `fid` is reserved for GeoPackage. IDs are not added automatically, so include an ID column when you need one.

If geometry is missing, invalid, cyclic, or empty, all rows staged for that object are skipped and extraction continues. Invalid layer definitions, wrong value types, unknown fields, source mismatches, and geometry-type mismatches stop extraction with a script error.

## Roadmap

- Polygon and multipolygon assembly, including closed-way polygon interpretation
- CRS reprojection and per-layer CRS selection
- Additional output formats
- Optional and deferred GeoPackage spatial indexes
- Incremental extraction and update workflows
