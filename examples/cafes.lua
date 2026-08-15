-- A small point layer for a specific amenity.
local cafes = osmdb.define_layer({
    name = "cafes",
    source = "node",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "name", type = "string" },
        { name = "geometry", type = "point", required = true },
    },
})

function osmdb.process_node(object)
    if object.tags.amenity == "cafe" then
        cafes:insert({
            osm_id = object.id,
            name = object.tags.name,
            geometry = object:as_point(),
        })
    end
end
