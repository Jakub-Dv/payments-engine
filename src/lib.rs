use std::io::{Read, Write};

use eyre::Context;

mod domain;
mod tx_processor;

use domain::Row;
use tx_processor::TransactionsProcessor;

/// Processes transaction CSV from `input`, writing account balances to `output`.
/// Rejected transactions are logged and skipped. Unknown dispute references are ignored.
///
/// # Errors
/// Returns an error for malformed CSV, missing columns, read failures, or write failures.
/// No balances are written until the complete input has been processed.
pub fn process(input: impl Read, output: impl Write) -> eyre::Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(input);

    let headers = reader.headers().context("reading CSV headers failed")?;
    if !headers.is_empty() {
        for required in ["type", "client", "tx", "amount"] {
            eyre::ensure!(
                headers.iter().filter(|header| *header == required).count() == 1,
                "CSV must contain exactly one '{required}' column"
            );
        }
    }

    let mut processor = TransactionsProcessor::default();
    for row in reader.deserialize::<Row>() {
        let row = row.context("deserializing transaction failed")?;
        if let Err(error) = processor.process_row(row) {
            tracing::warn!(
                client = %error.client_id,
                tx = %error.tx_id,
                error = ?error,
                "transaction rejected"
            );
        }
    }

    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(output);
    writer
        .write_record(["client", "available", "held", "total", "locked"])
        .context("writing account headers failed")?;
    for account in processor.into_output() {
        writer
            .serialize(account)
            .context("writing account failed")?;
    }
    writer.flush().context("flushing account output failed")?;
    Ok(())
}
