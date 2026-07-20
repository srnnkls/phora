use clap::Parser;

use phora::cli::{self, Cli};

fn main() {
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
