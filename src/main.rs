use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgAction, Parser};
use osmdb_extract::{ExtractOptions, OutputFormat, extract};

#[derive(Debug, Parser)]
#[command(
    name = "osmdb-extract",
    version,
    about = "Extract typed geospatial layers from an osmdb database using Lua"
)]
struct Cli {
    /// Directory containing data.rocksdb and locations.bin.
    #[arg(long)]
    db: PathBuf,

    /// Lua extraction configuration. Repeat to run multiple scripts.
    #[arg(long, required = true)]
    script: Vec<PathBuf>,

    /// Output file format.
    #[arg(long, value_enum)]
    format: OutputFormat,

    /// GeoPackage file or GeoParquet directory to create.
    #[arg(long)]
    output: PathBuf,

    /// Optional local wikidata_store directory for Lua entity lookups.
    #[arg(long)]
    wikidata_store: Option<PathBuf>,

    /// Number of parallel Lua workers.
    #[arg(long, default_value_t = default_threads())]
    threads: usize,

    /// Increase logging verbosity (-v for info, -vv for debug).
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let summary = extract(ExtractOptions {
        db: cli.db,
        scripts: cli.script,
        format: cli.format,
        output: cli.output.clone(),
        threads: cli.threads,
        wikidata_store: cli.wikidata_store,
    })?;
    println!(
        "Wrote {} rows to {} ({} nodes, {} ways, {} relations processed; {} skipped for geometry)",
        summary.rows_written,
        cli.output.display(),
        summary.nodes_processed,
        summary.ways_processed,
        summary.relations_processed,
        summary.geometry_skipped,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn accepts_repeated_script_arguments() {
        let cli = Cli::try_parse_from([
            "osmdb-extract",
            "--db",
            "region.osmdb",
            "--script",
            "cafes.lua",
            "--script",
            "roads.lua",
            "--format",
            "geopackage",
            "--output",
            "region.gpkg",
        ])
        .unwrap();

        assert_eq!(cli.script.len(), 2);
    }

    #[test]
    fn requires_a_script_argument() {
        let error = Cli::try_parse_from([
            "osmdb-extract",
            "--db",
            "region.osmdb",
            "--format",
            "geopackage",
            "--output",
            "region.gpkg",
        ])
        .unwrap_err();

        assert!(error.to_string().contains("--script"));
    }
}
