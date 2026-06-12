//! Filesystem locations for Phora's shared state, rooted via XDG base directories.
//!
//! [`cache_root`] holds regenerable git mirrors: `XDG_CACHE_HOME` when set,
//! else [`dirs::cache_dir`] (macOS `~/Library/Caches`, Linux `~/.cache`), then `/phora`.
//!
//! [`state_root`] holds the per-project registry (deploy journal, locks, records):
//! `XDG_STATE_HOME` when set, else [`dirs::state_dir`] (Linux `~/.local/state`), else
//! [`dirs::data_dir`] — the macOS fallback, since macOS has no native state dir
//! (`~/Library/Application Support`) — then `/phora`.
//!
//! An `XDG_*` override is honored only when absolute, per the XDG spec; a relative
//! value is ignored and the platform default is used.
//!
//! `XDG_DATA_HOME` and `XDG_CONFIG_HOME` are intentionally unused: phora has no
//! portable data payload (the registry is machine-local state, mirrors are
//! regenerable cache) and no global config root (config is project-local
//! `phora.toml`).

use std::path::PathBuf;

use crate::error::{Error, Result};

fn xdg_base(
    var_value: Option<std::ffi::OsString>,
    fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    var_value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or(fallback)
}

pub fn cache_root() -> Result<PathBuf> {
    let base = xdg_base(std::env::var_os("XDG_CACHE_HOME"), dirs::cache_dir())
        .ok_or_else(|| Error::Config("no cache directory".into()))?;
    Ok(base.join("phora"))
}

pub fn state_root() -> Result<PathBuf> {
    let base = xdg_base(
        std::env::var_os("XDG_STATE_HOME"),
        dirs::state_dir().or_else(dirs::data_dir),
    )
    .ok_or_else(|| Error::Config("no state directory".into()))?;
    Ok(base.join("phora"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_override_is_honored() {
        assert_eq!(
            xdg_base(Some("/abs/override".into()), Some(PathBuf::from("/fallback"))),
            Some(PathBuf::from("/abs/override")),
        );
    }

    #[test]
    fn relative_override_falls_through_to_fallback() {
        assert_eq!(
            xdg_base(Some("relative/path".into()), Some(PathBuf::from("/fallback"))),
            Some(PathBuf::from("/fallback")),
        );
    }

    #[test]
    fn missing_var_uses_fallback() {
        assert_eq!(
            xdg_base(None, Some(PathBuf::from("/fallback"))),
            Some(PathBuf::from("/fallback")),
        );
    }

    #[test]
    fn no_usable_path_yields_none() {
        assert_eq!(xdg_base(None, None), None);
        assert_eq!(xdg_base(Some("rel".into()), None), None);
    }
}
