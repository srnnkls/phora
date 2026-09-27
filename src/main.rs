use std::io::IsTerminal;

use clap::Parser;

use phora::cli::{self, Cli};

fn main() {
    forbid_credential_prompts_without_terminal();
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

/// Credential helpers inherit this environment, and a prompt nobody can answer (Git
/// Credential Manager's account picker) hangs the sync instead of failing it.
fn forbid_credential_prompts_without_terminal() {
    if std::io::stdin().is_terminal() {
        return;
    }
    for (key, value) in [("GIT_TERMINAL_PROMPT", "0"), ("GCM_INTERACTIVE", "never")] {
        if std::env::var_os(key).is_none() {
            // SAFETY: main calls this before it starts any thread.
            unsafe { std::env::set_var(key, value) };
        }
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
