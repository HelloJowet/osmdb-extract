-- Keep just the road classes commonly used for regional-scale maps.
local major_roads = osmdb.define_layer({
    name = "major_roads",
    source = "way",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "class", type = "string", required = true },
        { name = "name", type = "string" },
        { name = "ref", type = "string" },
        { name = "geometry", type = "linestring", required = true },
    },
})

local major_classes = {
    motorway = true,
    trunk = true,
    primary = true,
}

function osmdb.process_way(object)
    local class = object.tags.highway
    if major_classes[class] then
        major_roads:insert({
            osm_id = object.id,
            class = class,
            name = object.tags.name,
            ref = object.tags.ref,
            geometry = object:as_linestring(),
        })
    end
end
