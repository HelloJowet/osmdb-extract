-- Route records without recursively resolving their member geometries.
-- This produces an attribute table, which is much smaller and faster than
-- a geometry collection for every route relation.
local routes = osmdb.define_layer({
    name = "routes",
    source = "relation",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "route", type = "string", required = true },
        { name = "name", type = "string" },
        { name = "ref", type = "string" },
        { name = "network", type = "string" },
        { name = "operator", type = "string" },
    },
})

function osmdb.process_relation(object)
    if object.tags.type == "route" and object.tags.route then
        routes:insert({
            osm_id = object.id,
            route = object.tags.route,
            name = object.tags.name,
            ref = object.tags.ref,
            network = object.tags.network,
            operator = object.tags.operator,
        })
    end
end
