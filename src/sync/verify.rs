use crate::config::Config;
use crate::error::{Error, Result};
use crate::lock::Lock;
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

/// A composed target whose dep carries a stripped, still-untrusted `on_change` hook: the
/// artifact is deployed but NOT post-processed, so it may be incomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedHookFinding {
    /// Consumer-facing root import name to pass to `phora trust`.
    pub source: String,
    /// Namespaced `composed_target#on_change#…` identity of the stripped hook.
    pub hook_id: String,
}

/// What `verify` found: per-file content mismatches plus untrusted stripped-hook gaps. Either
/// being non-empty fails CI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub mismatches: Vec<VerifyMismatch>,
    pub untrusted_hooks: Vec<UntrustedHookFinding>,
}

impl VerifyReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty() && self.untrusted_hooks.is_empty()
    }
}

pub fn verify(
    config: &Config,
    registry: &dyn Registry,
    lock: Option<&Lock>,
) -> Result<VerifyReport> {
    Ok(VerifyReport {
        mismatches: verify_mismatches(config, registry)?,
        untrusted_hooks: untrusted_hook_findings(lock),
    })
}

/// Each `candidate_hooks` entry whose preimage is not approved in `trusted_hooks` (anti-TOFU),
/// reusing the same trust predicate `sync` applies before running a transitive hook.
fn untrusted_hook_findings(lock: Option<&Lock>) -> Vec<UntrustedHookFinding> {
    let Some(lock) = lock else {
        return Vec::new();
    };
    let trusted = super::trusted_preimages(Some(lock));
    lock.candidate_hooks
        .iter()
        .filter(|c| !trusted.contains(&c.preimage))
        .map(|c| UntrustedHookFinding {
            source: c.source.clone(),
            hook_id: c.hook_id.clone(),
        })
        .collect()
}

fn verify_mismatches(config: &Config, registry: &dyn Registry) -> Result<Vec<VerifyMismatch>> {
    let mut mismatches = Vec::new();
    let records = registry.list_all()?;
    let ejected = crate::store::ejected_index(registry, &records)?;
    for record in records {
        if record.linked {
            continue;
        }
        let k = &record.key;
        if ejected.contains(&(k.target.clone(), k.source.clone(), k.artifact.clone())) {
            continue;
        }
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let artifact_dir = super::target::record_manifest_base(target, &record);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{CandidateHookRecord, LOCK_SCHEMA_VERSION, Lock, TrustedHook};
    use crate::store::FileRegistry;
    use tempfile::TempDir;

    fn empty_registry() -> (TempDir, FileRegistry) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileRegistry::open(dir.path().to_path_buf()).expect("open registry");
        (dir, reg)
    }

    fn config() -> Config {
        Config::parse("version = 1\n").expect("minimal config parses")
    }

    fn candidate(preimage: &str) -> CandidateHookRecord {
        CandidateHookRecord {
            dep_instance: "inst0001".to_owned(),
            hook_id: "inst0001%1%editor#on_change#abc".to_owned(),
            preimage: preimage.to_owned(),
            command: "./install.sh".to_owned(),
            source: "mydeps".to_owned(),
            commit: "c0ffee".to_owned(),
        }
    }

    fn lock_with(candidates: Vec<CandidateHookRecord>, trusted: Vec<TrustedHook>) -> Lock {
        Lock {
            version: LOCK_SCHEMA_VERSION,
            sources: Vec::new(),
            trusted_hooks: trusted,
            candidate_hooks: candidates,
        }
    }

    #[test]
    fn verify_flags_an_untrusted_stripped_hook_candidate() {
        let (_dir, reg) = empty_registry();
        let lock = lock_with(vec![candidate("blake3:untrusted")], Vec::new());

        let report = verify(&config(), &reg, Some(&lock)).expect("verify runs");

        assert!(
            report.mismatches.is_empty(),
            "no deployed files => no content mismatch"
        );
        assert_eq!(
            report.untrusted_hooks.len(),
            1,
            "a candidate hook whose preimage is not approved must surface as a finding"
        );
        assert_eq!(report.untrusted_hooks[0].source, "mydeps");
        assert!(
            !report.is_clean(),
            "an untrusted stripped hook must make the report non-clean so CI fails non-zero"
        );
    }

    #[test]
    fn verify_does_not_flag_a_candidate_whose_preimage_is_trusted() {
        let (_dir, reg) = empty_registry();
        let trusted = vec![TrustedHook {
            dep_instance: "inst0001".to_owned(),
            hook_id: "inst0001%1%editor#on_change#abc".to_owned(),
            preimage: "blake3:approved".to_owned(),
            approved_at: "2026-06-20T00:00:00Z".to_owned(),
            source: "mydeps".to_owned(),
            commit: "c0ffee".to_owned(),
        }];
        let lock = lock_with(vec![candidate("blake3:approved")], trusted);

        let report = verify(&config(), &reg, Some(&lock)).expect("verify runs");

        assert!(
            report.untrusted_hooks.is_empty(),
            "a candidate whose preimage matches a trusted_hooks approval must NOT surface \
             (anti-TOFU: the match is what grants trust)"
        );
        assert!(report.is_clean(), "a trusted candidate leaves verify clean");
    }

    #[test]
    fn verify_without_a_lock_surfaces_no_hook_findings() {
        let (_dir, reg) = empty_registry();

        let report = verify(&config(), &reg, None).expect("verify runs");

        assert!(
            report.untrusted_hooks.is_empty(),
            "with no lock there are no candidate hooks to gate on"
        );
        assert!(report.is_clean());
    }
}
