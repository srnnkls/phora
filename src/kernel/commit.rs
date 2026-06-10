use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// A resolved git commit id: 40-hex (sha1) or 64-hex (sha256), canonicalized to lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Commit(String);

impl Commit {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Commit {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let len_ok = s.len() == 40 || s.len() == 64;
        let hex_ok = s.bytes().all(|b| b.is_ascii_hexdigit());
        if len_ok && hex_ok {
            Ok(Self(s.to_ascii_lowercase()))
        } else {
            Err(Error::Source(format!(
                "invalid commit id `{s}`: expected 40 or 64 hex chars"
            )))
        }
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
