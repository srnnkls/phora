use clap::Parser;

use phora::cli::{self, Cli};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    cli::run(cli).map_err(|e| {
        eprintln!("error: {e}");
        e.into()
    })
}
