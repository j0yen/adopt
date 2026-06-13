//! CLI argument parsing and command dispatch.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use adopt::scan;

/// Detect shipped wintermute artifacts that never entered the live system.
#[derive(Parser)]
#[command(name = "adopt", version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan wintermute repos for unadopted or stale artifacts.
    Scan {
        /// Output format.
        #[arg(long, default_value = "table")]
        format: OutputFormat,

        /// Include installed-current and not-a-bin entries in output.
        #[arg(long)]
        all: bool,

        /// Restrict output to artifacts whose bin name matches this regex.
        #[arg(long, value_name = "REGEX")]
        r#match: Option<String>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

/// Entry point called from `main`.
///
/// # Errors
/// Returns an error if scanning or output fails.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn run() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Scan { format, all, r#match } => {
            let results = scan::run_scan(all, r#match.as_deref())?;
            match format {
                OutputFormat::Table => scan::print_table(&results),
                OutputFormat::Json => scan::print_json(&results)?,
            }
        }
    }
    Ok(())
}
