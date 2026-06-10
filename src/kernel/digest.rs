use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize, Serializer};

use crate::error::{Error, Result};

/// A content/integrity digest `<algo>:<64 lowercase hex>`, algo ∈ {sha256, blake3}.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Digest {
    algo: Algo,
    bytes: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algo {
    Sha256,
    Blake3,
}

impl Algo {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Blake3 => "blake3",
        }
    }
}

impl Digest {
    #[must_use]
    pub fn sha256(bytes: [u8; 32]) -> Self {
        Self {
            algo: Algo::Sha256,
            bytes,
        }
    }

    #[must_use]
    pub fn blake3(bytes: [u8; 32]) -> Self {
        Self {
            algo: Algo::Blake3,
            bytes,
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn algo(&self) -> Algo {
        self.algo
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (algo, hex) = s.split_once(':').ok_or_else(|| {
            Error::Config(format!("invalid digest `{s}`: missing `<algo>:` prefix"))
        })?;
        let bytes = decode_hex32(hex).ok_or_else(|| {
            Error::Config(format!("invalid digest `{s}`: body must be 64 hex chars"))
        })?;
        let algo = match algo {
            "sha256" => Algo::Sha256,
            "blake3" => Algo::Blake3,
            other => {
                return Err(Error::Config(format!(
                    "invalid digest `{s}`: unknown algorithm `{other}` (expected sha256 or blake3)"
                )));
            }
        };
        Ok(Self { algo, bytes })
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.algo.as_str())?;
        for b in &self.bytes {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *slot = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(out)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}
