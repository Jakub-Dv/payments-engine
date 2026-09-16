use clap::Parser;
use eyre::Context;

mod domain;
mod tx_processor;

use crate::domain::Row;
use crate::tx_processor::TransactionsProcessor;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    input_file: String,
}

pub fn run() -> eyre::Result<()> {
    let args = Args::parse();

    let mut csv_reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(&args.input_file)
        .with_context(|| format!("Failed to read file: {}", args.input_file))?;

    let mut tx_processor = TransactionsProcessor::new();

    for row in csv_reader.deserialize() {
        let row: Row = row.with_context(|| "Failed to deserialize row")?;
        if let Err(e) = tx_processor.process_row(row) {
            tracing::error!(
                action = "process-row",
                error = format!("{e:#}"),
                "Failed to process row"
            );
        }
    }

    let writer = std::io::LineWriter::new(std::io::stdout());
    let mut csv_writer = csv::Writer::from_writer(writer);

    for output_row in tx_processor.get_output() {
        csv_writer
            .serialize(output_row)
            .with_context(|| "Failed to serialize row")?;
    }
    csv_writer
        .flush()
        .with_context(|| "Failed to flush output")?;

    Ok(())
}
