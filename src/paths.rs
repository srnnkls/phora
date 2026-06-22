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
//! A `phora.toml` `[paths]` override, when present, is itself the root (no `phora`
//! leaf is appended): absolute verbatim, relative joined under the project cwd. It
//! wins over the `XDG_*` env and the platform default. An `XDG_*` override is
//! honored only when absolute, per the XDG spec; a relative value is ignored and
//! the platform default is used.
//!
//! `XDG_DATA_HOME` and `XDG_CONFIG_HOME` are intentionally unused: phora has no
//! portable data payload (the registry is machine-local state, mirrors are
//! regenerable cache) and no global config root (config is project-local
//! `phora.toml`).

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

fn xdg_base(var_value: Option<std::ffi::OsString>, fallback: Option<PathBuf>) -> Option<PathBuf> {
    var_value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or(fallback)
}

fn rooted(override_: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    override_.map(|path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    })
}

pub fn cache_root() -> Result<PathBuf> {
    cache_root_for(None, Path::new("."))
}

pub fn state_root() -> Result<PathBuf> {
    state_root_for(None, Path::new("."))
}

pub fn cache_root_for(override_: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(root) = rooted(override_, cwd) {
        return Ok(root);
    }
    let base = xdg_base(std::env::var_os("XDG_CACHE_HOME"), dirs::cache_dir())
        .ok_or_else(|| Error::Config("no cache directory".into()))?;
    Ok(base.join("phora"))
}

pub fn state_root_for(override_: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(root) = rooted(override_, cwd) {
        return Ok(root);
    }
    let base = xdg_base(
        std::env::var_os("XDG_STATE_HOME"),
        dirs::state_dir().or_else(dirs::data_dir),
    )
    .ok_or_else(|| Error::Config("no state directory".into()))?;
    Ok(base.join("phora"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn config_absolute_cache_override_is_used_verbatim() {
        let root = cache_root_for(Some(Path::new("/abs/cache")), Path::new("/proj")).expect("root");
        assert_eq!(root, PathBuf::from("/abs/cache"));
    }

    #[test]
    fn config_relative_cache_override_joins_cwd() {
        let root =
            cache_root_for(Some(Path::new(".phora/cache")), Path::new("/proj")).expect("root");
        assert_eq!(root, PathBuf::from("/proj/.phora/cache"));
    }

    #[test]
    fn config_absolute_state_override_is_used_verbatim() {
        let root = state_root_for(Some(Path::new("/abs/state")), Path::new("/proj")).expect("root");
        assert_eq!(root, PathBuf::from("/abs/state"));
    }

    #[test]
    fn config_relative_state_override_joins_cwd() {
        let root =
            state_root_for(Some(Path::new(".phora/state")), Path::new("/proj")).expect("root");
        assert_eq!(root, PathBuf::from("/proj/.phora/state"));
    }

    #[test]
    fn none_cache_override_falls_through_to_default() {
        let with_override =
            cache_root_for(Some(Path::new("/abs/cache")), Path::new("/proj")).expect("root");
        let fallthrough = cache_root_for(None, Path::new("/proj")).expect("root");
        assert_ne!(
            with_override, fallthrough,
            "a None override must not adopt the configured path; it falls through to XDG/default"
        );
        assert_eq!(
            fallthrough,
            cache_root().expect("default cache root"),
            "None override must reproduce the arg-free XDG/default resolution exactly"
        );
    }

    #[test]
    fn none_state_override_falls_through_to_default() {
        let fallthrough = state_root_for(None, Path::new("/proj")).expect("root");
        assert_eq!(
            fallthrough,
            state_root().expect("default state root"),
            "None override must reproduce the arg-free XDG/default resolution exactly"
        );
    }

    #[test]
    fn absolute_override_is_honored() {
        assert_eq!(
            xdg_base(
                Some("/abs/override".into()),
                Some(PathBuf::from("/fallback"))
            ),
            Some(PathBuf::from("/abs/override")),
        );
    }

    #[test]
    fn relative_override_falls_through_to_fallback() {
        assert_eq!(
            xdg_base(
                Some("relative/path".into()),
                Some(PathBuf::from("/fallback"))
            ),
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
