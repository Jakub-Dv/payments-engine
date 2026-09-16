fn main() {
    let _fmt = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = kraken_assignment::run() {
        tracing::error!(
            action = "run",
            error = format!("{e:#}"),
            "The main application run failed.",
        );
        std::process::exit(1);
    }
}
