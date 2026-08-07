use anyhow::Result;
use clap::Parser;
use mempool_atlas::perf_fixtures::{ExportOptions, FixtureProfile, export};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(about = "Export canonical browser fixtures from the Rust domain model")]
struct Cli {
    #[arg(long, value_enum)]
    profile: FixtureProfile,
    #[arg(long, default_value = "web/.perf-fixtures")]
    output_root: PathBuf,
    #[arg(long, default_value_t = 70_000)]
    transaction_count: usize,
    #[arg(long, default_value_t = 2)]
    source_count: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let manifest = export(ExportOptions {
        profile: cli.profile,
        output_root: cli.output_root,
        transaction_count: cli.transaction_count,
        source_count: cli.source_count,
    })?;
    println!(
        "exported {} fixture profile with {} sources to {}",
        manifest.profile,
        manifest.snapshots.len(),
        manifest.output_directory.display()
    );
    Ok(())
}
