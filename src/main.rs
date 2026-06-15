use clap::Parser;

use phora::cli::{self, Cli};

fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli::run(cli) {
        eprintln!("error: {e}");
        std::process::exit(cli::exit_code(&e));
    }
}
