use std::{fs::File, path::PathBuf, process::ExitCode};

use clap::Parser;
use eyre::Context;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Process transactions and write account balances as CSV"
)]
struct Args {
    input_file: PathBuf,
}

fn run(args: &Args) -> eyre::Result<()> {
    let input = File::open(&args.input_file)
        .with_context(|| format!("opening input {} failed", args.input_file.display()))?;
    payments_engine::process(input, std::io::stdout().lock())
        .with_context(|| format!("processing input {} failed", args.input_file.display()))
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = ?error, "processing failed");
            ExitCode::FAILURE
        }
    }
}
