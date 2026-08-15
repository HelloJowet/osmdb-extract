local cafes = osmdb.define_layer({
    name = "cafes",
    source = "node",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "name", type = "string" },
        { name = "tags", type = "json" },
        { name = "geometry", type = "point", required = true },
    },
})

local roads = osmdb.define_layer({
    name = "roads",
    source = "way",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "class", type = "string", required = true },
        { name = "geometry", type = "linestring", required = true },
    },
})

local major_road_classes = {
    motorway = true,
    trunk = true,
    primary = true,
}

function osmdb.process_node(object)
    if object.tags.amenity == "cafe" then
        cafes:insert({
            osm_id = object.id,
            name = object.tags.name,
            tags = object.tags,
            geometry = object:as_point(),
        })
    end
end

function osmdb.process_way(object)
    local class = object.tags.highway
    if major_road_classes[class] then
        roads:insert({
            osm_id = object.id,
            class = class,
            geometry = object:as_linestring(),
        })
    end
end
