# osmdb-extract

`osmdb-extract` creates typed GeoPackage and GeoParquet datasets from an [osmdb](https://github.com/HelloJowet/osmdb) database. Lua scripts select the OpenStreetMap (OSM) objects and attributes to export, construct their geometries, and optionally enrich them with entities from a local Wikidata store.

Its main strengths are:

- OSM and Wikidata can be combined directly while producing each output row.
- Way and relation geometries are resolved on demand from disk, without loading global node or way indexes into memory.
- Multiple focused Lua scripts can share one scan of the OSM database and write typed layers to the same output.

## Quick start

Install the command-line tool:

```bash
cargo install osmdb-extract
```

You need an osmdb database directory containing `data.rocksdb` and `locations.bin`. See the [osmdb project](https://github.com/HelloJowet/osmdb) for instructions on creating one.

Run the bundled example to extract cafés and major roads:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script examples/basic.lua \
  --format geopackage \
  --output region.gpkg \
  -v
```

This creates a `cafes` point layer and a `roads` line layer in `region.gpkg`.

## How extraction works

A Lua script defines one or more output layers and receives each relevant OSM object through a callback. The callback decides whether to keep the object and which values to write. Geometry and optional Wikidata data are requested only when the script needs them.

This example creates a line layer containing road-like ways:

```lua
local roads = osmdb.define_layer({
    name = "roads",
    source = "way",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "name", type = "string" },
        { name = "geometry", type = "linestring", required = true },
    },
})

function osmdb.process_way(object)
    if object.tags.highway then
        roads:insert({
            osm_id = object.id,
            name = object.tags.name,
            geometry = object:as_linestring(),
        })
    end
end
```

Each layer reads from one source: `node`, `way`, or `relation`. Rows for that layer must be inserted from the matching `osmdb.process_node`, `osmdb.process_way`, or `osmdb.process_relation` callback.

## Combine OSM and Wikidata

Pass a read-only store created with [wikidata_store](https://github.com/HelloJowet/wikidata_store) to make local Wikidata entities available to the same Lua callbacks that process OSM data:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script examples/places.lua \
  --wikidata-store ./wikidata.store \
  --format geopackage \
  --output places.gpkg
```

Call `osmdb.wikidata(object.tags.wikidata)` to retrieve the Wikidata entity referenced by an OSM tag. For example, a script can prefer its English Wikidata label while retaining the OSM name as a fallback:

```lua
local entity = osmdb.wikidata(object.tags.wikidata)
local english = entity and entity.labels and entity.labels.en
local name = english and english.value or object.tags.name
```

`osmdb.wikidata(id)` accepts one canonical uppercase item, property, or lexeme ID (`Q`, `P`, or `L`). It returns the stored entity as a Lua table, or `nil` if the ID is missing, no entity is found, or no store was configured. Lookups are local; the command does not download Wikidata data or create a store.

See [`examples/places.lua`](examples/places.lua) for a complete example that combines named OSM settlements with Wikidata population, images, and external IDs.

## CLI notes

- Repeat `--script` to run multiple scripts in one database scan. Layer names must be unique across all scripts.
- `--format geopackage` writes one new `.gpkg` file.
- `--format geoparquet` writes one `<layer>.parquet` file per layer to a new directory. Geometry layers include GeoParquet metadata.
- Existing output paths are refused. Output is published only after extraction succeeds.
- `--threads 8` sets the number of parallel Lua workers. The default is the number of available CPU cores.
- `-v` shows progress, while `-vv` also explains geometry failures. The final summary reports processed objects, written rows, and skipped geometries.

The bundled examples are:

- `basic.lua`: café points and major-road lines.
- `cafes.lua`: a single amenity point layer.
- `major_roads.lua`: motorway, trunk, and primary ways.
- `places.lua`: named settlements enriched from a local Wikidata store.
- `route_metadata.lua`: route attributes without relation geometry.

## Lua reference

| Source | Available fields | Geometry methods |
| --- | --- | --- |
| `node` | `id`, `version`, `tags`, `lat`, `lon` | `as_point()` |
| `way` | `id`, `version`, `tags`, `nodes`, `is_closed` | `as_linestring()` |
| `relation` | `id`, `version`, `tags`, `members` | `as_multipoint()`, `as_multilinestring()`, `as_geometrycollection()` |

Relation members have `type`, `id`, and `role`. Relation geometry follows nested relations, detects cycles, and uses EPSG:4326 coordinates.

Supported column types are `string`, `int64`, `double`, `boolean`, `json`, `point`, `linestring`, `multipoint`, `multilinestring`, and `geometrycollection`. Columns are nullable unless `required = true`, and a layer can have at most one geometry column. Layer and column names must be unique ASCII identifiers; `fid` is reserved for GeoPackage. IDs are not added automatically.

If an object's requested geometry is missing, invalid, cyclic, or empty, its rows are skipped and extraction continues. Invalid schemas, values, fields, sources, or geometry types stop extraction with a script error.

## Roadmap

- Polygon and multipolygon assembly, including closed-way polygon interpretation
- CRS reprojection and per-layer CRS selection
- Additional output formats
- Optional and deferred GeoPackage spatial indexes
- Incremental extraction and update workflows
