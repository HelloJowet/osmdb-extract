use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use geo_types::Geometry;
use mlua::{Function, Lua, Table, UserData, UserDataFields, UserDataMethods, Value as LuaValue};
use osmdb::{MemberType, Node, Relation, Way};

use crate::geometry::{GeometryResolver, RelationGeometryKind};
use crate::schema::{
    ColumnDef, ColumnType, LayerDef, OutputRow, OutputValue, SourceKind, validate_schema,
};

#[derive(Default)]
struct WorkerState {
    current_source: Option<SourceKind>,
    current_id: i64,
    rows: Vec<OutputRow>,
    geometry_failure: Option<String>,
    fatal_error: Option<String>,
}

#[derive(Clone)]
struct GeometryValue(Geometry<f64>);

impl UserData for GeometryValue {}

#[derive(Clone)]
struct LayerHandle {
    index: usize,
    definition: LayerDef,
    state: Arc<Mutex<WorkerState>>,
}

impl UserData for LayerHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("insert", |lua, this, values: Table| {
            match this.parse_row(lua, &values) {
                Ok(row) => {
                    this.state.lock().unwrap().rows.push(row);
                    Ok(())
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    this.state.lock().unwrap().fatal_error = Some(message.clone());
                    Err(mlua::Error::RuntimeError(message))
                }
            }
        });
    }
}

impl LayerHandle {
    fn parse_row(&self, lua: &Lua, values: &Table) -> Result<OutputRow> {
        let state = self.state.lock().unwrap();
        if state.current_source != Some(self.definition.source) {
            bail!(
                "layer '{}' accepts {} objects but insert was called while processing {} {}",
                self.definition.name,
                self.definition.source.as_str(),
                state.current_source.map(SourceKind::as_str).unwrap_or("no"),
                state.current_id
            );
        }
        let object_id = state.current_id;
        drop(state);

        let known: HashSet<&str> = self
            .definition
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        for pair in values.clone().pairs::<LuaValue, LuaValue>() {
            let (key, _) = pair.context("failed to inspect inserted row")?;
            let LuaValue::String(key) = key else {
                bail!(
                    "layer '{}' row keys must be strings (object {object_id})",
                    self.definition.name
                );
            };
            let key = key.to_str()?;
            if !known.contains(key.as_ref()) {
                bail!(
                    "unknown column '{}' for layer '{}' (object {object_id})",
                    key,
                    self.definition.name
                );
            }
        }

        let mut output = Vec::with_capacity(self.definition.columns.len());
        for column in &self.definition.columns {
            let value: LuaValue = values.get(column.name.as_str())?;
            if matches!(value, LuaValue::Nil) {
                if column.required {
                    bail!(
                        "required column '{}.{}' is missing for {} {object_id}",
                        self.definition.name,
                        column.name,
                        self.definition.source.as_str()
                    );
                }
                output.push(OutputValue::Null);
                continue;
            }
            output.push(convert_value(lua, value, column).with_context(|| {
                format!(
                    "invalid value for column '{}.{}' while processing {} {object_id}",
                    self.definition.name,
                    column.name,
                    self.definition.source.as_str()
                )
            })?);
        }
        Ok(OutputRow {
            layer_index: self.index,
            values: output,
        })
    }
}

fn convert_value(_lua: &Lua, value: LuaValue, column: &ColumnDef) -> Result<OutputValue> {
    match column.column_type {
        ColumnType::String => match value {
            LuaValue::String(value) => Ok(OutputValue::String(value.to_str()?.to_owned())),
            _ => bail!("expected string"),
        },
        ColumnType::Int64 => match value {
            LuaValue::Integer(value) => Ok(OutputValue::Int64(value)),
            LuaValue::Number(value)
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i64::MIN as f64
                    && value <= i64::MAX as f64 =>
            {
                Ok(OutputValue::Int64(value as i64))
            }
            _ => bail!("expected int64"),
        },
        ColumnType::Double => match value {
            LuaValue::Integer(value) => Ok(OutputValue::Double(value as f64)),
            LuaValue::Number(value) if value.is_finite() => Ok(OutputValue::Double(value)),
            _ => bail!("expected finite number"),
        },
        ColumnType::Boolean => match value {
            LuaValue::Boolean(value) => Ok(OutputValue::Boolean(value)),
            _ => bail!("expected boolean"),
        },
        ColumnType::Json => Ok(OutputValue::Json(serde_json::to_string(&lua_to_json(
            value, 0,
        )?)?)),
        expected if expected.is_geometry() => {
            let LuaValue::UserData(value) = value else {
                bail!("expected {} geometry", expected.as_str());
            };
            let geometry = value
                .borrow::<GeometryValue>()
                .map_err(|_| anyhow!("expected {} geometry", expected.as_str()))?;
            if !expected.accepts(&geometry.0) {
                bail!(
                    "expected {} geometry, got {}",
                    expected.as_str(),
                    geometry_name(&geometry.0)
                );
            }
            Ok(OutputValue::Geometry(geometry.0.clone()))
        }
        _ => unreachable!(),
    }
}

fn lua_to_json(value: LuaValue, depth: usize) -> Result<serde_json::Value> {
    if depth > 64 {
        bail!("JSON value exceeds maximum nesting depth of 64");
    }
    match value {
        LuaValue::Nil => Ok(serde_json::Value::Null),
        LuaValue::Boolean(value) => Ok(value.into()),
        LuaValue::Integer(value) => Ok(value.into()),
        LuaValue::Number(value) if value.is_finite() => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| anyhow!("JSON number must be finite")),
        LuaValue::String(value) => Ok(value.to_str()?.to_owned().into()),
        LuaValue::Table(table) => {
            let mut entries = Vec::new();
            let mut integer_keys = true;
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, value) = pair?;
                integer_keys &= matches!(key, LuaValue::Integer(_));
                entries.push((key, value));
            }
            if !entries.is_empty() && integer_keys {
                entries.sort_by_key(|(key, _)| match key {
                    LuaValue::Integer(value) => *value,
                    _ => 0,
                });
                for (index, (key, _)) in entries.iter().enumerate() {
                    if !matches!(key, LuaValue::Integer(value) if *value == index as i64 + 1) {
                        bail!("JSON array keys must be contiguous and start at 1");
                    }
                }
                entries
                    .into_iter()
                    .map(|(_, value)| lua_to_json(value, depth + 1))
                    .collect::<Result<Vec<_>>>()
                    .map(serde_json::Value::Array)
            } else {
                let mut object = serde_json::Map::new();
                for (key, value) in entries {
                    let LuaValue::String(key) = key else {
                        bail!("JSON object keys must be strings");
                    };
                    object.insert(key.to_str()?.to_owned(), lua_to_json(value, depth + 1)?);
                }
                Ok(serde_json::Value::Object(object))
            }
        }
        _ => bail!("unsupported Lua value in JSON column"),
    }
}

fn geometry_name(geometry: &Geometry<f64>) -> &'static str {
    match geometry {
        Geometry::Point(_) => "point",
        Geometry::LineString(_) => "linestring",
        Geometry::MultiPoint(_) => "multipoint",
        Geometry::MultiLineString(_) => "multilinestring",
        Geometry::GeometryCollection(_) => "geometrycollection",
        _ => "unsupported",
    }
}

#[derive(Clone)]
struct NodeObject {
    id: i64,
    node: Node,
}

impl UserData for NodeObject {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.id));
        fields.add_field_method_get("version", |_, this| Ok(this.node.version));
        fields.add_field_method_get("lat", |_, this| Ok(this.node.latitude_degrees()));
        fields.add_field_method_get("lon", |_, this| Ok(this.node.longitude_degrees()));
        fields.add_field_method_get("tags", |lua, this| tags_table(lua, &this.node.tags));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("as_point", |_, this, ()| {
            Ok(GeometryValue(Geometry::Point(geo_types::Point::new(
                this.node.longitude_degrees(),
                this.node.latitude_degrees(),
            ))))
        });
    }
}

#[derive(Clone)]
struct WayObject {
    id: i64,
    way: Way,
    resolver: Arc<GeometryResolver>,
    state: Arc<Mutex<WorkerState>>,
}

impl UserData for WayObject {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.id));
        fields.add_field_method_get("version", |_, this| Ok(this.way.version));
        fields.add_field_method_get("is_closed", |_, this| {
            Ok(this.way.node_ids.len() >= 2
                && this.way.node_ids.first() == this.way.node_ids.last())
        });
        fields.add_field_method_get("tags", |lua, this| tags_table(lua, &this.way.tags));
        fields.add_field_method_get("nodes", |lua, this| sequence_table(lua, &this.way.node_ids));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("as_linestring", |_, this, ()| {
            this.geometry_result(
                this.resolver
                    .way_geometry(this.id, &this.way)
                    .map(Geometry::LineString),
            )
        });
    }
}

impl WayObject {
    fn geometry_result(&self, result: Result<Geometry<f64>>) -> mlua::Result<GeometryValue> {
        match result {
            Ok(geometry) => Ok(GeometryValue(geometry)),
            Err(error) => {
                let message = error.to_string();
                self.state.lock().unwrap().geometry_failure = Some(message.clone());
                Err(mlua::Error::RuntimeError(message))
            }
        }
    }
}

#[derive(Clone)]
struct RelationObject {
    id: i64,
    relation: Relation,
    resolver: Arc<GeometryResolver>,
    state: Arc<Mutex<WorkerState>>,
}

impl UserData for RelationObject {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, this| Ok(this.id));
        fields.add_field_method_get("version", |_, this| Ok(this.relation.version));
        fields.add_field_method_get("tags", |lua, this| tags_table(lua, &this.relation.tags));
        fields.add_field_method_get("members", |lua, this| {
            let output = lua.create_table_with_capacity(this.relation.members.len(), 0)?;
            for (index, member) in this.relation.members.iter().enumerate() {
                let value = lua.create_table()?;
                value.set(
                    "type",
                    match member.member_type {
                        MemberType::Node => "node",
                        MemberType::Way => "way",
                        MemberType::Relation => "relation",
                    },
                )?;
                value.set("id", member.id)?;
                value.set("role", member.role.as_str())?;
                output.raw_set(index + 1, value)?;
            }
            Ok(output)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("as_multipoint", |_, this, ()| {
            this.resolve(RelationGeometryKind::MultiPoint)
        });
        methods.add_method("as_multilinestring", |_, this, ()| {
            this.resolve(RelationGeometryKind::MultiLineString)
        });
        methods.add_method("as_geometrycollection", |_, this, ()| {
            this.resolve(RelationGeometryKind::GeometryCollection)
        });
    }
}

impl RelationObject {
    fn resolve(&self, kind: RelationGeometryKind) -> mlua::Result<GeometryValue> {
        match self
            .resolver
            .relation_geometry(self.id, &self.relation, kind)
        {
            Ok(geometry) => Ok(GeometryValue(geometry)),
            Err(error) => {
                let message = error.to_string();
                self.state.lock().unwrap().geometry_failure = Some(message.clone());
                Err(mlua::Error::RuntimeError(message))
            }
        }
    }
}

fn tags_table(lua: &Lua, tags: &HashMap<String, String>) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, tags.len())?;
    for (key, value) in tags {
        table.set(key.as_str(), value.as_str())?;
    }
    Ok(table)
}

fn sequence_table(lua: &Lua, values: &[i64]) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(values.len(), 0)?;
    for (index, value) in values.iter().enumerate() {
        table.raw_set(index + 1, *value)?;
    }
    Ok(table)
}

pub enum ObjectOutcome {
    Rows(Vec<OutputRow>),
    GeometrySkipped(String),
}

pub struct LuaWorker {
    lua: Lua,
    state: Arc<Mutex<WorkerState>>,
    resolver: Arc<GeometryResolver>,
    layers: Vec<LayerDef>,
}

impl LuaWorker {
    pub fn new(script: &[u8], resolver: Arc<GeometryResolver>) -> Result<Self> {
        let lua = Lua::new();
        let state = Arc::new(Mutex::new(WorkerState::default()));
        let registry = Arc::new(Mutex::new(Vec::<LayerDef>::new()));
        let api = lua.create_table()?;
        let define_registry = Arc::clone(&registry);
        let define_state = Arc::clone(&state);
        api.set(
            "define_layer",
            lua.create_function(move |_, definition: Table| {
                let layer = parse_layer(&definition).map_err(mlua::Error::external)?;
                layer.validate().map_err(mlua::Error::external)?;
                let mut layers = define_registry.lock().unwrap();
                if layers.iter().any(|existing| existing.name == layer.name) {
                    return Err(mlua::Error::external(anyhow!(
                        "duplicate layer name '{}'",
                        layer.name
                    )));
                }
                let index = layers.len();
                layers.push(layer.clone());
                Ok(LayerHandle {
                    index,
                    definition: layer,
                    state: Arc::clone(&define_state),
                })
            })?,
        )?;
        lua.globals().set("osmdb", api)?;
        lua.load(script)
            .set_name("extract script")
            .exec()
            .context("failed to load Lua extraction script")?;
        let layers = registry.lock().unwrap().clone();
        validate_schema(&layers)?;
        Ok(Self {
            lua,
            state,
            resolver,
            layers,
        })
    }

    pub fn layers(&self) -> &[LayerDef] {
        &self.layers
    }

    pub fn process_node(&self, id: i64, node: Node) -> Result<ObjectOutcome> {
        self.run(SourceKind::Node, id, NodeObject { id, node })
    }

    pub fn process_way(&self, id: i64, way: Way) -> Result<ObjectOutcome> {
        self.run(
            SourceKind::Way,
            id,
            WayObject {
                id,
                way,
                resolver: Arc::clone(&self.resolver),
                state: Arc::clone(&self.state),
            },
        )
    }

    pub fn process_relation(&self, id: i64, relation: Relation) -> Result<ObjectOutcome> {
        self.run(
            SourceKind::Relation,
            id,
            RelationObject {
                id,
                relation,
                resolver: Arc::clone(&self.resolver),
                state: Arc::clone(&self.state),
            },
        )
    }

    fn run<T>(&self, source: SourceKind, id: i64, object: T) -> Result<ObjectOutcome>
    where
        T: UserData + Send + 'static,
    {
        {
            let mut state = self.state.lock().unwrap();
            state.current_source = Some(source);
            state.current_id = id;
            state.rows.clear();
            state.geometry_failure = None;
            state.fatal_error = None;
        }
        let api: Table = self.lua.globals().get("osmdb")?;
        let callback_name = format!("process_{}", source.as_str());
        let callback: Option<Function> = api.get(callback_name.as_str())?;
        let callback_result = match callback {
            Some(callback) => callback.call::<()>(object),
            None => Ok(()),
        };

        let mut state = self.state.lock().unwrap();
        state.current_source = None;
        if let Some(error) = state.fatal_error.take() {
            state.rows.clear();
            bail!("{error}");
        }
        if let Some(error) = state.geometry_failure.take() {
            state.rows.clear();
            return Ok(ObjectOutcome::GeometrySkipped(error));
        }
        callback_result
            .with_context(|| format!("Lua {callback_name} failed for {} {id}", source.as_str()))?;
        Ok(ObjectOutcome::Rows(std::mem::take(&mut state.rows)))
    }
}

fn parse_layer(definition: &Table) -> Result<LayerDef> {
    let name: String = definition
        .get("name")
        .context("layer is missing string field 'name'")?;
    let source: String = definition
        .get("source")
        .context("layer is missing string field 'source'")?;
    let columns: Table = definition
        .get("columns")
        .context("layer is missing table field 'columns'")?;
    let mut parsed_columns = Vec::new();
    for column in columns.sequence_values::<Table>() {
        let column = column.context("invalid entry in layer columns")?;
        let column_name: String = column
            .get("name")
            .context("column is missing string field 'name'")?;
        let column_type: String = column
            .get("type")
            .context("column is missing string field 'type'")?;
        let required = column.get::<Option<bool>>("required")?.unwrap_or(false);
        parsed_columns.push(ColumnDef {
            name: column_name,
            column_type: ColumnType::parse(&column_type)?,
            required,
        });
    }
    Ok(LayerDef {
        name,
        source: SourceKind::parse(&source)?,
        columns: parsed_columns,
    })
}
