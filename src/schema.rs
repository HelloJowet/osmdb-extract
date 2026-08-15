use std::collections::HashSet;

use anyhow::{Result, bail};
use geo_types::Geometry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Node,
    Way,
    Relation,
}

impl SourceKind {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "node" => Ok(Self::Node),
            "way" => Ok(Self::Way),
            "relation" => Ok(Self::Relation),
            _ => bail!("unknown layer source '{value}' (expected node, way, or relation)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Way => "way",
            Self::Relation => "relation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    String,
    Int64,
    Double,
    Boolean,
    Json,
    Point,
    LineString,
    MultiPoint,
    MultiLineString,
    GeometryCollection,
}

impl ColumnType {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "string" => Ok(Self::String),
            "int64" => Ok(Self::Int64),
            "double" => Ok(Self::Double),
            "boolean" => Ok(Self::Boolean),
            "json" => Ok(Self::Json),
            "point" => Ok(Self::Point),
            "linestring" => Ok(Self::LineString),
            "multipoint" => Ok(Self::MultiPoint),
            "multilinestring" => Ok(Self::MultiLineString),
            "geometrycollection" => Ok(Self::GeometryCollection),
            _ => bail!("unknown column type '{value}'"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Int64 => "int64",
            Self::Double => "double",
            Self::Boolean => "boolean",
            Self::Json => "json",
            Self::Point => "point",
            Self::LineString => "linestring",
            Self::MultiPoint => "multipoint",
            Self::MultiLineString => "multilinestring",
            Self::GeometryCollection => "geometrycollection",
        }
    }

    pub fn is_geometry(self) -> bool {
        matches!(
            self,
            Self::Point
                | Self::LineString
                | Self::MultiPoint
                | Self::MultiLineString
                | Self::GeometryCollection
        )
    }

    pub fn accepts(self, geometry: &Geometry<f64>) -> bool {
        matches!(
            (self, geometry),
            (Self::Point, Geometry::Point(_))
                | (Self::LineString, Geometry::LineString(_))
                | (Self::MultiPoint, Geometry::MultiPoint(_))
                | (Self::MultiLineString, Geometry::MultiLineString(_))
                | (Self::GeometryCollection, Geometry::GeometryCollection(_))
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    pub column_type: ColumnType,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDef {
    pub name: String,
    pub source: SourceKind,
    pub columns: Vec<ColumnDef>,
}

impl LayerDef {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("layer", &self.name)?;
        if self.columns.is_empty() {
            bail!("layer '{}' must define at least one column", self.name);
        }

        let mut names = HashSet::new();
        let mut geometry_count = 0;
        for column in &self.columns {
            validate_identifier("column", &column.name)?;
            if !names.insert(column.name.as_str()) {
                bail!(
                    "layer '{}' has duplicate column '{}'",
                    self.name,
                    column.name
                );
            }
            if column.column_type.is_geometry() {
                geometry_count += 1;
                let valid = match self.source {
                    SourceKind::Node => column.column_type == ColumnType::Point,
                    SourceKind::Way => column.column_type == ColumnType::LineString,
                    SourceKind::Relation => matches!(
                        column.column_type,
                        ColumnType::MultiPoint
                            | ColumnType::MultiLineString
                            | ColumnType::GeometryCollection
                    ),
                };
                if !valid {
                    bail!(
                        "geometry type '{}' is not valid for {} layer '{}'",
                        column.column_type.as_str(),
                        self.source.as_str(),
                        self.name
                    );
                }
            }
        }
        if geometry_count > 1 {
            bail!(
                "layer '{}' may define at most one geometry column",
                self.name
            );
        }
        Ok(())
    }

    pub fn geometry_column(&self) -> Option<(usize, &ColumnDef)> {
        self.columns
            .iter()
            .enumerate()
            .find(|(_, column)| column.column_type.is_geometry())
    }
}

pub fn validate_schema(layers: &[LayerDef]) -> Result<()> {
    if layers.is_empty() {
        bail!("Lua script did not define any output layers");
    }
    let mut names = HashSet::new();
    for layer in layers {
        layer.validate()?;
        if !names.insert(layer.name.as_str()) {
            bail!("duplicate layer name '{}'", layer.name);
        }
    }
    Ok(())
}

fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    let mut chars = value.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow::anyhow!("{kind} name must not be empty"))?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!(
            "invalid {kind} name '{value}'; use ASCII letters, digits, and underscores and do not start with a digit"
        );
    }
    if value == "fid" {
        bail!("{kind} name 'fid' is reserved by the GeoPackage backend");
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum OutputValue {
    Null,
    String(String),
    Int64(i64),
    Double(f64),
    Boolean(bool),
    Json(String),
    Geometry(Geometry<f64>),
}

#[derive(Debug, Clone)]
pub struct OutputRow {
    pub layer_index: usize,
    pub values: Vec<OutputValue>,
}
