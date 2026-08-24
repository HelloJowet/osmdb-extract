use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use crossbeam_channel::bounded;
use osmdb::{Database, DatabaseConfig, DatabaseReader, LOCATION_STORE_FILENAME};
use wikidata_store::WikidataStore;

use crate::geometry::GeometryResolver;
use crate::lua::{LuaWorker, ObjectOutcome};
use crate::output::OutputWriter;
use crate::schema::OutputRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Geopackage,
    Geoparquet,
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub db: PathBuf,
    pub script: PathBuf,
    pub format: OutputFormat,
    pub output: PathBuf,
    pub threads: usize,
    pub wikidata_store: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractSummary {
    pub nodes_processed: u64,
    pub ways_processed: u64,
    pub relations_processed: u64,
    pub geometry_skipped: u64,
    pub rows_written: u64,
}

struct Counters {
    nodes: AtomicU64,
    ways: AtomicU64,
    relations: AtomicU64,
    skipped: AtomicU64,
    rows: AtomicU64,
}

impl Counters {
    fn new() -> Self {
        Self {
            nodes: AtomicU64::new(0),
            ways: AtomicU64::new(0),
            relations: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            rows: AtomicU64::new(0),
        }
    }

    fn summary(&self) -> ExtractSummary {
        ExtractSummary {
            nodes_processed: self.nodes.load(Ordering::Relaxed),
            ways_processed: self.ways.load(Ordering::Relaxed),
            relations_processed: self.relations.load(Ordering::Relaxed),
            geometry_skipped: self.skipped.load(Ordering::Relaxed),
            rows_written: self.rows.load(Ordering::Relaxed),
        }
    }
}

pub fn extract(options: ExtractOptions) -> Result<ExtractSummary> {
    validate_options(&options)?;
    let script = fs::read(&options.script)
        .with_context(|| format!("failed to read Lua script '{}'", options.script.display()))?;
    let wikidata_store = options
        .wikidata_store
        .as_ref()
        .map(|path| {
            WikidataStore::open(path)
                .with_context(|| format!("failed to open Wikidata store '{}'", path.display()))
                .map(Arc::new)
        })
        .transpose()?;

    let rocksdb_path = options.db.join("data.rocksdb");
    let location_path = options.db.join(LOCATION_STORE_FILENAME);
    let config = DatabaseConfig::for_reading(&rocksdb_path);
    let database =
        Arc::new(Database::open_read_only(&config).with_context(|| {
            format!("failed to open osmdb database '{}'", options.db.display())
        })?);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.threads)
        .thread_name(|index| format!("osmdb-extract-{index}"))
        .build()?;

    let mut workers = Vec::with_capacity(options.threads);
    for _ in 0..options.threads {
        let resolver = Arc::new(GeometryResolver::new(
            Arc::clone(&database),
            &location_path,
        )?);
        workers.push(Mutex::new(LuaWorker::new_with_wikidata_store(
            &script,
            resolver,
            wikidata_store.clone(),
        )?));
    }
    let layers = workers[0].lock().unwrap().layers().to_vec();
    for (index, worker) in workers.iter().enumerate().skip(1) {
        if worker.lock().unwrap().layers() != layers {
            bail!(
                "Lua worker {index} declared a different output schema; script initialization must be deterministic"
            );
        }
    }
    let workers = Arc::new(workers);

    let mut temporary = TemporaryOutput::new(&options.output)?;
    let temporary_path = temporary.path.clone();
    let (sender, receiver) = bounded::<Vec<OutputRow>>(options.threads * 4);
    let writer_layers = layers.clone();
    let format = options.format;
    let writer_handle = thread::Builder::new()
        .name("osmdb-output-writer".to_string())
        .spawn(move || -> Result<()> {
            let mut writer = OutputWriter::create(format, &temporary_path, &writer_layers)?;
            while let Ok(rows) = receiver.recv() {
                writer.push_rows(rows)?;
            }
            writer.finish()
        })?;

    let cancelled = Arc::new(AtomicBool::new(false));
    let first_error = Arc::new(Mutex::new(None::<String>));
    let counters = Arc::new(Counters::new());
    let reader = DatabaseReader::new(database.inner());

    let processing_result = (|| -> Result<()> {
        tracing::info!("processing tagged nodes");
        pool.install(|| {
            let workers = Arc::clone(&workers);
            let sender = sender.clone();
            let cancelled = Arc::clone(&cancelled);
            let first_error = Arc::clone(&first_error);
            let counters = Arc::clone(&counters);
            reader.par_iter_nodes(move |id, node| {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                counters.nodes.fetch_add(1, Ordering::Relaxed);
                let index = rayon::current_thread_index().unwrap_or(0);
                let result = workers[index].lock().unwrap().process_node(id, node);
                handle_outcome(
                    "node",
                    id,
                    result,
                    &sender,
                    &cancelled,
                    &first_error,
                    &counters,
                );
            })
        })?;

        if !cancelled.load(Ordering::Relaxed) {
            tracing::info!("processing ways");
            pool.install(|| {
                let workers = Arc::clone(&workers);
                let sender = sender.clone();
                let cancelled = Arc::clone(&cancelled);
                let first_error = Arc::clone(&first_error);
                let counters = Arc::clone(&counters);
                reader.par_iter_ways(move |id, way| {
                    if cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    counters.ways.fetch_add(1, Ordering::Relaxed);
                    let index = rayon::current_thread_index().unwrap_or(0);
                    let result = workers[index].lock().unwrap().process_way(id, way);
                    handle_outcome(
                        "way",
                        id,
                        result,
                        &sender,
                        &cancelled,
                        &first_error,
                        &counters,
                    );
                })
            })?;
        }

        if !cancelled.load(Ordering::Relaxed) {
            tracing::info!("processing relations");
            pool.install(|| {
                let workers = Arc::clone(&workers);
                let sender = sender.clone();
                let cancelled = Arc::clone(&cancelled);
                let first_error = Arc::clone(&first_error);
                let counters = Arc::clone(&counters);
                reader.par_iter_relations(move |id, relation| {
                    if cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    counters.relations.fetch_add(1, Ordering::Relaxed);
                    let index = rayon::current_thread_index().unwrap_or(0);
                    let result = workers[index]
                        .lock()
                        .unwrap()
                        .process_relation(id, relation);
                    handle_outcome(
                        "relation",
                        id,
                        result,
                        &sender,
                        &cancelled,
                        &first_error,
                        &counters,
                    );
                })
            })?;
        }
        Ok(())
    })();

    drop(sender);
    let writer_result = writer_handle
        .join()
        .map_err(|_| anyhow!("output writer thread panicked"))?;
    processing_result?;
    if let Some(error) = first_error.lock().unwrap().take() {
        bail!("{error}");
    }
    writer_result?;

    temporary.publish(&options.output)?;
    Ok(counters.summary())
}

fn handle_outcome(
    kind: &str,
    id: i64,
    result: Result<ObjectOutcome>,
    sender: &crossbeam_channel::Sender<Vec<OutputRow>>,
    cancelled: &AtomicBool,
    first_error: &Mutex<Option<String>>,
    counters: &Counters,
) {
    match result {
        Ok(ObjectOutcome::Rows(rows)) => {
            counters
                .rows
                .fetch_add(rows.len() as u64, Ordering::Relaxed);
            if !rows.is_empty() && sender.send(rows).is_err() {
                set_error(
                    cancelled,
                    first_error,
                    "output writer stopped unexpectedly".to_string(),
                );
            }
        }
        Ok(ObjectOutcome::GeometrySkipped(reason)) => {
            counters.skipped.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(object_type = kind, object_id = id, %reason, "skipping object because geometry could not be resolved");
        }
        Err(error) => set_error(
            cancelled,
            first_error,
            format!("failed while processing {kind} {id}: {error:#}"),
        ),
    }
}

fn set_error(cancelled: &AtomicBool, first_error: &Mutex<Option<String>>, error: String) {
    cancelled.store(true, Ordering::Relaxed);
    let mut slot = first_error.lock().unwrap();
    if slot.is_none() {
        *slot = Some(error);
    }
}

fn validate_options(options: &ExtractOptions) -> Result<()> {
    if options.threads == 0 {
        bail!("--threads must be at least 1");
    }
    if !options.db.is_dir() {
        bail!("osmdb directory '{}' does not exist", options.db.display());
    }
    if !options.db.join("data.rocksdb").is_dir() {
        bail!(
            "'{}' is not an osmdb directory: data.rocksdb is missing",
            options.db.display()
        );
    }
    if !options.db.join(LOCATION_STORE_FILENAME).is_file() {
        bail!(
            "'{}' is not an osmdb directory: {} is missing",
            options.db.display(),
            LOCATION_STORE_FILENAME
        );
    }
    if !options.script.is_file() {
        bail!("Lua script '{}' does not exist", options.script.display());
    }
    if options.output.exists() {
        bail!("output path '{}' already exists", options.output.display());
    }
    let parent = output_parent(&options.output);
    if !parent.is_dir() {
        bail!(
            "output parent directory '{}' does not exist",
            parent.display()
        );
    }
    Ok(())
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

struct TemporaryOutput {
    path: PathBuf,
    published: bool,
}

impl TemporaryOutput {
    fn new(output: &Path) -> Result<Self> {
        let parent = output_parent(output);
        let name = output
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("output path must have a UTF-8 file name"))?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = parent.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
        if path.exists() {
            bail!("temporary output path '{}' already exists", path.display());
        }
        Ok(Self {
            path,
            published: false,
        })
    }

    fn publish(&mut self, output: &Path) -> Result<()> {
        fs::rename(&self.path, output).with_context(|| {
            format!(
                "failed to publish temporary output '{}' as '{}'",
                self.path.display(),
                output.display()
            )
        })?;
        self.published = true;
        Ok(())
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if self.published || !self.path.exists() {
            return;
        }
        let result = if self.path.is_dir() {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        };
        if let Err(error) = result {
            tracing::warn!(path = %self.path.display(), %error, "failed to remove temporary output");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::output_parent;
    use std::path::Path;

    #[test]
    fn output_parent_uses_current_directory_for_bare_filename() {
        assert_eq!(output_parent(Path::new("region.gpkg")), Path::new("."));
    }

    #[test]
    fn output_parent_preserves_explicit_parent_directory() {
        assert_eq!(
            output_parent(Path::new("exports/region.gpkg")),
            Path::new("exports")
        );
    }
}
