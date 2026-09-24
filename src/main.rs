use clap::Parser;

use phora::cli::{self, Cli};

fn main() {
    #[cfg(feature = "trace")]
    init_tracing();
    let cli = Cli::parse();
    let exit_code = match cli::run_with_outcome(cli) {
        Ok(outcome) => outcome.exit_code(),
        Err(error) => {
            eprintln!("error: {error}");
            cli::exit_code(&error)
        }
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(feature = "trace")]
fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::CLOSE)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_thread_names(true)
        .with_writer(std::io::stderr)
        .init();
}
