//! Command-line surface.

use clap::{Parser, Subcommand};

use crate::error::{Error, Result};

#[derive(Parser, Debug)]
#[command(name = "phora", version, about = "Git-based artifact package manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse a URL and add a source to the config.
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        root: Option<String>,
    },
    /// Fetch sources and project them to targets.
    Sync {
        #[arg(long)]
        prune: bool,
        #[arg(long)]
        force: bool,
    },
    /// Bump the lock to latest, then sync.
    Update { source: Option<String> },
    /// Show sources and deployment state.
    List {
        #[arg(long)]
        plan: bool,
    },
    /// Verify deployed files by hashing contents.
    Verify,
    /// Query the global registry.
    Where {
        #[arg(long)]
        digest: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        artifact: Option<String>,
        #[arg(long)]
        commit: Option<String>,
    },
    /// Permanently stop managing an artifact, keeping its files.
    Eject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Resume managing a previously ejected artifact.
    Uneject {
        artifact: String,
        #[arg(long)]
        source: String,
        #[arg(long)]
        target: String,
    },
    /// Rebuild the global registry from lock and on-disk targets.
    RebuildRegistry,
    /// Debug include/exclude matching for a path.
    CheckMatch {
        #[arg(long)]
        source: String,
        path: String,
    },
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the command implementation will consume the parsed args"
)]
pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Add { .. } => Err(Error::NotImplemented("add")),
        Command::Sync { .. } => Err(Error::NotImplemented("sync")),
        Command::Update { .. } => Err(Error::NotImplemented("update")),
        Command::List { .. } => Err(Error::NotImplemented("list")),
        Command::Verify => Err(Error::NotImplemented("verify")),
        Command::Where { .. } => Err(Error::NotImplemented("where")),
        Command::Eject { .. } => Err(Error::NotImplemented("eject")),
        Command::Uneject { .. } => Err(Error::NotImplemented("uneject")),
        Command::RebuildRegistry => Err(Error::NotImplemented("rebuild-registry")),
        Command::CheckMatch { .. } => Err(Error::NotImplemented("check-match")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
