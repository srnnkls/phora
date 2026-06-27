//! The `phora trust` command: inspect, approve, and revoke transitive-dep hooks.
//!
//! Before approving, both `--list` and the interactive flow render the file-level paths changed in
//! the dep between the last trusted commit and the current candidate commit (R8), so a consumer can
//! verify the hook's surrounding tree was not tampered. A synced mirror holds both commits; a
//! discovery-only mirror that lacks the prior commit degrades to a "run `phora sync` first" note.

use std::io::IsTerminal;
use std::path::Path;

use crate::config::transitive::TransitiveManifest;
use crate::error::{Error, Result};
use crate::lock::{CandidateHookRecord, Lock, TrustedHook};
use crate::source::{GitBackend, SourceBackend, SourceError};
use crate::sync::transitive::ResolvedGraph;

use super::{load_config, open_project_registry};

/// A discovery-only candidate carries no real preimage: it is resolved on first sync.
const UNRESOLVED_PREIMAGE: &str = "";

pub(super) fn run_trust(
    source: Option<&str>,
    list: bool,
    revoke: bool,
    show: Option<&str>,
) -> Result<()> {
    let config = load_config()?;
    let registry = open_project_registry(&config)?;
    let _guard = registry.lock_exclusive()?;

    let cwd = std::env::current_dir()?;
    let cache_git = crate::paths::cache_root_for(config.paths.cache.as_deref(), &cwd)?.join("git");
    let (mut base_lock, local_lock) = super::sync::load_locks(&cwd)?;

    if revoke {
        let source = source.ok_or_else(|| {
            Error::Config("`phora trust --revoke` needs a source name".to_owned())
        })?;
        return revoke_source_hooks(&cwd, base_lock.as_mut(), local_lock.as_ref(), source);
    }

    if let Some(path) = show {
        let (source, url, commit) = resolve_show_target(base_lock.as_ref(), source)?;
        let backend = GitBackend::new(cache_git.clone());
        for line in render_show(&backend, &source, &url, &commit, Path::new(path))? {
            println!("{line}");
        }
        return Ok(());
    }

    let candidates = discover_candidates(&config, &cwd, base_lock.as_ref(), source)?;
    let mut differ = TrustDiff::open(&cache_git, base_lock.as_ref());
    differ.attach_surface(&config, base_lock.as_ref());
    if list || !std::io::stdin().is_terminal() {
        print_candidates(&candidates, &differ);
        return Ok(());
    }

    let approved = approve(&candidates, &differ, &StdinConfirm);
    persist_approvals(&cwd, &mut base_lock, local_lock.as_ref(), approved)
}

/// Renders the file-level diff between a candidate's commit and the commit a prior approval trusted.
struct TrustDiff {
    backend: Option<GitBackend>,
    trusted_hooks: Vec<TrustedHook>,
    dep_urls: Vec<String>,
    surface: Option<(ResolvedGraph, Lock)>,
}

impl TrustDiff {
    fn open(cache_git: &std::path::Path, base_lock: Option<&Lock>) -> Self {
        let backend = Some(GitBackend::new(cache_git.to_path_buf()));
        let (trusted_hooks, dep_urls) = base_lock.map_or_else(
            || (Vec::new(), Vec::new()),
            |lock| {
                let mut urls: Vec<String> = lock.sources.iter().map(|s| s.git.clone()).collect();
                urls.sort_unstable();
                urls.dedup();
                (lock.trusted_hooks.clone(), urls)
            },
        );
        Self {
            backend,
            trusted_hooks,
            dep_urls,
            surface: None,
        }
    }

    fn attach_surface(&mut self, config: &crate::config::Config, base_lock: Option<&Lock>) {
        let Some(lock) = base_lock else { return };
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let Ok(parsed) = config.parsed_sources() else {
            return;
        };
        if let Ok(graph) = crate::sync::transitive::resolve_transitive_graph_offline(
            config, &parsed, backend, lock,
        ) {
            self.surface = Some((graph, lock.clone()));
        }
    }

    fn lines_for(&self, candidate: &CandidateHookRecord) -> Vec<String> {
        let candidate_key = commit_stable_hook_key(&candidate.hook_id);
        let priors: Vec<&TrustedHook> = self
            .trusted_hooks
            .iter()
            .filter(|h| {
                h.source == candidate.source
                    && commit_stable_hook_key(&h.hook_id) == candidate_key
                    && !h.commit.is_empty()
            })
            .collect();
        if priors.is_empty() {
            return self.first_trust_lines(candidate);
        }
        if candidate.commit.is_empty() {
            return vec![
                "  diff unavailable — candidate has no recorded commit; run `phora sync` first"
                    .to_owned(),
            ];
        }
        if priors.iter().any(|p| p.commit == candidate.commit) {
            return vec!["  unchanged since last trusted".to_owned()];
        }
        self.changed_paths(candidate, &priors)
    }

    fn changed_paths(
        &self,
        candidate: &CandidateHookRecord,
        priors: &[&TrustedHook],
    ) -> Vec<String> {
        let Some(backend) = &self.backend else {
            return vec![diff_unavailable()];
        };
        let name = crate::kernel::SourceName::trusted(candidate.source.clone());
        // The dep is the mirror holding BOTH commits; this resolves nested deps and discriminates colliding (source, stripped-key) pairs that the lock can't.
        for url in &self.dep_urls {
            for prior in priors {
                if let Ok(paths) =
                    backend.file_diff_between(&name, url, &prior.commit, &candidate.commit)
                {
                    let mut out = vec![format!(
                        "  changed since last trusted ({}..{}):",
                        short(&prior.commit),
                        short(&candidate.commit)
                    )];
                    out.extend(paths.into_iter().map(|p| format!("    {p}")));
                    return out;
                }
            }
        }
        vec![diff_unavailable()]
    }

    fn first_trust_lines(&self, candidate: &CandidateHookRecord) -> Vec<String> {
        if candidate.commit.is_empty() {
            return vec![surface_unavailable()];
        }
        let (Some((graph, lock)), Some(backend)) = (&self.surface, &self.backend) else {
            return vec![surface_unavailable()];
        };
        let Some(target) = graph.targets.iter().find(|t| {
            candidate.hook_id == format!("{}#on_change", t.name)
                || candidate
                    .hook_id
                    .starts_with(&format!("{}#on_change#", t.name))
        }) else {
            return vec![surface_unavailable()];
        };
        match graph.composed_files(&target.name, backend, lock) {
            Ok(paths) if !paths.is_empty() => {
                let mut out = vec![format!("  composed files at {}:", short(&candidate.commit))];
                out.extend(paths.into_iter().map(|p| format!("    {p}")));
                out
            }
            _ => vec![surface_unavailable()],
        }
    }
}

fn surface_unavailable() -> String {
    "  composed surface unavailable — run `phora sync` first".to_owned()
}

/// Drops the `<instance-key>%<counter>%` prefix, whose `instance.stable_key()` folds in the resolved commit, so a hook matches across commits.
fn commit_stable_hook_key(hook_id: &str) -> String {
    let (head, tail) = hook_id.split_once('#').unwrap_or((hook_id, ""));
    let dep_target = head.rsplit('%').next().unwrap_or(head);
    format!("{dep_target}#{tail}")
}

fn diff_unavailable() -> String {
    "  diff unavailable — run `phora sync` first".to_owned()
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn persist_approvals(
    cwd: &std::path::Path,
    base_lock: &mut Option<Lock>,
    local_lock: Option<&Lock>,
    approved: Vec<CandidateHookRecord>,
) -> Result<()> {
    if approved.is_empty() {
        println!("phora: no transitive hooks approved");
        return Ok(());
    }
    let lock = base_lock.get_or_insert_with(empty_lock);
    let now = chrono::Utc::now().to_rfc3339();
    for candidate in approved {
        lock.trusted_hooks.push(TrustedHook {
            dep_instance: candidate.dep_instance,
            hook_id: candidate.hook_id,
            preimage: candidate.preimage,
            approved_at: now.clone(),
            source: candidate.source,
            commit: candidate.commit,
        });
    }
    super::sync::write_locks(cwd, lock, local_lock)?;
    Ok(())
}

fn revoke_source_hooks(
    cwd: &std::path::Path,
    base_lock: Option<&mut Lock>,
    local_lock: Option<&Lock>,
    source: &str,
) -> Result<()> {
    let Some(lock) = base_lock else {
        println!("phora: no lock — nothing to revoke for `{source}`");
        return Ok(());
    };
    let before = lock.trusted_hooks.len();
    lock.trusted_hooks.retain(|h| h.source != source);
    let removed = before - lock.trusted_hooks.len();
    super::sync::write_locks(cwd, lock, local_lock)?;
    println!("phora: revoked {removed} transitive hook approval(s) for `{source}`");
    Ok(())
}

/// Resolves the `(remote_url, commit)` a `--show` should read, entirely from the lock (offline).
/// The current candidate commit for `source` wins over a prior trusted commit; a single distinct
/// non-empty commit is required, so an unsynced or ambiguous source refuses rather than guesses.
pub(super) fn resolve_show_target(
    lock: Option<&Lock>,
    source: Option<&str>,
) -> Result<(String, String, String)> {
    let source = source
        .ok_or_else(|| Error::Config("`phora trust --show` needs a source name".to_owned()))?;
    let lock = lock.ok_or_else(|| {
        Error::Config(format!(
            "no commit recorded for `{source}` — run `phora sync` first"
        ))
    })?;

    let mut records: Vec<(&str, &str)> = lock
        .candidate_hooks
        .iter()
        .filter(|c| c.source == source)
        .map(|c| (c.commit.as_str(), c.dep_instance.as_str()))
        .collect();
    if records.is_empty() {
        records = lock
            .trusted_hooks
            .iter()
            .filter(|h| h.source == source)
            .map(|h| (h.commit.as_str(), h.dep_instance.as_str()))
            .collect();
    }

    let mut distinct: Vec<(&str, &str)> = Vec::new();
    for (commit, instance) in records {
        if !commit.is_empty() && !distinct.iter().any(|(c, _)| *c == commit) {
            distinct.push((commit, instance));
        }
    }

    match distinct.as_slice() {
        [] => Err(Error::Config(format!(
            "no commit recorded for `{source}` — run `phora sync` first"
        ))),
        [(commit, instance)] => {
            let url = locked_url(lock, instance, source)?;
            Ok((source.to_owned(), url, (*commit).to_owned()))
        }
        many => {
            let commits = many
                .iter()
                .map(|(c, _)| short(c))
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::Config(format!(
                "`{source}` has several recorded commits ({commits}); sync to pin one before --show"
            )))
        }
    }
}

/// The remote URL of the `LockedSource` for `instance` (a transitive node), falling back to the
/// consumer source named `source`.
fn locked_url(lock: &Lock, instance: &str, source: &str) -> Result<String> {
    lock.sources
        .iter()
        .find(|s| s.instance.as_deref() == Some(instance))
        .or_else(|| lock.sources.iter().find(|s| s.name == source))
        .map(|s| s.git.clone())
        .ok_or_else(|| {
            Error::Config(format!(
                "no locked remote for `{source}` — run `phora sync` first"
            ))
        })
}

/// Renders a dep `path` at `commit` from the mirror, offline: a UTF-8 file as its text lines, a
/// directory as an ls-style listing (subdirectories suffixed `/`), refusing binary content and
/// reporting an absent path. Dispatch is by backend outcome, never by matching error strings.
pub(super) fn render_show(
    backend: &dyn SourceBackend,
    source: &str,
    url: &str,
    commit: &str,
    path: &Path,
) -> Result<Vec<String>> {
    let name = crate::kernel::SourceName::trusted(source.to_owned());
    match backend.read_file_at(&name, url, commit, path) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
            Err(_) => Err(Error::Source(format!(
                "{} at {} is not UTF-8 text — binary content is not shown",
                path.display(),
                short(commit)
            ))),
        },
        Err(_) => match backend.list_tree_at(&name, url, commit, path) {
            Ok(entries) => Ok(entries
                .into_iter()
                .map(|e| {
                    if e.is_dir {
                        format!("{}/", e.name)
                    } else {
                        e.name
                    }
                })
                .collect()),
            Err(SourceError::RootNotFound { .. }) => Err(Error::Source(format!(
                "{} is absent at {} in `{source}`",
                path.display(),
                short(commit)
            ))),
            Err(_) => Err(Error::Source(format!(
                "`{}` cannot be shown — its commit `{}` is not in the mirror; run `phora sync` first",
                path.display(),
                short(commit)
            ))),
        },
    }
}

fn discover_candidates(
    config: &crate::config::Config,
    cwd: &std::path::Path,
    base_lock: Option<&Lock>,
    source: Option<&str>,
) -> Result<Vec<CandidateHookRecord>> {
    if let Some(lock) = base_lock {
        let recorded = match source {
            Some(name) => candidates_for_source(lock, name),
            None => lock.candidate_hooks.clone(),
        };
        if !recorded.is_empty() {
            return Ok(recorded);
        }
    }
    match source {
        Some(name) => discover_via_fetch(config, cwd, name),
        None => Ok(Vec::new()),
    }
}

fn discover_via_fetch(
    config: &crate::config::Config,
    cwd: &std::path::Path,
    name: &str,
) -> Result<Vec<CandidateHookRecord>> {
    use crate::source::{GitBackend, Protocol as SourceProtocol, SourceError};

    let sources = config.parsed_sources()?;
    let Some(source) = sources.get(name) else {
        return Ok(Vec::new());
    };
    if source.mode() != crate::config::SourceMode::Git {
        return Ok(Vec::new());
    }
    let protocol = source
        .protocol()
        .or(config.protocol)
        .unwrap_or(SourceProtocol::Https);
    let remote = source
        .resolved_remote(&config.hosts, protocol)
        .map_err(|e| Error::Config(format!("source `{name}`: {e}")))?;
    let git_dir = crate::paths::cache_root_for(config.paths.cache.as_deref(), cwd)?.join("git");
    let backend = GitBackend::new(git_dir);
    let source_name = crate::kernel::SourceName::trusted(name.to_owned());
    let bytes = match backend.fetch_root_manifest(&source_name, &remote, &source.refspec()) {
        Ok(bytes) => bytes,
        Err(SourceError::FileAbsent { .. }) => return Ok(Vec::new()),
        Err(e) => return Err(Error::Source(format!("inspect `{name}`: {e}"))),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| Error::Config(format!("source `{name}`: phora.toml is not utf8: {e}")))?;
    let manifest = TransitiveManifest::parse(text)?;
    Ok(fetched_candidates(name, &manifest))
}

fn fetched_candidates(name: &str, manifest: &TransitiveManifest) -> Vec<CandidateHookRecord> {
    let Some(hooks) = manifest.hooks() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(targets) = hooks.as_table() {
        for (target, block) in targets {
            let Some(on_change) = block.get("on_change") else {
                continue;
            };
            for command in hook_commands(on_change) {
                out.push(CandidateHookRecord {
                    dep_instance: name.to_owned(),
                    hook_id: format!("{name}#{target}#on_change"),
                    preimage: UNRESOLVED_PREIMAGE.to_owned(),
                    command,
                    source: name.to_owned(),
                    commit: String::new(),
                });
            }
        }
    }
    out
}

fn hook_commands(value: &toml::Value) -> Vec<String> {
    match value {
        toml::Value::String(run) => vec![run.clone()],
        toml::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                toml::Value::String(run) => Some(run.clone()),
                toml::Value::Table(table) => table
                    .get("run")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
                _ => None,
            })
            .collect(),
        toml::Value::Table(table) => table
            .get("run")
            .and_then(toml::Value::as_str)
            .map(|run| vec![run.to_owned()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Candidates recorded under `source` (the consumer-facing import name). An unknown source scopes
/// to EMPTY — never to every dep's candidates — so `--revoke`/`--list` cannot over-reach.
fn candidates_for_source(lock: &Lock, source: &str) -> Vec<CandidateHookRecord> {
    lock.candidate_hooks
        .iter()
        .filter(|c| c.source == source)
        .cloned()
        .collect()
}

fn print_candidates(candidates: &[CandidateHookRecord], differ: &TrustDiff) {
    if candidates.is_empty() {
        println!("phora: no transitive hooks");
        return;
    }
    for candidate in candidates {
        println!("hook {} (on_change)", candidate.hook_id);
        println!("  command: {}", candidate.command);
        println!("  preimage: {}", preimage_display(&candidate.preimage));
        println!("  env: PHORA_TARGET=<composed target path>");
        println!(
            "  note: the hook inherits the FULL process environment, not only the PHORA_* variables"
        );
        for line in differ.lines_for(candidate) {
            println!("{line}");
        }
    }
}

fn preimage_display(preimage: &str) -> String {
    if preimage == UNRESOLVED_PREIMAGE {
        "(preimage resolved on first sync — run sync, then trust)".to_owned()
    } else {
        preimage.to_owned()
    }
}

/// Per-candidate trust confirmation; lets a test drive approve→persist without a real TTY.
trait Confirm {
    fn confirm(&self, candidate: &CandidateHookRecord) -> bool;
}

struct StdinConfirm;

impl Confirm for StdinConfirm {
    fn confirm(&self, candidate: &CandidateHookRecord) -> bool {
        crate::sync::hooks::prompt_yes_on_stdin(&format!(
            "phora: trust `{}` (runs `{}`)? [y/N] ",
            candidate.hook_id, candidate.command
        ))
    }
}

fn approve(
    candidates: &[CandidateHookRecord],
    differ: &TrustDiff,
    confirm: &dyn Confirm,
) -> Vec<CandidateHookRecord> {
    let mut approved = Vec::new();
    for candidate in candidates {
        if candidate.preimage == UNRESOLVED_PREIMAGE {
            continue;
        }
        for line in differ.lines_for(candidate) {
            eprintln!("{line}");
        }
        if confirm.confirm(candidate) {
            approved.push(candidate.clone());
        }
    }
    approved
}

fn empty_lock() -> Lock {
    Lock {
        version: crate::lock::LOCK_SCHEMA_VERSION,
        sources: Vec::new(),
        trusted_hooks: Vec::new(),
        candidate_hooks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source: &str, preimage: &str) -> CandidateHookRecord {
        CandidateHookRecord {
            dep_instance: format!("{source}-instance"),
            hook_id: format!("{source}#editor#on_change"),
            preimage: preimage.to_owned(),
            command: "touch sentinel".to_owned(),
            source: source.to_owned(),
            commit: String::new(),
        }
    }

    fn no_diff() -> TrustDiff {
        TrustDiff::open(std::path::Path::new("/nonexistent/git"), None)
    }

    struct Canned(bool);

    impl Confirm for Canned {
        fn confirm(&self, _candidate: &CandidateHookRecord) -> bool {
            self.0
        }
    }

    #[test]
    fn offline_transitive_resolve_is_reachable_from_the_trust_layer() {
        use crate::sync::transitive::{
            ComposedTarget, ResolvedGraph, resolve_transitive_graph_offline,
        };

        let config =
            crate::config::Config::parse("version = 1\n\n[targets.x]\npath = \"/home/me/x\"\n")
                .expect("import-free config parses");
        let parsed: std::collections::BTreeMap<String, crate::config::ParsedSource> =
            std::collections::BTreeMap::new();
        let lock = empty_lock();
        let git_dir = tempfile::TempDir::new().expect("temp git dir");
        let backend = crate::source::GitBackend::new(git_dir.path().to_path_buf());

        let graph: ResolvedGraph =
            resolve_transitive_graph_offline(&config, &parsed, &backend, &lock)
                .expect("the offline transitive resolve must be callable from the CLI/trust layer");

        let names: Vec<&str> = graph
            .targets
            .iter()
            .map(|t: &ComposedTarget| t.name.as_str())
            .collect();
        assert!(
            names.is_empty(),
            "REACHABILITY: an import-free config composes no transitive targets, reached offline with no fetch — and the trust layer can see resolve_transitive_graph_offline, ResolvedGraph, ComposedTarget, and the `name` field"
        );
    }

    #[test]
    fn approve_yes_yields_a_record_matching_the_candidate_addressing() {
        let cand = candidate("depA", "blake3:real");

        let approved = approve(std::slice::from_ref(&cand), &no_diff(), &Canned(true));

        assert_eq!(
            approved.len(),
            1,
            "a yes confirmation must approve the hook"
        );
        let got = &approved[0];
        assert_eq!(got.source, cand.source, "approval keeps the source scope");
        assert_eq!(got.dep_instance, cand.dep_instance);
        assert_eq!(got.hook_id, cand.hook_id);
        assert_eq!(
            got.preimage, cand.preimage,
            "the approval must pin the candidate's commit-bound preimage verbatim"
        );
    }

    #[test]
    fn approve_no_persists_nothing() {
        let cand = candidate("depA", "blake3:real");

        let approved = approve(std::slice::from_ref(&cand), &no_diff(), &Canned(false));

        assert!(
            approved.is_empty(),
            "anti-TOFU: declining the prompt must approve nothing"
        );
    }

    #[test]
    fn approve_skips_an_unresolved_preimage_candidate() {
        let cand = candidate("depA", UNRESOLVED_PREIMAGE);

        let approved = approve(std::slice::from_ref(&cand), &no_diff(), &Canned(true));

        assert!(
            approved.is_empty(),
            "a discovery-only candidate has no real preimage to pin; a yes must NOT mint an \
             approval that can never match the commit-bound sync preimage"
        );
    }

    #[test]
    fn candidates_for_source_scopes_to_the_named_source_only() {
        let lock = Lock {
            candidate_hooks: vec![candidate("depA", "blake3:a"), candidate("depB", "blake3:b")],
            ..empty_lock()
        };

        let scoped = candidates_for_source(&lock, "depA");

        assert_eq!(scoped.len(), 1, "only depA's candidate is in scope");
        assert_eq!(scoped[0].source, "depA");
    }

    #[test]
    fn candidates_for_unknown_source_is_empty_never_everything() {
        let lock = Lock {
            candidate_hooks: vec![candidate("depA", "blake3:a"), candidate("depB", "blake3:b")],
            ..empty_lock()
        };

        let scoped = candidates_for_source(&lock, "nonesuch");

        assert!(
            scoped.is_empty(),
            "an unknown source must scope to EMPTY, never fall back to every dep's candidates"
        );
    }

    use crate::source::{SourceBackend, SourceError, TreeEntry};
    use std::path::Path;

    /// What the mocked mirror reader should hand back for `<path>` at the pinned commit.
    enum FileOutcome {
        Bytes(Vec<u8>),
        Absent,
        NotRegular,
        MirrorError,
    }

    enum TreeOutcome {
        Entries(Vec<TreeEntry>),
        Absent,
        MirrorError,
    }

    /// A `SourceBackend` whose only live methods are the two mirror reads `--show` drives;
    /// every other port method is irrelevant to the renderer and must never be touched.
    struct ShowBackend {
        file: FileOutcome,
        tree: TreeOutcome,
    }

    impl SourceBackend for ShowBackend {
        fn read_file_at(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
            commit: &str,
            path: &Path,
        ) -> std::result::Result<Vec<u8>, SourceError> {
            match &self.file {
                FileOutcome::Bytes(bytes) => Ok(bytes.clone()),
                FileOutcome::Absent => Err(SourceError::FileAbsent {
                    source_name: "mydeps".to_owned(),
                    commit: commit.to_owned(),
                    path: path.to_path_buf(),
                }),
                FileOutcome::NotRegular => Err(SourceError::Source(format!(
                    "{} is not a regular file",
                    path.display()
                ))),
                FileOutcome::MirrorError => Err(SourceError::Source(format!(
                    "open mirror for {commit}: missing"
                ))),
            }
        }

        fn list_tree_at(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
            commit: &str,
            path: &Path,
        ) -> std::result::Result<Vec<TreeEntry>, SourceError> {
            match &self.tree {
                TreeOutcome::Entries(entries) => Ok(entries.clone()),
                TreeOutcome::Absent => Err(SourceError::RootNotFound {
                    root: path.to_path_buf(),
                }),
                TreeOutcome::MirrorError => Err(SourceError::Source(format!(
                    "commit {commit} in mirror: missing"
                ))),
            }
        }

        fn fetch(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
        ) -> std::result::Result<(), SourceError> {
            unimplemented!("`--show` is offline; it must not fetch")
        }

        fn resolve(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
            _refspec: &crate::config::Refspec,
        ) -> std::result::Result<String, SourceError> {
            unimplemented!("`--show` reads a pinned commit; it must not resolve refs")
        }

        fn commit_time(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
            _commit: &str,
        ) -> std::result::Result<u64, SourceError> {
            unimplemented!()
        }

        fn export_artifact(
            &self,
            _req: &crate::source::ExportRequest<'_>,
        ) -> std::result::Result<crate::source::ExportResult, SourceError> {
            unimplemented!()
        }

        fn compute_digest(
            &self,
            _source: &crate::kernel::SourceName,
            _url: &str,
            _commit: &str,
            _root: Option<&Path>,
            _include: &[String],
            _exclude: &[String],
        ) -> std::result::Result<String, SourceError> {
            unimplemented!()
        }
    }

    const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const COMMIT_ONE: &str = "111111111111111111111111111111111111aaaa";
    const COMMIT_TWO: &str = "222222222222222222222222222222222222bbbb";

    fn cand_commit(source: &str, instance: &str, commit: &str) -> CandidateHookRecord {
        CandidateHookRecord {
            dep_instance: instance.to_owned(),
            hook_id: format!("{source}#editor#on_change"),
            preimage: "blake3:p".to_owned(),
            command: "touch sentinel".to_owned(),
            source: source.to_owned(),
            commit: commit.to_owned(),
        }
    }

    fn trusted_commit(source: &str, instance: &str, commit: &str) -> TrustedHook {
        TrustedHook {
            dep_instance: instance.to_owned(),
            hook_id: format!("{source}#editor#on_change"),
            preimage: "blake3:p".to_owned(),
            approved_at: "2026-01-01T00:00:00Z".to_owned(),
            source: source.to_owned(),
            commit: commit.to_owned(),
        }
    }

    fn locked_dep(name: &str, instance: &str, git: &str) -> crate::lock::LockedSource {
        crate::lock::LockedSource {
            name: name.to_owned(),
            git: git.to_owned(),
            resolved: "main".to_owned(),
            commit: "c0ffee".to_owned(),
            digest: "blake3:artifact".to_owned(),
            config_digest: "blake3:cfg".to_owned(),
            r#ref: None,
            instance: Some(instance.to_owned()),
        }
    }

    #[test]
    fn render_show_prints_utf8_file_contents() {
        let backend = ShowBackend {
            file: FileOutcome::Bytes(b"hello\nworld\n".to_vec()),
            tree: TreeOutcome::Absent,
        };

        let lines = render_show(
            &backend,
            "mydeps",
            "https://github.com/dep/mydeps.git",
            COMMIT_A,
            Path::new("README.md"),
        )
        .expect("a tracked UTF-8 file must render its contents");

        let body = lines.join("\n");
        assert!(
            body.contains("hello") && body.contains("world"),
            "the file's text must be printed verbatim, got: {body:?}"
        );
    }

    #[test]
    fn render_show_refuses_non_utf8_binary_content() {
        let backend = ShowBackend {
            file: FileOutcome::Bytes(vec![0xff, 0xfe, 0x00]),
            tree: TreeOutcome::Absent,
        };

        let err = render_show(
            &backend,
            "mydeps",
            "https://github.com/dep/mydeps.git",
            COMMIT_A,
            Path::new("logo.png"),
        )
        .expect_err("binary content must be refused, never dumped as raw bytes");

        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("utf-8") || msg.contains("utf8") || msg.contains("binary"),
            "the refusal must name the binary / non-UTF-8 reason, got: {msg:?}"
        );
        assert!(
            !msg.contains('\u{0}'),
            "the refusal must not leak the raw NUL byte of the content"
        );
    }

    #[test]
    fn render_show_errors_when_path_is_absent_at_commit() {
        let backend = ShowBackend {
            file: FileOutcome::Absent,
            tree: TreeOutcome::Absent,
        };

        let err = render_show(
            &backend,
            "mydeps",
            "https://github.com/dep/mydeps.git",
            COMMIT_A,
            Path::new("missing/thing.txt"),
        )
        .expect_err("a path absent at the commit must error, not print nothing");

        let msg = err.to_string();
        assert!(
            msg.contains("missing/thing.txt"),
            "the not-found error must name the requested path, got: {msg:?}"
        );
        assert!(
            msg.to_lowercase().contains("absent"),
            "a genuinely-absent path must report absence, got: {msg:?}"
        );
    }

    #[test]
    fn render_show_directs_to_sync_when_the_mirror_or_commit_is_missing() {
        let backend = ShowBackend {
            file: FileOutcome::MirrorError,
            tree: TreeOutcome::MirrorError,
        };

        let err = render_show(
            &backend,
            "mydeps",
            "https://github.com/dep/mydeps.git",
            COMMIT_A,
            Path::new("README.md"),
        )
        .expect_err("a missing mirror / commit must error, not pretend the path is absent");

        let msg = err.to_string();
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("sync"),
            "a commit not yet in the mirror must direct the user to run `phora sync`, got: {msg:?}"
        );
        assert!(
            !lower.contains("absent"),
            "a missing mirror / commit is not a genuinely-absent path and must not be reported as absent, got: {msg:?}"
        );
    }

    #[test]
    fn render_show_lists_direct_directory_entries_ls_style() {
        let backend = ShowBackend {
            file: FileOutcome::NotRegular,
            tree: TreeOutcome::Entries(vec![
                TreeEntry {
                    name: "a.txt".to_owned(),
                    is_dir: false,
                },
                TreeEntry {
                    name: "sub".to_owned(),
                    is_dir: true,
                },
                TreeEntry {
                    name: "weird name.txt".to_owned(),
                    is_dir: false,
                },
            ]),
        };

        let lines = render_show(
            &backend,
            "mydeps",
            "https://github.com/dep/mydeps.git",
            COMMIT_A,
            Path::new("d"),
        )
        .expect("a directory path must list its direct children");

        let body = lines.join("\n");
        let file_idx = lines
            .iter()
            .position(|l| l.contains("a.txt"))
            .unwrap_or_else(|| panic!("the listing must include the file `a.txt`, got: {body:?}"));
        let dir_idx = lines
            .iter()
            .position(|l| l.contains("sub"))
            .unwrap_or_else(|| {
                panic!("the listing must include the directory `sub`, got: {body:?}")
            });
        let file_line = &lines[file_idx];
        let dir_line = &lines[dir_idx];
        assert!(
            dir_line.trim_end().ends_with('/'),
            "an ls-style directory entry must be marked with a trailing slash, got: {dir_line:?}"
        );
        assert!(
            !file_line.trim_end().ends_with('/'),
            "a regular-file entry must NOT carry the directory slash, got: {file_line:?}"
        );
        assert!(
            file_idx < dir_idx,
            "the renderer must preserve the order `list_tree_at` returned (a.txt before sub), got: {body:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("weird name.txt")),
            "the renderer must emit each returned entry verbatim, including names with spaces, got: {body:?}"
        );
    }

    #[test]
    fn resolve_show_target_requires_a_source_name() {
        let err = resolve_show_target(None, None)
            .expect_err("`--show` without a source must error cleanly, mirroring `--revoke`");

        assert!(
            matches!(err, Error::Config(_)),
            "the missing-source failure must be a clean Error::Config, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("--show") && msg.contains("source"),
            "the message must name `--show` and the missing source, got: {msg:?}"
        );
    }

    #[test]
    fn resolve_show_target_prefers_the_candidate_commit_over_trusted() {
        let lock = Lock {
            candidate_hooks: vec![cand_commit("mydeps", "inst", COMMIT_A)],
            trusted_hooks: vec![trusted_commit("mydeps", "inst", COMMIT_B)],
            sources: vec![locked_dep(
                "mydeps",
                "inst",
                "https://github.com/dep/mydeps.git",
            )],
            ..empty_lock()
        };

        let (_source, url, commit) = resolve_show_target(Some(&lock), Some("mydeps"))
            .expect("a candidate commit must resolve a show target");

        assert_eq!(
            commit, COMMIT_A,
            "the current candidate commit must win over the prior trusted commit"
        );
        assert_eq!(
            url, "https://github.com/dep/mydeps.git",
            "the remote URL must come from the LockedSource matching the candidate's dep_instance"
        );
    }

    #[test]
    fn resolve_show_target_with_no_commit_directs_to_sync() {
        let lock = Lock {
            candidate_hooks: vec![cand_commit("mydeps", "inst", "")],
            trusted_hooks: Vec::new(),
            sources: vec![locked_dep(
                "mydeps",
                "inst",
                "https://github.com/dep/mydeps.git",
            )],
            ..empty_lock()
        };

        let err = resolve_show_target(Some(&lock), Some("mydeps"))
            .expect_err("an empty/absent dep commit cannot be shown offline");

        assert!(
            err.to_string().contains("sync"),
            "the user must be told to run `phora sync`, got: {err}"
        );
    }

    #[test]
    fn resolve_show_target_refuses_to_guess_between_distinct_commits() {
        let lock = Lock {
            candidate_hooks: vec![
                cand_commit("mydeps", "inst", COMMIT_ONE),
                cand_commit("mydeps", "inst", COMMIT_TWO),
            ],
            sources: vec![locked_dep(
                "mydeps",
                "inst",
                "https://github.com/dep/mydeps.git",
            )],
            ..empty_lock()
        };

        let err = resolve_show_target(Some(&lock), Some("mydeps"))
            .expect_err("two distinct dep commits for one source must not be silently picked");

        let msg = err.to_string();
        assert!(
            msg.contains(&COMMIT_ONE[..12]) && msg.contains(&COMMIT_TWO[..12]),
            "the error must list BOTH candidate commits so the user can disambiguate, got: {msg:?}"
        );
    }
}
