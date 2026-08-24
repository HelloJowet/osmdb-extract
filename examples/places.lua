-- Named OSM settlements as point features.
local places = osmdb.define_layer({
    name = "places",
    source = "node",
    columns = {
        { name = "osm_id", type = "int64", required = true },
        { name = "name", type = "string", required = true },
        { name = "place", type = "string", required = true },
        { name = "population", type = "int64" },
        { name = "images", type = "json" },
        { name = "freebase_id", type = "string" },
        { name = "google_knowledge_graph_id", type = "string" },
        { name = "geometry", type = "point", required = true },
    },
})

local settlement_places = {
    city = true,
    town = true,
    village = true,
    hamlet = true,
    isolated_dwelling = true,
    locality = true,
}

local function statement_value(statement)
    local snak = statement and statement.mainsnak
    if snak and snak.snaktype == "value" and snak.datavalue then
        return snak.datavalue.value
    end
end

local function first_statement(claims, select)
    for _, rank in ipairs({ "preferred", "normal" }) do
        for _, statement in ipairs(claims or {}) do
            if statement.rank == rank then
                local value = statement_value(statement)
                local selected = select(value)
                if selected ~= nil then return selected end
            end
        end
    end
end

local function quantity_integer(value)
    if type(value) ~= "table" or type(value.amount) ~= "string" then
        return nil
    end
    if not value.amount:match("^[+-]?%d+$") then
        return nil
    end
    local amount = tonumber(value.amount)
    if math.type(amount) ~= "integer" then
        return nil
    end
    return amount
end

local function point_in_time(statement)
    local latest
    for _, qualifier in ipairs((statement.qualifiers or {}).P585 or {}) do
        if qualifier.snaktype == "value" and qualifier.datavalue
            and qualifier.datavalue.type == "time" then
            local time = qualifier.datavalue.value.time
            local sign, year, month, day, hour, minute, second
            if type(time) == "string" then
                sign, year, month, day, hour, minute, second = time:match(
                    "^([+-])(%d+)%-(%d%d)%-(%d%d)T(%d%d):(%d%d):(%d%d)Z$"
                )
            end
            if sign then
                year = tonumber(year)
                if sign == "-" then year = -year end
                local candidate = { year, tonumber(month), tonumber(day), tonumber(hour), tonumber(minute), tonumber(second) }
                if not latest then
                    latest = candidate
                else
                    for index = 1, #candidate do
                        if candidate[index] ~= latest[index] then
                            if candidate[index] > latest[index] then latest = candidate end
                            break
                        end
                    end
                end
            end
        end
    end
    return latest
end

local function is_later(left, right)
    for index = 1, #left do
        if left[index] ~= right[index] then return left[index] > right[index] end
    end
    return false
end

local function population(claims)
    local selected, selected_time, selected_rank
    for _, statement in ipairs(claims or {}) do
        if statement.rank ~= "deprecated" then
            local amount = quantity_integer(statement_value(statement))
            local time = amount and point_in_time(statement)
            if time and (not selected_time or is_later(time, selected_time)
                or (not is_later(selected_time, time) and statement.rank == "preferred" and selected_rank ~= "preferred")) then
                selected, selected_time, selected_rank = amount, time, statement.rank
            end
        end
    end
    if selected_time then return selected end
    return first_statement(claims, quantity_integer)
end

local function images(claims)
    local output = {}
    for _, statement in ipairs(claims or {}) do
        if statement.rank ~= "deprecated" then
            local value = statement_value(statement)
            if type(value) == "string" and value ~= "" then table.insert(output, value) end
        end
    end
    return #output > 0 and output or nil
end

local function external_id(claims)
    return first_statement(claims, function(value)
        if type(value) == "string" and value ~= "" then return value end
    end)
end

function osmdb.process_node(object)
    local tags = object.tags
    local name = tags.name
    local place = tags.place

    if name and name ~= "" and settlement_places[place] then
        local entity = osmdb.wikidata(tags.wikidata)
        local claims = entity and entity.claims or {}
        places:insert({
            osm_id = object.id,
            name = name,
            place = place,
            population = population(claims.P1082),
            images = images(claims.P18),
            freebase_id = external_id(claims.P646),
            google_knowledge_graph_id = external_id(claims.P2671),
            geometry = object:as_point(),
        })
    end
end
