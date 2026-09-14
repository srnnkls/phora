use crate::config::Config;
use crate::error::{Error, Result};
use crate::lock::Lock;
use crate::source::{
    SourceName, SourceStore, WorktreeObservationLevel, WorktreeObservationLock,
    WorktreeObservationResult,
};
use crate::sync::scan::link_target_bytes;
use crate::sync::state::{ArtifactKey, ManifestEntryKind, StateStore};

/// Why a deployed file failed verification against its registry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyReason {
    /// The deployed file's content hash differs from the recorded `blake3`.
    ContentMismatch { expected: String, actual: String },
    /// The deployed entry type differs from the recorded manifest kind.
    EntryKindMismatch,
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

/// A report-only history-overlay finding. It is surfaced to callers but does not make
/// [`VerifyReport::is_clean`] false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayFinding {
    pub key: ArtifactKey,
    pub reason: String,
    pub remedy: &'static str,
}

/// Verification findings. Overlay findings are report-only and do not affect [`Self::is_clean`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub mismatches: Vec<VerifyMismatch>,
    pub untrusted_hooks: Vec<UntrustedHookFinding>,
    pub overlay_findings: Vec<OverlayFinding>,
}

impl VerifyReport {
    /// Returns whether content and hook verification found no failures; overlay findings are
    /// report-only.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty() && self.untrusted_hooks.is_empty()
    }
}

pub fn verify(
    config: &Config,
    registry: &dyn StateStore,
    lock: Option<&Lock>,
    backend: &dyn SourceStore,
) -> Result<VerifyReport> {
    Ok(VerifyReport {
        mismatches: verify_mismatches(config, registry)?,
        untrusted_hooks: untrusted_hook_findings(lock),
        overlay_findings: overlay_findings(config, registry, backend)?,
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

fn overlay_findings(
    config: &Config,
    registry: &dyn StateStore,
    backend: &dyn SourceStore,
) -> Result<Vec<OverlayFinding>> {
    let records = registry.all_artifacts()?;
    let ejected = crate::sync::state::ejected_index(registry, &records)?;
    let mut findings = Vec::new();
    for record in records {
        if !record.history
            || record.linked
            || ejected.contains(&(
                record.key.target.clone(),
                record.key.source.clone(),
                record.key.artifact.clone(),
            ))
        {
            continue;
        }
        let Some(target) = config.targets.get(&record.key.target) else {
            continue;
        };
        let artifact_root = super::target::record_artifact_path(target, &record);
        let Some(request) = crate::sync::observe::history_observation_request(
            &record,
            SourceName::trusted(&record.key.source),
            &artifact_root,
            WorktreeObservationLock::Try,
            WorktreeObservationLevel::Semantic,
        )?
        else {
            continue;
        };
        match backend.observe_worktree(&request)? {
            WorktreeObservationResult::Conformant => {}
            WorktreeObservationResult::Stale => findings.push(OverlayFinding {
                key: record.key,
                reason: "history overlay is stale".to_owned(),
                remedy: "phora sync",
            }),
            WorktreeObservationResult::Unknown => findings.push(OverlayFinding {
                key: record.key,
                reason: "history overlay could not be observed".to_owned(),
                remedy: "phora sync",
            }),
        }
    }
    Ok(findings)
}

fn verify_mismatches(config: &Config, registry: &dyn StateStore) -> Result<Vec<VerifyMismatch>> {
    let mut mismatches = Vec::new();
    let records = registry.all_artifacts()?;
    let ejected = crate::sync::state::ejected_index(registry, &records)?;
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
            match std::fs::symlink_metadata(&dst) {
                Ok(metadata) => {
                    let actual_kind = if metadata.file_type().is_symlink() {
                        ManifestEntryKind::Link
                    } else {
                        ManifestEntryKind::File
                    };
                    if actual_kind != file.kind {
                        mismatches.push(VerifyMismatch {
                            key: record.key.clone(),
                            path: file.path.clone(),
                            reason: VerifyReason::EntryKindMismatch,
                        });
                        continue;
                    }
                    let content = match file.kind {
                        ManifestEntryKind::File => std::fs::read(&dst),
                        ManifestEntryKind::Link => link_target_bytes(&dst),
                    }
                    .map_err(|e| Error::Sync(format!("verify read {}: {e}", dst.display())))?;
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
                    return Err(Error::Sync(format!("verify stat {}: {e}", dst.display())));
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
    use crate::source::GitBackend;
    use crate::sync::state::FileStateStore;
    use tempfile::TempDir;

    fn empty_registry() -> (TempDir, FileStateStore) {
        let dir = TempDir::new().expect("temp state root");
        let reg = FileStateStore::open(dir.path().to_path_buf()).expect("open registry");
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
        let (dir, reg) = empty_registry();
        let backend = GitBackend::new(dir.path().to_path_buf());
        let lock = lock_with(vec![candidate("blake3:untrusted")], Vec::new());

        let report = verify(&config(), &reg, Some(&lock), &backend).expect("verify runs");

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
        let (dir, reg) = empty_registry();
        let backend = GitBackend::new(dir.path().to_path_buf());
        let trusted = vec![TrustedHook {
            dep_instance: "inst0001".to_owned(),
            hook_id: "inst0001%1%editor#on_change#abc".to_owned(),
            preimage: "blake3:approved".to_owned(),
            approved_at: "2026-06-20T00:00:00Z".to_owned(),
            source: "mydeps".to_owned(),
            commit: "c0ffee".to_owned(),
        }];
        let lock = lock_with(vec![candidate("blake3:approved")], trusted);

        let report = verify(&config(), &reg, Some(&lock), &backend).expect("verify runs");

        assert!(
            report.untrusted_hooks.is_empty(),
            "a candidate whose preimage matches a trusted_hooks approval must NOT surface \
             (anti-TOFU: the match is what grants trust)"
        );
        assert!(report.is_clean(), "a trusted candidate leaves verify clean");
    }

    #[test]
    fn verify_without_a_lock_surfaces_no_hook_findings() {
        let (dir, reg) = empty_registry();
        let backend = GitBackend::new(dir.path().to_path_buf());

        let report = verify(&config(), &reg, None, &backend).expect("verify runs");

        assert!(
            report.untrusted_hooks.is_empty(),
            "with no lock there are no candidate hooks to gate on"
        );
        assert!(report.is_clean());
    }
}
