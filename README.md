# osmdb-extract

`osmdb-extract` turns an [osmdb](https://github.com/HelloJowet/osmdb) database into typed GeoPackage or GeoParquet datasets. Use a small Lua script to choose OpenStreetMap nodes, ways, or relations; keep the attributes you need; and write their geometry.

Way and relation geometry is built while the database is streamed, so focused extracts do not require loading the whole source dataset into memory.

## Quick start

Install the CLI:

```bash
cargo install osmdb-extract
```

You need an osmdb database directory containing `data.rocksdb` and `locations.bin`. See the [osmdb project](https://github.com/HelloJowet/osmdb) for how to create one.

Run the bundled example to extract cafés and major roads into a GeoPackage:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script examples/basic.lua \
  --format geopackage \
  --output region.gpkg \
  -v
```

This creates a `cafes` point layer and a `roads` line layer.

## Using the CLI

- `--format geopackage` writes one new `.gpkg` file.
- `--format geoparquet` writes a directory with one `<layer>.parquet` file per layer. Layers with geometry include GeoParquet metadata; other layers are regular Parquet tables.
- Existing output paths are refused. Extraction writes beside the destination, then renames the result only after success.
- Use `--threads 8` to choose the number of parallel Lua workers. By default, all available CPU cores are used.
- Use `-v` for progress and `-vv` for diagnostics about geometry that could not be created. The final summary includes processed objects, written rows, and skipped geometries.

Adapt a script from `examples/`:

- `basic.lua`: café points and major-road lines.
- `cafes.lua`: a single amenity point layer.
- `major_roads.lua`: motorway, trunk, and primary ways.
- `places.lua`: named settlements enriched from an optional local Wikidata store.
- `route_metadata.lua`: route attributes without relation geometry.

## Write a Lua script

Define layers when the script loads, then add a callback for each OSM object type you use. This example writes one row for every road-like way:

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

Each layer has one source: `node`, `way`, or `relation`. Insert into it only from its matching callback: `osmdb.process_node(object)`, `osmdb.process_way(object)`, or `osmdb.process_relation(object)`.

## Lua reference

| Source | Available fields | Geometry methods |
| --- | --- | --- |
| `node` | `id`, `version`, `tags`, `lat`, `lon` | `as_point()` |
| `way` | `id`, `version`, `tags`, `nodes`, `is_closed` | `as_linestring()` |
| `relation` | `id`, `version`, `tags`, `members` | `as_multipoint()`, `as_multilinestring()`, `as_geometrycollection()` |

Relation members have `type`, `id`, and `role`. Relation geometry follows nested relations, detects cycles, and uses EPSG:4326 coordinates.

Column types are `string`, `int64`, `double`, `boolean`, `json`, `point`, `linestring`, `multipoint`, `multilinestring`, and `geometrycollection`. Columns are nullable unless `required = true`; a layer can have one geometry column. Layer and column names must be unique ASCII identifiers, and `fid` is reserved for GeoPackage. Include an ID column when you need one; it is not added automatically.

If an object's geometry is missing, invalid, cyclic, or empty, its staged rows are skipped and extraction continues. Invalid layer definitions, wrong value types, unknown fields, source mismatches, and geometry-type mismatches stop extraction with a script error.

## Optional: Wikidata lookups

To look up entities locally, create a read-only store with [wikidata_store](https://github.com/HelloJowet/wikidata_store) and pass it with `--wikidata-store`:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script ./with-wikidata.lua \
  --wikidata-store ./wikidata.store \
  --format geopackage \
  --output region.gpkg
```

`osmdb.wikidata(id)` returns the stored entity as a nested Lua table, or `nil` when the ID is absent, the store is not configured, or no entity exists. It looks up one canonical uppercase item, property, or lexeme ID (`Q`, `P`, or `L`) at a time.

Claims are grouped by property ID. Each property contains statements. Read a statement value from `statement.mainsnak.datavalue.value` when its `snaktype` is `"value"`; statements can also have a rank and qualifiers.

`examples/places.lua` reads population (`P1082`), images (`P18`), and external IDs (`P646`, `P2671`) this way:

```lua
local entity = osmdb.wikidata(object.tags.wikidata)
local claims = entity and entity.claims or {}

for _, statement in ipairs(claims.P1082 or {}) do
    if statement.rank ~= "deprecated" then
        local snak = statement.mainsnak
        if snak.snaktype == "value" then
            local population = snak.datavalue.value.amount
            -- P585 qualifiers can be used to select the most recent value.
        end
    end
end
```

For population, the example ignores deprecated statements, chooses the newest value with a `P585` date qualifier, then falls back to the first preferred or normal value. It keeps all valid images and uses the first preferred or normal external ID.

The lookup does not download or create a store, split multi-ID tag values, or follow related entities. Invalid IDs and store read errors stop extraction.

`examples/places.lua` uses these lookups to enrich named settlements. Run it with a local store:

```bash
osmdb-extract \
  --db ./region.osmdb \
  --script examples/places.lua \
  --wikidata-store ./wikidata.store \
  --format geopackage \
  --output places.gpkg
```

## Roadmap

- Polygon and multipolygon assembly, including closed-way polygon interpretation
- CRS reprojection and per-layer CRS selection
- Additional output formats
- Optional and deferred GeoPackage spatial indexes
- Incremental extraction and update workflows
