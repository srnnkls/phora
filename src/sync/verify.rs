use crate::config::Config;
use crate::error::{Error, Result};
use crate::store::{ArtifactKey, Registry};

/// Why a deployed file failed verification against its registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyReason {
    /// The deployed file's content hash differs from the recorded `blake3`.
    ContentMismatch { expected: String, actual: String },
    /// The recorded file is absent on disk at the deployed location.
    Missing,
}

/// A single deployed file that does not match its registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyMismatch {
    pub key: ArtifactKey,
    pub path: std::path::PathBuf,
    pub reason: VerifyReason,
}

pub fn verify(config: &Config, registry: &dyn Registry) -> Result<Vec<VerifyMismatch>> {
    let mut mismatches = Vec::new();
    for record in registry.list_all()? {
        if record.linked {
            continue;
        }
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let artifact_dir = target.expanded_path().join(
            target
                .layout()
                .artifact_path(&record.key.source, &record.key.artifact),
        );
        for file in &record.files {
            let dst = artifact_dir.join(&file.path);
            match std::fs::read(&dst) {
                Ok(content) => {
                    let actual = blake3::hash(&content).to_hex().to_string();
                    if actual != file.blake3 {
                        mismatches.push(VerifyMismatch {
                            key: record.key.clone(),
                            path: file.path.clone(),
                            reason: VerifyReason::ContentMismatch {
                                expected: file.blake3.clone(),
                                actual,
                            },
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    mismatches.push(VerifyMismatch {
                        key: record.key.clone(),
                        path: file.path.clone(),
                        reason: VerifyReason::Missing,
                    });
                }
                Err(e) => {
                    return Err(Error::Sync(format!("verify read {}: {e}", dst.display())));
                }
            }
        }
    }
    Ok(mismatches)
}
