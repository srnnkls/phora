//! Filesystem locations for Phora's shared state.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// Root of Phora's shared state: `~/.phora`.
pub fn phora_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| Error::Config("no home directory".into()))?;
    Ok(home.join(".phora"))
}
