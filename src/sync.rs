//! Top-level orchestration: sync, eject, uneject.

use crate::config::{Config, Lock};
use crate::error::{Error, Result};
use crate::registry::Registry;

pub fn sync(
    _base_config: &Config,
    _local_config: Option<&Config>,
    _base_lock: Option<Lock>,
    _local_lock: Option<Lock>,
    _force: bool,
    _interactive: bool,
    _prune: bool,
) -> Result<(Lock, Option<Lock>)> {
    Err(Error::NotImplemented("sync"))
}

pub fn eject(
    _config: &Config,
    _registry: &dyn Registry,
    _artifact: &str,
    _source: &str,
    _target: &str,
) -> Result<()> {
    Err(Error::NotImplemented("eject"))
}

pub fn uneject(
    _config: &Config,
    _registry: &dyn Registry,
    _artifact: &str,
    _source: &str,
    _target: &str,
) -> Result<()> {
    Err(Error::NotImplemented("uneject"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_is_unimplemented() {
        use std::collections::BTreeMap;
        let config = Config {
            version: 1,
            hosts: BTreeMap::new(),
            sources: BTreeMap::new(),
            targets: BTreeMap::new(),
        };
        assert!(sync(&config, None, None, None, false, false, false).is_err());
    }
}
