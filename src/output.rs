use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaBuilder, SchemaRef};
use geo_types::Geometry;
use geoarrow_schema::crs::Crs;
use geoarrow_schema::{GeoArrowType, Metadata, WkbType};
use geoparquet::writer::{GeoParquetRecordBatchEncoder, GeoParquetWriterOptionsBuilder};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};

use crate::pipeline::OutputFormat;
use crate::schema::{ColumnType, LayerDef, OutputRow, OutputValue};

const BATCH_SIZE: usize = 2048;

pub struct OutputWriter {
    backend: Backend,
    buffers: Vec<Vec<OutputRow>>,
}

enum Backend {
    GeoPackage(GeoPackageWriter),
    GeoParquet(GeoParquetWriter),
}

impl OutputWriter {
    pub fn create(format: OutputFormat, path: &Path, layers: &[LayerDef]) -> Result<Self> {
        let backend = match format {
            OutputFormat::Geopackage => {
                Backend::GeoPackage(GeoPackageWriter::create(path, layers)?)
            }
            OutputFormat::Geoparquet => {
                Backend::GeoParquet(GeoParquetWriter::create(path, layers)?)
            }
        };
        Ok(Self {
            backend,
            buffers: vec![Vec::new(); layers.len()],
        })
    }

    pub fn push_rows(&mut self, rows: Vec<OutputRow>) -> Result<()> {
        for row in rows {
            let index = row.layer_index;
            let buffer = self
                .buffers
                .get_mut(index)
                .ok_or_else(|| anyhow!("row targets unknown layer index {index}"))?;
            buffer.push(row);
            if buffer.len() >= BATCH_SIZE {
                let rows = std::mem::take(buffer);
                self.backend.write(index, &rows)?;
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            if !buffer.is_empty() {
                let rows = std::mem::take(buffer);
                self.backend.write(index, &rows)?;
            }
        }
        self.backend.finish()
    }
}

impl Backend {
    fn write(&mut self, layer: usize, rows: &[OutputRow]) -> Result<()> {
        match self {
            Self::GeoPackage(writer) => writer.write(layer, rows),
            Self::GeoParquet(writer) => writer.write(layer, rows),
        }
    }

    fn finish(self) -> Result<()> {
        match self {
            Self::GeoPackage(writer) => writer.finish(),
            Self::GeoParquet(writer) => writer.finish(),
        }
    }
}

struct GeoParquetWriter {
    layers: Vec<ParquetLayer>,
}

struct ParquetLayer {
    definition: LayerDef,
    input_schema: SchemaRef,
    encoder: Option<GeoParquetRecordBatchEncoder>,
    writer: ArrowWriter<File>,
}

impl GeoParquetWriter {
    fn create(path: &Path, layers: &[LayerDef]) -> Result<Self> {
        fs::create_dir(path).with_context(|| {
            format!("failed to create GeoParquet directory '{}'", path.display())
        })?;
        let mut output_layers = Vec::with_capacity(layers.len());
        for layer in layers {
            let input_schema = Arc::new(arrow_schema(layer)?);
            let file_path = path.join(format!("{}.parquet", layer.name));
            let file = File::create(&file_path)
                .with_context(|| format!("failed to create '{}'", file_path.display()))?;
            let properties = WriterProperties::builder()
                .set_compression(Compression::ZSTD(ZstdLevel::default()))
                .build();
            let encoder = if let Some((_, column)) = layer.geometry_column() {
                let options = GeoParquetWriterOptionsBuilder::default()
                    .set_primary_column(column.name.clone())
                    .build();
                Some(GeoParquetRecordBatchEncoder::try_new(
                    &input_schema,
                    &options,
                )?)
            } else {
                None
            };
            let target_schema = encoder
                .as_ref()
                .map(GeoParquetRecordBatchEncoder::target_schema)
                .unwrap_or_else(|| Arc::clone(&input_schema));
            let writer = ArrowWriter::try_new(file, target_schema, Some(properties))?;
            output_layers.push(ParquetLayer {
                definition: layer.clone(),
                input_schema,
                encoder,
                writer,
            });
        }
        Ok(Self {
            layers: output_layers,
        })
    }

    fn write(&mut self, layer_index: usize, rows: &[OutputRow]) -> Result<()> {
        let layer = &mut self.layers[layer_index];
        let batch = rows_to_batch(&layer.definition, Arc::clone(&layer.input_schema), rows)?;
        if let Some(encoder) = &mut layer.encoder {
            let encoded = encoder.encode_record_batch(&batch)?;
            layer.writer.write(&encoded)?;
        } else {
            layer.writer.write(&batch)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        for layer in self.layers {
            let mut writer = layer.writer;
            if let Some(encoder) = layer.encoder {
                writer.append_key_value_metadata(encoder.into_keyvalue()?);
            }
            writer.finish()?;
        }
        Ok(())
    }
}

fn arrow_schema(layer: &LayerDef) -> Result<Schema> {
    let mut builder = SchemaBuilder::new();
    let crs = Crs::from_authority_code("EPSG:4326".to_string());
    let metadata = Arc::new(Metadata::new(crs, None));
    for column in &layer.columns {
        let nullable = !column.required;
        let field = match column.column_type {
            ColumnType::String | ColumnType::Json => {
                Field::new(&column.name, DataType::Utf8, nullable)
            }
            ColumnType::Int64 => Field::new(&column.name, DataType::Int64, nullable),
            ColumnType::Double => Field::new(&column.name, DataType::Float64, nullable),
            ColumnType::Boolean => Field::new(&column.name, DataType::Boolean, nullable),
            kind if kind.is_geometry() => GeoArrowType::Wkb(WkbType::new(Arc::clone(&metadata)))
                .to_field(&column.name, nullable),
            _ => unreachable!(),
        };
        builder.push(field);
    }
    Ok(builder.finish())
}

fn rows_to_batch(layer: &LayerDef, schema: SchemaRef, rows: &[OutputRow]) -> Result<RecordBatch> {
    let mut arrays = Vec::<ArrayRef>::with_capacity(layer.columns.len());
    for (column_index, column) in layer.columns.iter().enumerate() {
        let array: ArrayRef = match column.column_type {
            ColumnType::String => {
                Arc::new(StringArray::from_iter(rows.iter().map(
                    |row| match &row.values[column_index] {
                        OutputValue::String(value) => Some(value.as_str()),
                        OutputValue::Null => None,
                        _ => unreachable!("validated output value"),
                    },
                )))
            }
            ColumnType::Json => {
                Arc::new(StringArray::from_iter(rows.iter().map(
                    |row| match &row.values[column_index] {
                        OutputValue::Json(value) => Some(value.as_str()),
                        OutputValue::Null => None,
                        _ => unreachable!("validated output value"),
                    },
                )))
            }
            ColumnType::Int64 => {
                Arc::new(Int64Array::from_iter(rows.iter().map(
                    |row| match row.values[column_index] {
                        OutputValue::Int64(value) => Some(value),
                        OutputValue::Null => None,
                        _ => unreachable!("validated output value"),
                    },
                )))
            }
            ColumnType::Double => {
                Arc::new(Float64Array::from_iter(rows.iter().map(
                    |row| match row.values[column_index] {
                        OutputValue::Double(value) => Some(value),
                        OutputValue::Null => None,
                        _ => unreachable!("validated output value"),
                    },
                )))
            }
            ColumnType::Boolean => {
                Arc::new(BooleanArray::from_iter(rows.iter().map(
                    |row| match row.values[column_index] {
                        OutputValue::Boolean(value) => Some(value),
                        OutputValue::Null => None,
                        _ => unreachable!("validated output value"),
                    },
                )))
            }
            kind if kind.is_geometry() => {
                let mut encoded = Vec::with_capacity(rows.len());
                for row in rows {
                    match &row.values[column_index] {
                        OutputValue::Geometry(geometry) => encoded.push(Some(to_wkb(geometry)?)),
                        OutputValue::Null => encoded.push(None),
                        _ => unreachable!("validated output value"),
                    }
                }
                Arc::new(BinaryArray::from_iter(
                    encoded.iter().map(|value| value.as_deref()),
                ))
            }
            _ => unreachable!(),
        };
        arrays.push(array);
    }
    Ok(RecordBatch::try_new(schema, arrays)?)
}

struct GeoPackageWriter {
    connection: Connection,
    layers: Vec<LayerDef>,
    extents: Vec<Option<Extent>>,
}

#[derive(Clone, Copy)]
struct Extent {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Extent {
    fn include(&mut self, other: Self) {
        self.min_x = self.min_x.min(other.min_x);
        self.min_y = self.min_y.min(other.min_y);
        self.max_x = self.max_x.max(other.max_x);
        self.max_y = self.max_y.max(other.max_y);
    }
}

impl GeoPackageWriter {
    fn create(path: &Path, layers: &[LayerDef]) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("failed to create GeoPackage '{}'", path.display()))?;
        initialize_geopackage(&connection)?;
        for layer in layers {
            create_gpkg_layer(&connection, layer)?;
        }
        Ok(Self {
            connection,
            layers: layers.to_vec(),
            extents: vec![None; layers.len()],
        })
    }

    fn write(&mut self, layer_index: usize, rows: &[OutputRow]) -> Result<()> {
        let layer = &self.layers[layer_index];
        let column_names = layer
            .columns
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=layer.columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {} ({column_names}) VALUES ({placeholders})",
            quote_identifier(&layer.name)
        );
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare(&sql)?;
            for row in rows {
                let mut values = Vec::with_capacity(row.values.len());
                for value in &row.values {
                    values.push(match value {
                        OutputValue::Null => SqlValue::Null,
                        OutputValue::String(value) | OutputValue::Json(value) => {
                            SqlValue::Text(value.clone())
                        }
                        OutputValue::Int64(value) => SqlValue::Integer(*value),
                        OutputValue::Double(value) => SqlValue::Real(*value),
                        OutputValue::Boolean(value) => SqlValue::Integer(i64::from(*value)),
                        OutputValue::Geometry(geometry) => {
                            let extent = geometry_extent(geometry).ok_or_else(|| {
                                anyhow!("empty geometry for layer '{}'", layer.name)
                            })?;
                            match &mut self.extents[layer_index] {
                                Some(current) => current.include(extent),
                                slot @ None => *slot = Some(extent),
                            }
                            SqlValue::Blob(to_gpkg_geometry(geometry)?)
                        }
                    });
                }
                statement.execute(params_from_iter(values))?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn finish(self) -> Result<()> {
        for (layer, extent) in self.layers.iter().zip(self.extents) {
            if let Some(extent) = extent {
                self.connection.execute(
                    "UPDATE gpkg_contents SET min_x=?1, min_y=?2, max_x=?3, max_y=?4, last_change=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE table_name=?5",
                    rusqlite::params![extent.min_x, extent.min_y, extent.max_x, extent.max_y, layer.name],
                )?;
            }
        }
        self.connection.execute_batch("PRAGMA optimize;")?;
        Ok(())
    }
}

fn initialize_geopackage(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        PRAGMA application_id = 1196444487;
        PRAGMA user_version = 10400;
        PRAGMA foreign_keys = ON;
        CREATE TABLE gpkg_spatial_ref_sys (
            srs_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL PRIMARY KEY,
            organization TEXT NOT NULL,
            organization_coordsys_id INTEGER NOT NULL,
            definition TEXT NOT NULL,
            description TEXT
        );
        CREATE TABLE gpkg_contents (
            table_name TEXT NOT NULL PRIMARY KEY,
            data_type TEXT NOT NULL,
            identifier TEXT UNIQUE,
            description TEXT DEFAULT '',
            last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE,
            srs_id INTEGER,
            FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
        );
        CREATE TABLE gpkg_geometry_columns (
            table_name TEXT NOT NULL,
            column_name TEXT NOT NULL,
            geometry_type_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL,
            z TINYINT NOT NULL,
            m TINYINT NOT NULL,
            PRIMARY KEY (table_name, column_name),
            FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id),
            FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name)
        );
        INSERT INTO gpkg_spatial_ref_sys VALUES
            ('Undefined Cartesian SRS', -1, 'NONE', -1, 'undefined', 'undefined Cartesian coordinate reference system'),
            ('Undefined geographic SRS', 0, 'NONE', 0, 'undefined', 'undefined geographic coordinate reference system'),
            ('WGS 84 geodetic', 4326, 'EPSG', 4326,
             'GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]',
             'longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid');
        "#,
    )?;
    Ok(())
}

fn create_gpkg_layer(connection: &Connection, layer: &LayerDef) -> Result<()> {
    let mut definitions = vec!["fid INTEGER PRIMARY KEY AUTOINCREMENT".to_string()];
    for column in &layer.columns {
        let sql_type = match column.column_type {
            ColumnType::String | ColumnType::Json => "TEXT",
            ColumnType::Int64 | ColumnType::Boolean => "INTEGER",
            ColumnType::Double => "REAL",
            kind if kind.is_geometry() => "BLOB",
            _ => unreachable!(),
        };
        definitions.push(format!(
            "{} {sql_type}{}",
            quote_identifier(&column.name),
            if column.required { " NOT NULL" } else { "" }
        ));
    }
    connection.execute_batch(&format!(
        "CREATE TABLE {} ({})",
        quote_identifier(&layer.name),
        definitions.join(", ")
    ))?;

    if let Some((_, geometry)) = layer.geometry_column() {
        connection.execute(
            "INSERT INTO gpkg_contents (table_name, data_type, identifier, srs_id) VALUES (?1, 'features', ?1, 4326)",
            [&layer.name],
        )?;
        connection.execute(
            "INSERT INTO gpkg_geometry_columns (table_name, column_name, geometry_type_name, srs_id, z, m) VALUES (?1, ?2, ?3, 4326, 0, 0)",
            rusqlite::params![layer.name, geometry.name, gpkg_geometry_type(geometry.column_type)],
        )?;
    } else {
        connection.execute(
            "INSERT INTO gpkg_contents (table_name, data_type, identifier, srs_id) VALUES (?1, 'attributes', ?1, NULL)",
            [&layer.name],
        )?;
    }
    Ok(())
}

fn gpkg_geometry_type(kind: ColumnType) -> &'static str {
    match kind {
        ColumnType::Point => "POINT",
        ColumnType::LineString => "LINESTRING",
        ColumnType::MultiPoint => "MULTIPOINT",
        ColumnType::MultiLineString => "MULTILINESTRING",
        ColumnType::GeometryCollection => "GEOMETRYCOLLECTION",
        _ => unreachable!(),
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{value}\"")
}

fn to_wkb(geometry: &Geometry<f64>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    wkb::writer::write_geometry(&mut bytes, geometry, &Default::default())?;
    Ok(bytes)
}

fn to_gpkg_geometry(geometry: &Geometry<f64>) -> Result<Vec<u8>> {
    let wkb = to_wkb(geometry)?;
    let mut bytes = Vec::with_capacity(8 + wkb.len());
    bytes.extend_from_slice(b"GP");
    bytes.push(0);
    bytes.push(1); // little-endian header, no envelope
    bytes.extend_from_slice(&4326_i32.to_le_bytes());
    bytes.extend_from_slice(&wkb);
    Ok(bytes)
}

fn geometry_extent(geometry: &Geometry<f64>) -> Option<Extent> {
    let mut extent = None;
    let mut include = |x: f64, y: f64| match &mut extent {
        None => {
            extent = Some(Extent {
                min_x: x,
                min_y: y,
                max_x: x,
                max_y: y,
            })
        }
        Some(current) => current.include(Extent {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
        }),
    };
    visit_coordinates(geometry, &mut include);
    extent
}

fn visit_coordinates(geometry: &Geometry<f64>, include: &mut impl FnMut(f64, f64)) {
    match geometry {
        Geometry::Point(point) => include(point.x(), point.y()),
        Geometry::LineString(line) => line.0.iter().for_each(|coord| include(coord.x, coord.y)),
        Geometry::MultiPoint(points) => points
            .0
            .iter()
            .for_each(|point| include(point.x(), point.y())),
        Geometry::MultiLineString(lines) => lines
            .0
            .iter()
            .flat_map(|line| line.0.iter())
            .for_each(|coord| include(coord.x, coord.y)),
        Geometry::GeometryCollection(collection) => collection
            .0
            .iter()
            .for_each(|geometry| visit_coordinates(geometry, include)),
        _ => {}
    }
}
