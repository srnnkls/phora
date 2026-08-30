//! T027 public contract and boundary probes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const PROBE_SOURCE: &str = r#"
use std::num::NonZeroUsize;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use phora::config::Config;
use phora::projection::diagnostic::ProjectionWarning;
use phora::source::GitBackend;
use phora::sync::state::{
    ArtifactKey, ArtifactRecord, FileStateStore, RecordKind, StateStore,
};
use phora::sync::model::{ChangeSet, ConflictKind, ReconciliationPolicy, RemovalReason, SyncChange};
use phora::sync::{
    AppliedChange, Concurrency, Conflict, ConflictPolicy, ConflictResolver, HookOutcome,
    HookPolicy, LockSet, MovedPinPolicy, PrunePolicy, Resolution, SkippedChange, SourcePolicy,
    SyncOptions, SyncReport, SyncRequest, SyncStatus, SyncWarning, sync,
};

fn options(
    source_policy: SourcePolicy,
    conflict_policy: ConflictPolicy,
    prune_policy: PrunePolicy,
    hook_policy: HookPolicy,
    moved_pin_policy: MovedPinPolicy,
    jobs: Option<NonZeroUsize>,
) -> SyncOptions {
    SyncOptions {
        source_policy,
        conflict_policy,
        prune_policy,
        hook_policy,
        moved_pin_policy,
        concurrency: Concurrency { jobs },
    }
}

fn mapped(options: &SyncOptions) -> ReconciliationPolicy {
    options.into()
}

fn assert_policy_mapping_table() {
    let cases = [
        ("source locked", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("source refresh", options(SourcePolicy::Refresh, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("source frozen", options(SourcePolicy::Frozen, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("conflict refuse", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("conflict interactive", options(SourcePolicy::Locked, ConflictPolicy::ResolveInteractively, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("conflict overwrite", options(SourcePolicy::Locked, ConflictPolicy::Overwrite, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (true, false, false)),
        ("prune keep", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("prune remove", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::RemoveOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, true, false)),
        ("hooks all", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("hooks no transitive", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::NoTransitive, MovedPinPolicy::Seal, None), (false, false, false)),
        ("hooks none", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::None, MovedPinPolicy::Seal, None), (false, false, false)),
        ("moved pin seal", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("moved pin fast forward", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::FastForward, None), (false, false, true)),
        ("concurrency derived", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None), (false, false, false)),
        ("concurrency fixed", options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, NonZeroUsize::new(3)), (false, false, false)),
    ];
    for (name, options, expected) in cases {
        let actual = mapped(&options);
        assert_eq!((actual.force, actual.prune, actual.follow_moved_pin), expected, "mapping row {name}");
    }
}

fn assert_exact_public_shapes<'a>(request: SyncRequest<'a>, report: SyncReport) {
    let SyncRequest { base_config, local_config, locks, options: request_options, resolver } = request;
    let _: &'a Config = base_config;
    let _: Option<&'a Config> = local_config;
    let _: LockSet = locks;
    let _: SyncOptions = request_options;
    let _: Option<&'a dyn ConflictResolver> = resolver;

    let SyncOptions {
        source_policy,
        conflict_policy,
        prune_policy,
        hook_policy,
        moved_pin_policy,
        concurrency,
    } = options(SourcePolicy::Locked, ConflictPolicy::Refuse, PrunePolicy::KeepOrphans, HookPolicy::All, MovedPinPolicy::Seal, None);
    let _: SourcePolicy = source_policy;
    let _: ConflictPolicy = conflict_policy;
    let _: PrunePolicy = prune_policy;
    let _: HookPolicy = hook_policy;
    let _: MovedPinPolicy = moved_pin_policy;
    let _: Concurrency = concurrency;

    let SyncReport { locks, changes, applied, skipped, warnings, hook_outcomes, status } = report;
    let _: LockSet = locks;
    let _: ChangeSet = changes;
    let _: Vec<AppliedChange> = applied;
    let _: Vec<SkippedChange> = skipped;
    let _: Vec<SyncWarning> = warnings;
    let _: Vec<HookOutcome> = hook_outcomes;
    let _: SyncStatus = status;
}

struct SpyResolver { calls: AtomicUsize }
impl SpyResolver { fn new() -> Self { Self { calls: AtomicUsize::new(0) } } }
impl ConflictResolver for SpyResolver {
    fn resolve(&self, _: &Conflict) -> Resolution {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Resolution::Skip
    }
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git fixture command");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn assert_request_flow_returns_conflict_warning_and_remove_outcomes() {
    let temp = tempfile::TempDir::new().expect("temp fixture");
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let orphan_root = temp.path().join("orphan-root");
    std::fs::create_dir_all(source.join("editor")).expect("source tree");
    std::fs::write(source.join("editor/init.lua"), b"-- upstream\n").expect("source file");
    git(&source, &["init", "-b", "main", "."]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "Test"]);
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "fixture"]);

    std::fs::create_dir_all(target.join("editor")).expect("foreign target tree");
    std::fs::write(target.join("editor/init.lua"), b"-- foreign\n").expect("foreign file");
    std::fs::create_dir_all(&orphan_root).expect("orphan root");
    let orphan_path = orphan_root.join("obsolete");
    std::fs::write(&orphan_path, b"obsolete\n").expect("orphan file");

    let config = Config::parse(&format!(
        "version = 1\n\n[sources.editor-src]\ngit = \"{}\"\nbranch = \"main\"\n\n[targets.dest]\npath = \"{}\"\nlayout = \"flat\"\nsources = {{ editor-src = {{ take = [\"editor/**\", \"*.nomatch\"] }} }}\n",
        source.display(), target.display(),
    )).expect("config parses");
    let registry = FileStateStore::open(temp.path().join("state")).expect("registry");
    let orphan_key = ArtifactKey { target: "gone".into(), source: "old".into(), artifact: "obsolete".into() };
    registry.put_artifact(&ArtifactRecord {
        version: 1,
        key: orphan_key.clone(),
        source: "old".into(),
        commit: "deadbeef".into(),
        digest: "blake3:obsolete".into(),
        projected_at: "2026-01-01T00:00:00Z".into(),
        layout: "flat".into(),
        kind: RecordKind::File,
        allow_symlinks: false,
        preserve_executable: true,
        files: vec![],
        linked: false,
        history: false,
        vars_digest: None,
        deploy_root: Some(orphan_root.to_string_lossy().into_owned()),
        layout_separator: None,
    }).expect("seed orphan record");

    let spy = SpyResolver::new();
    let request = SyncRequest {
        base_config: &config,
        local_config: None,
        locks: LockSet::default(),
        options: options(
            SourcePolicy::Refresh,
            ConflictPolicy::ResolveInteractively,
            PrunePolicy::RemoveOrphans,
            HookPolicy::None,
            MovedPinPolicy::Seal,
            None,
        ),
        resolver: Some(&spy),
    };
    let backend = GitBackend::new(temp.path().join("cache"));
    let report = sync(&request, &backend, &registry).expect("request flow succeeds");

    assert_eq!(spy.calls.load(Ordering::SeqCst), 1, "request resolver must be consulted");
    assert!(matches!(report.skipped.as_slice(), [
        SkippedChange::Conflict { target, source, artifact, kind: ConflictKind::Foreign }
    ] if target == "dest" && source == "editor-src" && artifact == "editor"),
        "the exact foreign editor conflict must be returned as the resolver-skipped outcome: {:?}", report.skipped);
    assert!(matches!(report.warnings.as_slice(), [
        SyncWarning::Projection(ProjectionWarning::TakeNoMatchGlob(pattern))
    ] if pattern == "*.nomatch"),
        "the exact unmatched take pattern must be returned as a structured projection warning: {:?}", report.warnings);
    assert!(report.changes.changes.iter().any(|change| matches!(change,
        SyncChange::Remove { target, source, artifact, .. }
            if target == "gone" && source == "old" && artifact == "obsolete"
    )), "normal workspace reconciliation must expose the orphan Remove row");
    assert!(matches!(report.applied.as_slice(), [
        AppliedChange::Removed { target, source, artifact, reason: RemovalReason::Pruned }
    ] if target == "gone" && source == "old" && artifact == "obsolete"),
        "the same reconciled orphan Remove must be the sole applied mutation: {:?}", report.applied);
    assert!(!orphan_path.exists(), "the normal workspace apply pass must execute the Remove row");
    assert!(registry.artifact(&orphan_key).expect("read orphan record").is_none(), "Remove drops state");
}

fn main() {
    assert_policy_mapping_table();
    assert_request_flow_returns_conflict_warning_and_remove_outcomes();
    let _shape: for<'a> fn(SyncRequest<'a>, SyncReport) = assert_exact_public_shapes;
}
"#;

#[test]
fn public_request_mapping_report_and_workspace_flow_probe() {
    let temp = tempfile::TempDir::new().expect("create isolated contract probe");
    let manifest = format!(
        "[package]\nname = \"t027-contract-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nphora = {{ path = {:?} }}\ntempfile = \"3.27.0\"\n",
        root()
    );
    fs::write(temp.path().join("Cargo.toml"), manifest).expect("write probe manifest");
    fs::create_dir(temp.path().join("src")).expect("create probe source directory");
    fs::write(temp.path().join("src/main.rs"), PROBE_SOURCE).expect("write probe source");

    let output = Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--offline"])
        .current_dir(temp.path())
        .env(
            "CARGO_TARGET_DIR",
            root().join("target/t027-contract-probe"),
        )
        .output()
        .expect("run isolated T027 contract probe");
    assert!(
        output.status.success(),
        "the isolated public T027 API/behavior probe must compile and pass; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Ident(String),
    Punct(char),
}

fn lex(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r' && bytes.get(i + 1).is_some_and(|b| *b == b'"' || *b == b'#') {
            let mut hashes = 0;
            let mut j = i + 1;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) != Some(&b'"') {
                tokens.push(Token::Ident("r".into()));
                i += 1;
                continue;
            }
            j += 1;
            while j < bytes.len() {
                if bytes[j] == b'"' && (0..hashes).all(|n| bytes.get(j + 1 + n) == Some(&b'#')) {
                    i = j + 1 + hashes;
                    break;
                }
                j += 1;
            }
            if j == bytes.len() {
                i = j;
            }
        } else if bytes[i] == b'\''
            && (bytes.get(i + 1) == Some(&b'\\') || bytes.get(i + 2) == Some(&b'\''))
        {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token::Ident(source[start..i].to_owned()));
        } else {
            if !bytes[i].is_ascii_whitespace() {
                tokens.push(Token::Punct(char::from(bytes[i])));
            }
            i += 1;
        }
    }
    tokens
}

fn ident(token: Option<&Token>) -> Option<&str> {
    match token {
        Some(Token::Ident(value)) => Some(value),
        _ => None,
    }
}

fn punct(token: Option<&Token>, expected: char) -> bool {
    token == Some(&Token::Punct(expected))
}

fn cfg_test_only(tokens: &[Token]) -> bool {
    let Some(head) = ident(tokens.first()) else {
        return false;
    };
    if matches!(head, "test" | "doctest") {
        return true;
    }
    if !matches!(head, "all" | "any") || !punct(tokens.get(1), '(') {
        return false;
    }
    let mut args = Vec::new();
    let mut start = 2;
    let mut depth = 0usize;
    for i in 2..tokens.len() {
        if punct(tokens.get(i), '(') {
            depth += 1;
        } else if punct(tokens.get(i), ')') {
            if depth == 0 {
                args.push(&tokens[start..i]);
                break;
            }
            depth -= 1;
        } else if punct(tokens.get(i), ',') && depth == 0 {
            args.push(&tokens[start..i]);
            start = i + 1;
        }
    }
    match head {
        "all" => args.into_iter().any(cfg_test_only),
        "any" => !args.is_empty() && args.into_iter().all(cfg_test_only),
        _ => false,
    }
}

fn production_tokens(source: &str) -> Vec<Token> {
    let tokens = lex(source);
    let mut production = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let cfg_attribute = punct(tokens.get(i), '#')
            && punct(tokens.get(i + 1), '[')
            && ident(tokens.get(i + 2)) == Some("cfg")
            && punct(tokens.get(i + 3), '(');
        if !cfg_attribute {
            production.push(tokens[i].clone());
            i += 1;
            continue;
        }

        let mut depth = 0usize;
        let Some(close_paren) = (i + 3..tokens.len()).find(|&index| {
            if punct(tokens.get(index), '(') {
                depth += 1;
            } else if punct(tokens.get(index), ')') {
                depth -= 1;
                return depth == 0;
            }
            false
        }) else {
            production.push(tokens[i].clone());
            i += 1;
            continue;
        };
        let test_only = cfg_test_only(&tokens[i + 4..close_paren]);
        if !test_only || !punct(tokens.get(close_paren + 1), ']') {
            production.extend_from_slice(&tokens[i..=close_paren + 1]);
            i = close_paren + 2;
            continue;
        }

        i = close_paren + 2;
        let Some(boundary) = (i..tokens.len())
            .find(|&index| punct(tokens.get(index), '{') || punct(tokens.get(index), ';'))
        else {
            break;
        };
        if punct(tokens.get(boundary), ';') {
            i = boundary + 1;
            continue;
        }
        let mut depth = 0usize;
        i = boundary;
        while i < tokens.len() {
            if punct(tokens.get(i), '{') {
                depth += 1;
            } else if punct(tokens.get(i), '}') {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            }
            i += 1;
        }
    }
    production
}

fn alias_after(tokens: &[Token], index: usize, fallback: &str) -> String {
    if ident(tokens.get(index)) == Some("as") {
        ident(tokens.get(index + 1)).unwrap_or(fallback).to_owned()
    } else {
        fallback.to_owned()
    }
}

fn imported_aliases(
    tokens: &[Token],
) -> (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
) {
    let mut stderr_macros = BTreeSet::from(["eprintln".to_owned()]);
    let mut process_modules = BTreeSet::new();
    let mut exit_functions = BTreeSet::new();
    let mut std_modules = BTreeSet::from(["std".to_owned()]);
    let mut alias_edges = Vec::new();
    for i in 0..tokens.len() {
        let statement_start = tokens[..i]
            .iter()
            .rposition(|token| token == &Token::Punct(';'))
            .map_or(0, |semicolon| semicolon + 1);
        let statement_end = tokens[i..]
            .iter()
            .position(|token| token == &Token::Punct(';'))
            .map_or(tokens.len(), |offset| i + offset);
        let statement = &tokens[statement_start..statement_end];
        let in_use = statement
            .iter()
            .any(|token| ident(Some(token)) == Some("use"));
        if in_use
            && let (Some(source), Some("as"), Some(alias)) = (
                ident(tokens.get(i)),
                ident(tokens.get(i + 1)),
                ident(tokens.get(i + 2)),
            )
        {
            alias_edges.push((source.to_owned(), alias.to_owned()));
        }
        let imports_std = statement
            .iter()
            .any(|token| ident(Some(token)) == Some("std"));
        if !in_use || !imports_std {
            continue;
        }
        match ident(tokens.get(i)) {
            Some("std") if ident(tokens.get(i + 1)) == Some("as") => {
                std_modules.insert(alias_after(tokens, i + 1, "std"));
            }
            Some("eprintln") => {
                stderr_macros.insert(alias_after(tokens, i + 1, "eprintln"));
            }
            Some("process") => {
                process_modules.insert(alias_after(tokens, i + 1, "process"));
            }
            Some("self")
                if tokens[statement_start..i]
                    .iter()
                    .any(|token| ident(Some(token)) == Some("process")) =>
            {
                process_modules.insert(alias_after(tokens, i + 1, "process"));
            }
            Some("exit")
                if tokens[statement_start..i]
                    .iter()
                    .any(|token| ident(Some(token)) == Some("process")) =>
            {
                exit_functions.insert(alias_after(tokens, i + 1, "exit"));
            }
            _ => {}
        }
    }
    loop {
        let mut changed = false;
        for (source, alias) in &alias_edges {
            if stderr_macros.contains(source) {
                changed |= stderr_macros.insert(alias.clone());
            }
            if process_modules.contains(source) {
                changed |= process_modules.insert(alias.clone());
            }
            if exit_functions.contains(source) {
                changed |= exit_functions.insert(alias.clone());
            }
            if std_modules.contains(source) {
                changed |= std_modules.insert(alias.clone());
            }
        }
        if !changed {
            break;
        }
    }
    (stderr_macros, process_modules, exit_functions, std_modules)
}

fn direct_stderr_calls(source: &str) -> usize {
    let tokens = production_tokens(source);
    let (aliases, _, _, std_modules) = imported_aliases(&tokens);
    (0..tokens.len())
        .filter(|&i| {
            let bare = ident(tokens.get(i)).is_some_and(|name| aliases.contains(name))
                && punct(tokens.get(i + 1), '!')
                && !(i >= 2 && punct(tokens.get(i - 1), ':') && punct(tokens.get(i - 2), ':'));
            let qualified = ident(tokens.get(i)).is_some_and(|name| std_modules.contains(name))
                && punct(tokens.get(i + 1), ':')
                && punct(tokens.get(i + 2), ':')
                && ident(tokens.get(i + 3)) == Some("eprintln")
                && punct(tokens.get(i + 4), '!');
            bare || qualified
        })
        .count()
}

fn process_exit_calls(source: &str) -> usize {
    let tokens = production_tokens(source);
    let (_, process_modules, exit_functions, std_modules) = imported_aliases(&tokens);
    (0..tokens.len())
        .filter(|&i| {
            let direct = ident(tokens.get(i)).is_some_and(|name| std_modules.contains(name))
                && punct(tokens.get(i + 1), ':')
                && punct(tokens.get(i + 2), ':')
                && ident(tokens.get(i + 3)) == Some("process")
                && punct(tokens.get(i + 4), ':')
                && punct(tokens.get(i + 5), ':')
                && ident(tokens.get(i + 6)) == Some("exit")
                && punct(tokens.get(i + 7), '(');
            let module_alias = ident(tokens.get(i))
                .is_some_and(|name| process_modules.contains(name))
                && punct(tokens.get(i + 1), ':')
                && punct(tokens.get(i + 2), ':')
                && ident(tokens.get(i + 3)) == Some("exit")
                && punct(tokens.get(i + 4), '(');
            let function_alias = ident(tokens.get(i))
                .is_some_and(|name| exit_functions.contains(name))
                && punct(tokens.get(i + 1), '(');
            direct || module_alias || function_alias
        })
        .count()
}

fn rust_files_below(rel: &str) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read source directory") {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_none_or(|name| name != "tests.rs")
            {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    visit(&root().join(rel), &mut paths);
    paths.sort();
    paths
}

#[derive(Debug)]
struct FunctionItem {
    name: String,
    header: Vec<Token>,
    body: Vec<Token>,
}

fn matching_brace(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in open..tokens.len() {
        if punct(tokens.get(i), '{') {
            depth += 1;
        } else if punct(tokens.get(i), '}') {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn function_items(tokens: &[Token]) -> Vec<FunctionItem> {
    let mut items = Vec::new();
    let mut i = 0;
    while i + 2 < tokens.len() {
        if punct(tokens.get(i), '{') {
            let Some(close) = matching_brace(tokens, i) else {
                break;
            };
            i = close + 1;
            continue;
        }
        if ident(tokens.get(i)) != Some("fn") {
            i += 1;
            continue;
        }
        let Some(name) = ident(tokens.get(i + 1)) else {
            i += 1;
            continue;
        };
        let Some(open) = (i + 2..tokens.len())
            .find(|&index| punct(tokens.get(index), '{') || punct(tokens.get(index), ';'))
        else {
            break;
        };
        if punct(tokens.get(open), ';') {
            i = open + 1;
            continue;
        }
        let Some(close) = matching_brace(tokens, open) else {
            break;
        };
        items.push(FunctionItem {
            name: name.to_owned(),
            header: tokens[i..open].to_vec(),
            body: tokens[open + 1..close].to_vec(),
        });
        i = close + 1;
    }
    items
}

#[derive(Debug)]
struct PublicItem {
    kind: String,
    name: Option<String>,
    header: Vec<Token>,
    body: Vec<Token>,
}

fn item_keyword_index(tokens: &[Token], mut index: usize) -> Option<usize> {
    while matches!(
        ident(tokens.get(index)),
        Some("async" | "const" | "unsafe" | "extern")
    ) {
        index += 1;
    }

    matches!(
        ident(tokens.get(index)),
        Some("enum" | "fn" | "mod" | "struct" | "trait" | "type" | "use")
    )
    .then_some(index)
}

fn public_items(tokens: &[Token]) -> Vec<PublicItem> {
    let mut items = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        if punct(tokens.get(index), '{') {
            index = matching_brace(tokens, index).map_or(tokens.len(), |close| close + 1);
            continue;
        }
        if ident(tokens.get(index)) != Some("pub") {
            index += 1;
            continue;
        }

        let visibility_end = index + 1;
        if tokens
            .get(visibility_end)
            .is_some_and(|token| punct(Some(token), '('))
        {
            index += 1;
            continue;
        }
        let Some(keyword_index) = item_keyword_index(tokens, visibility_end) else {
            index += 1;
            continue;
        };
        let kind = ident(tokens.get(keyword_index))
            .expect("item keyword")
            .to_owned();
        let name = (kind != "use")
            .then(|| ident(tokens.get(keyword_index + 1)))
            .flatten()
            .map(str::to_owned);
        let Some(boundary) = (keyword_index + 1..tokens.len()).find(|candidate| {
            punct(tokens.get(*candidate), '{') || punct(tokens.get(*candidate), ';')
        }) else {
            break;
        };

        if punct(tokens.get(boundary), ';') {
            items.push(PublicItem {
                kind,
                name,
                header: tokens[index..boundary].to_vec(),
                body: Vec::new(),
            });
            index = boundary + 1;
            continue;
        }

        let Some(close) = matching_brace(tokens, boundary) else {
            break;
        };
        items.push(PublicItem {
            kind,
            name,
            header: tokens[index..boundary].to_vec(),
            body: tokens[boundary + 1..close].to_vec(),
        });
        index = close + 1;
    }

    items
}

#[derive(Debug)]
struct ModuleDecl {
    public: bool,
    name: String,
    body: Option<Vec<Token>>,
}

#[derive(Debug)]
struct UseItem {
    public: bool,
    path: Vec<Token>,
}

fn matching_delimiter(tokens: &[Token], open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0usize;
    for index in open..tokens.len() {
        if punct(tokens.get(index), left) {
            depth += 1;
        } else if punct(tokens.get(index), right) {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn visibility_at(tokens: &[Token], index: usize) -> (bool, usize) {
    if ident(tokens.get(index)) != Some("pub") {
        return (false, index);
    }
    if !punct(tokens.get(index + 1), '(') {
        return (true, index + 1);
    }
    let end = matching_delimiter(tokens, index + 1, '(', ')').map_or(index + 1, |close| close + 1);
    (false, end)
}

fn module_declarations(tokens: &[Token]) -> Vec<ModuleDecl> {
    let mut modules = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if punct(tokens.get(index), '{') {
            index = matching_brace(tokens, index).map_or(tokens.len(), |close| close + 1);
            continue;
        }
        let (public, keyword) = visibility_at(tokens, index);
        if ident(tokens.get(keyword)) != Some("mod") {
            index += 1;
            continue;
        }
        let Some(name) = ident(tokens.get(keyword + 1)) else {
            index += 1;
            continue;
        };
        let Some(boundary) = (keyword + 2..tokens.len()).find(|candidate| {
            punct(tokens.get(*candidate), '{') || punct(tokens.get(*candidate), ';')
        }) else {
            break;
        };
        if punct(tokens.get(boundary), ';') {
            modules.push(ModuleDecl {
                public,
                name: name.to_owned(),
                body: None,
            });
            index = boundary + 1;
            continue;
        }
        let Some(close) = matching_brace(tokens, boundary) else {
            break;
        };
        modules.push(ModuleDecl {
            public,
            name: name.to_owned(),
            body: Some(tokens[boundary + 1..close].to_vec()),
        });
        index = close + 1;
    }
    modules
}

fn use_items(tokens: &[Token]) -> Vec<UseItem> {
    let mut uses = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if punct(tokens.get(index), '{') {
            index = matching_brace(tokens, index).map_or(tokens.len(), |close| close + 1);
            continue;
        }
        let (public, keyword) = visibility_at(tokens, index);
        if ident(tokens.get(keyword)) != Some("use") {
            index += 1;
            continue;
        }
        let Some(end) =
            (keyword + 1..tokens.len()).find(|candidate| punct(tokens.get(*candidate), ';'))
        else {
            break;
        };
        uses.push(UseItem {
            public,
            path: tokens[keyword + 1..end].to_vec(),
        });
        index = end + 1;
    }
    uses
}

const FORBIDDEN_PUBLIC_SYNC_CARRIERS: [&str; 6] = [
    "SyncInput",
    "SyncOutput",
    "SyncInvocation",
    "TestSyncInvocation",
    "SyncRunInput",
    "SyncExecution",
];

fn has_public_lockless_field(item: &PublicItem) -> bool {
    item.kind == "struct"
        && item.body.windows(2).any(|tokens| {
            ident(tokens.first()) == Some("pub") && ident(tokens.get(1)) == Some("lockless")
        })
}

fn public_item_is_dangerous(item: &PublicItem) -> bool {
    let forbidden_name = item
        .name
        .as_deref()
        .is_some_and(|name| FORBIDDEN_PUBLIC_SYNC_CARRIERS.contains(&name));
    let exposes_forbidden = matches!(item.kind.as_str(), "fn" | "trait" | "type")
        && FORBIDDEN_PUBLIC_SYNC_CARRIERS.iter().any(|carrier| {
            mentions(&item.header, carrier)
                || (item.kind == "trait" && mentions(&item.body, carrier))
        });
    let exposes_lockless = matches!(item.kind.as_str(), "fn" | "trait" | "type")
        && (mentions(&item.header, "lockless")
            || (item.kind == "trait" && mentions(&item.body, "lockless")));
    forbidden_name || exposes_forbidden || exposes_lockless || has_public_lockless_field(item)
}

fn module_file(module_dir: &Path, name: &str) -> Option<(PathBuf, PathBuf)> {
    let child_dir = module_dir.join(name);
    let flat = module_dir.join(format!("{name}.rs"));
    if flat.is_file() {
        return Some((flat, child_dir));
    }
    let nested = child_dir.join("mod.rs");
    nested.is_file().then_some((nested, child_dir))
}

fn imported_dangerous_names(
    use_item: &UseItem,
    local_names: &BTreeSet<String>,
    children: &std::collections::BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let identifiers = use_item
        .path
        .iter()
        .enumerate()
        .filter_map(|(index, token)| ident(Some(token)).map(|name| (index, name)))
        .collect::<Vec<_>>();
    let mut imported = BTreeSet::new();
    let child_exports = identifiers
        .iter()
        .map(|(_, name)| *name)
        .filter(|name| !matches!(*name, "self" | "crate" | "super"))
        .find_map(|name| children.get(name));
    if let Some(exports) = child_exports {
        for exported in exports {
            if let Some(position) = use_item
                .path
                .iter()
                .position(|token| ident(Some(token)) == Some(exported))
            {
                let alias = (ident(use_item.path.get(position + 1)) == Some("as"))
                    .then(|| ident(use_item.path.get(position + 2)))
                    .flatten()
                    .unwrap_or(exported);
                imported.insert(alias.to_owned());
            }
        }
    }
    if use_item.path.iter().any(|token| punct(Some(token), '*')) {
        if let Some(exports) = child_exports {
            imported.extend(exports.iter().cloned());
        }
        return imported;
    }

    let alias_position = identifiers.iter().position(|(_, name)| *name == "as");
    let alias =
        alias_position.and_then(|position| identifiers.get(position + 1).map(|(_, name)| *name));
    let target = alias_position
        .and_then(|position| position.checked_sub(1))
        .and_then(|position| identifiers.get(position).map(|(_, name)| *name))
        .or_else(|| identifiers.last().map(|(_, name)| *name));
    let Some(target) = target else {
        return imported;
    };
    let child_exposes_target = child_exports.is_some_and(|exports| exports.contains(target));
    let forbidden = FORBIDDEN_PUBLIC_SYNC_CARRIERS.contains(&target);
    if forbidden || local_names.contains(target) || child_exposes_target {
        imported.insert(alias.unwrap_or(target).to_owned());
    }

    for carrier in FORBIDDEN_PUBLIC_SYNC_CARRIERS {
        if mentions(&use_item.path, carrier) {
            imported.insert(alias.unwrap_or(carrier).to_owned());
        }
    }
    imported
}

fn extend_dangerous_use_exports(
    uses: &[UseItem],
    children: &std::collections::BTreeMap<String, BTreeSet<String>>,
    exports: &mut BTreeSet<String>,
) {
    let mut local_names = exports.clone();
    loop {
        let mut changed = false;
        for use_item in uses {
            let imported = imported_dangerous_names(use_item, &local_names, children);
            for name in imported {
                changed |= local_names.insert(name.clone());
                if use_item.public {
                    changed |= exports.insert(name);
                }
            }
        }
        if !changed {
            break;
        }
    }
}

#[derive(Debug)]
struct ModuleAnalysis {
    exports: BTreeSet<String>,
    children: std::collections::BTreeMap<String, BTreeSet<String>>,
}

fn dangerous_module_analysis(tokens: &[Token], module_dir: Option<&Path>) -> ModuleAnalysis {
    let mut children = std::collections::BTreeMap::new();
    let mut child_sources = std::collections::BTreeMap::new();
    let mut public_children = BTreeSet::new();
    for module in module_declarations(tokens) {
        let ModuleDecl { public, name, body } = module;
        let inline_dir = module_dir.map(|dir| dir.join(&name));
        let (child_tokens, child_dir) = if let Some(body) = body {
            (body, inline_dir)
        } else if let Some((path, child_dir)) = module_dir.and_then(|dir| module_file(dir, &name)) {
            let source = fs::read_to_string(path).expect("read declared sync module");
            (production_tokens(&source), Some(child_dir))
        } else {
            (Vec::new(), None)
        };
        let exports = dangerous_module_analysis(&child_tokens, child_dir.as_deref()).exports;
        if public {
            public_children.insert(name.clone());
        }
        child_sources.insert(name.clone(), child_tokens);
        children.insert(name, exports);
    }

    let parent_children = children.clone();
    for (name, source) in &child_sources {
        let parent_qualified_uses = use_items(source)
            .into_iter()
            .filter(|use_item| {
                matches!(
                    use_item.path.iter().find_map(|token| ident(Some(token))),
                    Some("crate" | "super")
                )
            })
            .collect::<Vec<_>>();
        if let Some(exports) = children.get_mut(name) {
            extend_dangerous_use_exports(&parent_qualified_uses, &parent_children, exports);
        }
    }

    let mut exports = public_items(tokens)
        .into_iter()
        .filter(|item| !matches!(item.kind.as_str(), "mod" | "use"))
        .filter(public_item_is_dangerous)
        .filter_map(|item| item.name)
        .collect::<BTreeSet<_>>();
    for module in public_children {
        if children.get(&module).is_some_and(|names| !names.is_empty()) {
            exports.insert(module);
        }
    }

    extend_dangerous_use_exports(&use_items(tokens), &children, &mut exports);
    ModuleAnalysis { exports, children }
}

fn dangerous_module_exports(tokens: &[Token], module_dir: Option<&Path>) -> BTreeSet<String> {
    dangerous_module_analysis(tokens, module_dir).exports
}

fn public_sync_escape_hatches(source: &str) -> Vec<String> {
    dangerous_module_exports(&production_tokens(source), None)
        .into_iter()
        .map(|name| format!("public sync escape hatch `{name}`"))
        .collect()
}

fn crate_module_context(path: &Path) -> Option<(PathBuf, PathBuf, String)> {
    if path.file_name()?.to_str()? != "mod.rs" {
        return None;
    }
    let module_dir = path.parent()?;
    let crate_dir = module_dir.parent()?.to_path_buf();
    let crate_root = crate_dir.join("lib.rs");
    if !crate_root.is_file() {
        return None;
    }
    let module_name = module_dir.file_name()?.to_str()?.to_owned();
    Some((crate_root, crate_dir, module_name))
}

fn public_sync_tree_escape_hatches(path: &Path) -> Vec<String> {
    let module_dir = path.parent().expect("sync module has parent");
    let exports = if let Some((crate_root, crate_dir, module_name)) = crate_module_context(path) {
        let source = fs::read_to_string(crate_root).expect("read crate root");
        let tokens = production_tokens(&source);
        let module_is_public = module_declarations(&tokens)
            .into_iter()
            .any(|module| module.public && module.name == module_name);
        if module_is_public {
            dangerous_module_analysis(&tokens, Some(&crate_dir))
                .children
                .remove(&module_name)
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        }
    } else {
        let source = fs::read_to_string(path).expect("read sync module tree root");
        dangerous_module_exports(&production_tokens(&source), Some(module_dir))
    };
    exports
        .into_iter()
        .map(|name| format!("public sync escape hatch `{name}`"))
        .collect()
}

const ALLOWED_SYNC_WARNING_SHAPES: [&str; 14] = [
    "Projection(ProjectionWarning)",
    "MalformedTransitiveHooks{target:String,detail:String}",
    "LinkPathNotPortable{source:String,path:PathBuf}",
    "ReferenceMoved{source:String,target:String,from:String,to:String}",
    "OrphanedRecords{count:usize}",
    "PruneSkippedAfterFailures",
    "PruneRefused{path:PathBuf,reason:String}",
    "OrphanRecordPathUnknown{source:String,artifact:String,layout:String}",
    "FastForwardKeptLive{source:String,artifact:String,path:PathBuf}",
    "FastForwardDropped{source:String,artifact:String}",
    "CrossDeviceFallback{destination:PathBuf}",
    "ConflictModified{source:String,artifact:String,changed:Vec<PathBuf>}",
    "ConflictForeign{path:PathBuf}",
    "UntrustedTransitiveHooks{count:usize}",
];

fn render_tokens(tokens: &[Token]) -> String {
    tokens
        .iter()
        .enumerate()
        .filter_map(|(index, token)| match token {
            Token::Punct(',')
                if tokens
                    .get(index + 1)
                    .is_some_and(|next| punct(Some(next), '}') || punct(Some(next), ')')) =>
            {
                None
            }
            Token::Ident(value) => Some(value.clone()),
            Token::Punct(value) => Some(value.to_string()),
        })
        .collect()
}

fn sync_warning_shapes(source: &str) -> Result<BTreeSet<String>, String> {
    let tokens = production_tokens(source);
    let warning = public_items(&tokens)
        .into_iter()
        .find(|item| item.kind == "enum" && item.name.as_deref() == Some("SyncWarning"))
        .ok_or_else(|| "missing public SyncWarning enum".to_owned())?;
    let mut shapes = BTreeSet::new();
    let mut index = 0;
    while index < warning.body.len() {
        while punct(warning.body.get(index), ',') {
            index += 1;
        }
        if index == warning.body.len() {
            break;
        }
        let start = index;
        if ident(warning.body.get(index)).is_none() {
            return Err(format!("unexpected SyncWarning token at index {index}"));
        }
        index += 1;
        let end = if punct(warning.body.get(index), '(') {
            matching_delimiter(&warning.body, index, '(', ')')
                .ok_or_else(|| "unterminated tuple warning variant".to_owned())?
                + 1
        } else if punct(warning.body.get(index), '{') {
            matching_brace(&warning.body, index)
                .ok_or_else(|| "unterminated structured warning variant".to_owned())?
                + 1
        } else {
            index
        };
        shapes.insert(render_tokens(&warning.body[start..end]));
        index = end;
        if index < warning.body.len() && !punct(warning.body.get(index), ',') {
            return Err(format!("warning variant lacks delimiter at index {index}"));
        }
    }
    Ok(shapes)
}

fn sync_warning_contract_violations(source: &str) -> Vec<String> {
    let expected = ALLOWED_SYNC_WARNING_SHAPES
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let Ok(actual) = sync_warning_shapes(source) else {
        return vec!["unable to parse SyncWarning".to_owned()];
    };
    expected
        .difference(&actual)
        .map(|shape| format!("missing {shape}"))
        .chain(
            actual
                .difference(&expected)
                .map(|shape| format!("unexpected {shape}")),
        )
        .collect()
}

#[test]
fn production_sync_api_has_no_compatibility_or_warning_escape_hatches() {
    let sync_source = fs::read_to_string(root().join("src/sync/mod.rs")).expect("read sync module");
    let sync_tokens = production_tokens(&sync_source);
    let sync_functions: Vec<_> = public_items(&sync_tokens)
        .into_iter()
        .filter(|item| item.kind == "fn" && item.name.as_deref() == Some("sync"))
        .collect();
    assert_eq!(
        sync_functions.len(),
        1,
        "production must expose one unambiguous public sync entry point"
    );
    assert!(
        mentions(&sync_functions[0].header, "SyncRequest")
            && mentions(&sync_functions[0].header, "SyncReport"),
        "production sync must accept SyncRequest and return SyncReport directly"
    );

    let offenders = public_sync_tree_escape_hatches(&root().join("src/sync/mod.rs"));
    assert!(
        offenders.is_empty(),
        "production sync API leaked compatibility surfaces: {offenders:?}"
    );

    let warning_source =
        fs::read_to_string(root().join("src/sync/request.rs")).expect("read warning contract");
    let warning_violations = sync_warning_contract_violations(&warning_source);
    assert!(
        warning_violations.is_empty(),
        "SyncWarning must retain only the structured contract: {warning_violations:?}"
    );
}

#[test]
fn public_sync_escape_hatch_scanner_rejects_visibility_mutations() {
    let private_and_test_only = r"
        struct SyncRunInput { lockless: bool }
        enum SyncExecution { Completed }
        mod private { pub struct SyncOutput; }
        use self::private::SyncOutput as HiddenOutput;
        pub(crate) fn sync_opened(lockless: bool) {}
        #[cfg(test)]
        pub struct SyncInput { pub lockless: bool }
    ";
    assert_eq!(
        public_sync_escape_hatches(private_and_test_only),
        Vec::<String>::new()
    );

    for carrier in FORBIDDEN_PUBLIC_SYNC_CARRIERS {
        let mutation = format!("pub struct {carrier};");
        assert!(
            !public_sync_escape_hatches(&mutation).is_empty(),
            "scanner accepted public legacy carrier `{carrier}`"
        );
    }

    let renamed_carrier = "pub struct OpenSyncInput { pub lockless: bool }";
    assert!(
        !public_sync_escape_hatches(renamed_carrier).is_empty(),
        "scanner accepted a renamed public lockless-bearing carrier"
    );

    let renamed_entry = r"
        struct SyncRunInput;
        struct SyncExecution;
        pub fn run_compatibility(_: SyncRunInput) -> SyncExecution { unreachable!() }
    ";
    assert!(
        !public_sync_escape_hatches(renamed_entry).is_empty(),
        "scanner accepted a renamed public compatibility function"
    );

    let lockless_function =
        "pub fn sync_opened(request: &SyncRequest, lockless: bool) -> SyncReport { todo!() }";
    assert!(
        !public_sync_escape_hatches(lockless_function).is_empty(),
        "scanner accepted caller-supplied lockless in a public function signature"
    );

    let nested_public = "pub mod compatibility { pub struct SyncInput { pub lockless: bool } }";
    assert!(
        !public_sync_escape_hatches(nested_public).is_empty(),
        "scanner accepted a legacy carrier inside a public inline module"
    );

    let direct_reexport = r"
        mod compatibility { pub struct SyncInput; }
        pub use compatibility::SyncInput as OpenSyncInput;
    ";
    assert!(
        !public_sync_escape_hatches(direct_reexport).is_empty(),
        "scanner accepted a direct public legacy-carrier re-export"
    );

    let grouped_reexport = r"
        mod compatibility {
            pub struct OpenSyncInput { pub lockless: bool }
            pub struct Harmless;
        }
        pub use compatibility::{OpenSyncInput, Harmless};
    ";
    assert!(
        !public_sync_escape_hatches(grouped_reexport).is_empty(),
        "scanner accepted a non-final dangerous item in a grouped re-export"
    );

    let glob_reexport = r"
        mod compatibility { pub struct SyncInput; }
        pub use compatibility::*;
    ";
    assert!(
        !public_sync_escape_hatches(glob_reexport).is_empty(),
        "scanner accepted a glob-reexported legacy carrier"
    );

    let laundered_reexport = r"
        mod compatibility { pub struct SyncInput; }
        use compatibility::SyncInput as HiddenInput;
        pub use HiddenInput as OpenSyncInput;
    ";
    assert!(
        !public_sync_escape_hatches(laundered_reexport).is_empty(),
        "scanner accepted an alias-laundered public legacy carrier"
    );

    let file_module = tempfile::TempDir::new().expect("create file-module mutation fixture");
    fs::write(file_module.path().join("mod.rs"), "pub mod compatibility;")
        .expect("write file-module root");
    fs::write(
        file_module.path().join("compatibility.rs"),
        "pub struct SyncInput { pub lockless: bool }",
    )
    .expect("write dangerous public child module");
    assert!(
        !public_sync_tree_escape_hatches(&file_module.path().join("mod.rs")).is_empty(),
        "scanner accepted a legacy carrier inside a public file module"
    );
}

#[test]
fn public_sync_escape_hatch_scanner_normalizes_reexport_prefixes() {
    for (prefix, label) in [("self::", "self"), ("crate::sync::", "crate")] {
        let qualified_glob = format!(
            "mod compatibility {{ pub struct SyncInput; }} \
             pub use {prefix}compatibility::*;"
        );
        assert!(
            !public_sync_escape_hatches(&qualified_glob).is_empty(),
            "scanner accepted a {label}-qualified glob re-export"
        );
    }

    let super_module = tempfile::TempDir::new().expect("create super-path mutation fixture");
    fs::write(
        super_module.path().join("mod.rs"),
        "mod compatibility; pub mod facade;",
    )
    .expect("write super-path root");
    fs::write(
        super_module.path().join("compatibility.rs"),
        "pub struct SyncInput { pub lockless: bool }",
    )
    .expect("write private dangerous sibling module");
    fs::write(
        super_module.path().join("facade.rs"),
        "pub use super::compatibility::*;",
    )
    .expect("write public super-qualified facade");
    assert!(
        !public_sync_tree_escape_hatches(&super_module.path().join("mod.rs")).is_empty(),
        "scanner accepted a super-qualified glob from a public sibling facade"
    );

    let crate_tree = tempfile::TempDir::new().expect("create crate-path mutation fixture");
    fs::create_dir(crate_tree.path().join("sync")).expect("create sync module directory");
    fs::write(
        crate_tree.path().join("lib.rs"),
        "mod compat; pub mod sync;",
    )
    .expect("write crate root");
    fs::write(
        crate_tree.path().join("compat.rs"),
        "pub struct SyncInput { pub lockless: bool }",
    )
    .expect("write dangerous crate-root sibling");
    fs::write(
        crate_tree.path().join("sync/mod.rs"),
        "pub use crate::compat::*;",
    )
    .expect("write crate-qualified sync re-export");
    assert!(
        !public_sync_tree_escape_hatches(&crate_tree.path().join("sync/mod.rs")).is_empty(),
        "scanner accepted a crate-root sibling glob re-exported through sync"
    );

    fs::write(crate_tree.path().join("sync/mod.rs"), "pub struct Safe;")
        .expect("replace sync module with safe API");
    assert!(
        public_sync_tree_escape_hatches(&crate_tree.path().join("sync/mod.rs")).is_empty(),
        "an unrelated dangerous crate-root sibling must not taint a safe sync module"
    );

    fs::write(crate_tree.path().join("lib.rs"), "mod compat; mod sync;")
        .expect("make sync module private");
    fs::write(
        crate_tree.path().join("sync/mod.rs"),
        "pub use crate::compat::*;",
    )
    .expect("restore dangerous re-export behind private sync module");
    assert!(
        public_sync_tree_escape_hatches(&crate_tree.path().join("sync/mod.rs")).is_empty(),
        "a private sync module must not be treated as public API"
    );
}

#[test]
fn sync_warning_shape_scanner_rejects_renamed_string_catch_alls() {
    let allowed = ALLOWED_SYNC_WARNING_SHAPES.join(",");
    let control = format!("pub enum SyncWarning {{ {allowed} }}");
    assert!(
        sync_warning_contract_violations(&control).is_empty(),
        "scanner must accept the exact structured warning shapes"
    );

    for catch_all in ["Raw(String)", "Diagnostic(String)"] {
        let mutation = format!("pub enum SyncWarning {{ {allowed},{catch_all} }}");
        let violations = sync_warning_contract_violations(&mutation);
        assert!(
            violations
                .iter()
                .any(|violation| violation == &format!("unexpected {catch_all}")),
            "scanner accepted renamed string catch-all `{catch_all}`: {violations:?}"
        );
    }
}

fn symbol_aliases(tokens: &[Token]) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    for i in 0..tokens.len() {
        if let (Some(source), Some("as"), Some(alias)) = (
            ident(tokens.get(i)),
            ident(tokens.get(i + 1)),
            ident(tokens.get(i + 2)),
        ) {
            aliases.push((source.to_owned(), alias.to_owned()));
        }
        if ident(tokens.get(i)) == Some("let")
            && let Some(alias) = ident(tokens.get(i + 1))
            && punct(tokens.get(i + 2), '=')
        {
            let end = (i + 3..tokens.len())
                .find(|&index| punct(tokens.get(index), ';'))
                .unwrap_or(tokens.len());
            if let Some(source) = tokens[i + 3..end]
                .iter()
                .rev()
                .find_map(|token| ident(Some(token)))
            {
                aliases.push((source.to_owned(), alias.to_owned()));
            }
        }
    }
    aliases
}

fn resolve_alias(name: &str, aliases: &[(String, String)]) -> String {
    let mut resolved = name.to_owned();
    for _ in 0..=aliases.len() {
        let Some((source, _)) = aliases.iter().find(|(_, alias)| alias == &resolved) else {
            break;
        };
        resolved.clone_from(source);
    }
    resolved
}

fn called_names(tokens: &[Token], aliases: &[(String, String)]) -> Vec<String> {
    (0..tokens.len())
        .filter_map(|i| {
            let name = ident(tokens.get(i))?;
            punct(tokens.get(i + 1), '(').then(|| resolve_alias(name, aliases))
        })
        .collect()
}

#[derive(Debug)]
struct AnalyzedFunction {
    location: String,
    name: String,
    header: Vec<Token>,
    body: Vec<Token>,
    calls: Vec<String>,
}

fn mentions(tokens: &[Token], expected: &str) -> bool {
    tokens
        .iter()
        .any(|token| ident(Some(token)) == Some(expected))
}

fn analyze_functions(sources: &[(&str, &str)]) -> Vec<AnalyzedFunction> {
    let mut functions = Vec::new();
    for (location, source) in sources {
        let tokens = production_tokens(source);
        let aliases = symbol_aliases(&tokens);
        functions.extend(function_items(&tokens).into_iter().map(|item| {
            let calls = called_names(&item.body, &aliases);
            AnalyzedFunction {
                location: (*location).to_owned(),
                name: item.name,
                header: item.header,
                body: item.body,
                calls,
            }
        }));
    }
    functions
}

fn forbidden_post_apply_removal_paths(sources: &[(&str, &str)]) -> Vec<String> {
    let functions = analyze_functions(sources);
    let reconciled_remove_helpers: BTreeSet<String> = functions
        .iter()
        .filter(|function| {
            mentions(&function.header, "SyncChange")
                && (mentions(&function.header, "Remove") || mentions(&function.body, "Remove"))
        })
        .map(|function| function.name.clone())
        .collect();

    let is_low_level_remove = |call: &str| {
        matches!(
            call,
            "remove" | "remove_file" | "remove_dir" | "remove_orphan_path"
        )
    };
    let mut unreconciled_mutators: BTreeSet<String> = functions
        .iter()
        .filter(|function| {
            !reconciled_remove_helpers.contains(&function.name)
                && function.calls.iter().any(|call| is_low_level_remove(call))
        })
        .map(|function| function.name.clone())
        .collect();
    loop {
        let mut changed = false;
        for function in &functions {
            if !reconciled_remove_helpers.contains(&function.name)
                && function
                    .calls
                    .iter()
                    .any(|call| unreconciled_mutators.contains(call))
            {
                changed |= unreconciled_mutators.insert(function.name.clone());
            }
        }
        if !changed {
            break;
        }
    }

    let mut forbidden: BTreeSet<String> =
        functions
            .iter()
            .filter(|function| {
                unreconciled_mutators.contains(&function.name)
                    && function.header.iter().chain(&function.body).any(|token| {
                        matches!(ident(Some(token)), Some("Projection" | "projection"))
                    })
            })
            .map(|function| function.name.clone())
            .collect();
    loop {
        let mut changed = false;
        for function in &functions {
            if !reconciled_remove_helpers.contains(&function.name)
                && function.calls.iter().any(|call| forbidden.contains(call))
            {
                changed |= forbidden.insert(function.name.clone());
            }
        }
        if !changed {
            break;
        }
    }

    let mut offenders = Vec::new();
    for function in &functions {
        let Some(apply_index) = function.calls.iter().position(|call| {
            call.starts_with("apply") && !call.contains("fast_forward") && !call.contains("drop")
        }) else {
            continue;
        };
        let later = &function.calls[apply_index + 1..];
        for call in later {
            if forbidden.contains(call)
                || (is_low_level_remove(call)
                    && unreconciled_mutators.contains(&function.name)
                    && !reconciled_remove_helpers.contains(&function.name))
            {
                offenders.push(format!(
                    "{}::{} -> {call}",
                    function.location, function.name
                ));
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    offenders
}

#[test]
fn removal_has_no_separate_projection_driven_post_apply_prune_path() {
    let owned: Vec<(String, String)> = rust_files_below("src/sync")
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root().join("src/sync"))
                .expect("sync-relative path")
                .display()
                .to_string();
            let source = fs::read_to_string(path).expect("read sync source");
            (relative, source)
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = owned
        .iter()
        .map(|(location, source)| (location.as_str(), source.as_str()))
        .collect();
    let post_apply = forbidden_post_apply_removal_paths(&borrowed);
    assert!(
        post_apply.is_empty(),
        "orphan removal must be executed from reconciled Remove rows, never by a separate \
         Projection-driven post-apply pruning path anywhere under src/sync; calls: {post_apply:?}"
    );
}

#[test]
fn sync_production_returns_structured_output_without_direct_stderr_calls() {
    let offenders: Vec<String> = rust_files_below("src/sync")
        .into_iter()
        .filter_map(|path| {
            let count = direct_stderr_calls(&fs::read_to_string(&path).expect("read sync source"));
            (count > 0).then(|| {
                format!(
                    "{} ({count})",
                    path.strip_prefix(root()).expect("relative path").display()
                )
            })
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "sync production contains direct stderr calls: {offenders:?}"
    );
}

#[test]
fn process_exit_is_called_only_by_main() {
    let mut offenders = Vec::new();
    for path in rust_files_below("src") {
        let relative = path.strip_prefix(root()).expect("relative path");
        if relative == Path::new("src/main.rs") {
            continue;
        }
        let count = process_exit_calls(&fs::read_to_string(&path).expect("read Rust source"));
        if count > 0 {
            offenders.push(format!("{} ({count})", relative.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "only main may call process exit: {offenders:?}"
    );
    assert_eq!(
        process_exit_calls(&fs::read_to_string(root().join("src/main.rs")).expect("read main")),
        1,
        "main owns one exit mapping call"
    );
}

#[test]
fn call_scanner_ignores_non_code_and_test_only_cfg() {
    let decoys = r#"// eprintln!("comment"); std::process::exit(1)
        const S: &str = "eprintln!(literal); process::exit(2)";
        /* use std::process::exit as die; die(3); */"#;
    assert_eq!(direct_stderr_calls(decoys), 0);
    assert_eq!(process_exit_calls(decoys), 0);
    assert_eq!(
        direct_stderr_calls("#[cfg(test)] mod tests { fn f() { eprintln!(\"test\"); } }"),
        0
    );
    assert_eq!(
        direct_stderr_calls(
            "#[cfg(all(test, unix))] mod tests { fn f() { eprintln!(\"test\"); } }"
        ),
        0
    );
    assert_eq!(
        process_exit_calls(
            "#[cfg(any(test, doctest))] mod tests { fn f() { std::process::exit(1); } }"
        ),
        0
    );
    assert_eq!(
        process_exit_calls(
            "#[cfg(any(test, unix))] fn maybe_production() { std::process::exit(1); }"
        ),
        1,
        "a cfg that can compile outside tests must remain in the production scan"
    );
    assert_eq!(
        process_exit_calls("fn f<'a>(_: &'a str) { std::process::exit(1); }"),
        1,
        "a lifetime must not be lexed as a character literal that hides the call"
    );
}

#[test]
fn call_scanner_resolves_ordinary_aliases() {
    assert_eq!(
        direct_stderr_calls("use std::eprintln as warn; warn!(\"x\");"),
        1
    );
    assert_eq!(
        direct_stderr_calls("use std::{eprintln as warn}; warn!(\"x\");"),
        1
    );
    assert_eq!(
        direct_stderr_calls("use std::eprintln as warn; use self::warn as note; note!(\"x\");"),
        1
    );
    assert_eq!(direct_stderr_calls("std::eprintln!(\"x\");"), 1);
    assert_eq!(process_exit_calls("use std::process as p; p::exit(1);"), 1);
    assert_eq!(
        process_exit_calls("use std::process::exit as die; die(1);"),
        1
    );
    assert_eq!(
        process_exit_calls("use std::{process::exit as die}; die(1);"),
        1
    );
    assert_eq!(
        process_exit_calls(
            "use std::process as platform_process; use self::platform_process as process2; \
             process2::exit(1);"
        ),
        1
    );
    assert_eq!(
        process_exit_calls(
            "use std::process::exit as die; use self::die as terminate; terminate(1);"
        ),
        1
    );
    assert_eq!(
        process_exit_calls("use std as standard; standard::process::exit(1);"),
        1
    );
}

#[test]
fn removal_call_graph_detects_separate_post_apply_paths() {
    let moved_second_pass = [
        (
            "relocated.rs",
            "fn sweep_all(_: &Projection) { remove_orphan_path(); }",
        ),
        (
            "orchestrator.rs",
            "use relocated::sweep_all as prune_once;
             fn maybe_cleanup() { let finish = prune_once; finish(); }
             fn orchestrate() { apply_changes(); let phase_two = maybe_cleanup; phase_two(); }",
        ),
    ];
    assert_eq!(
        forbidden_post_apply_removal_paths(&moved_second_pass),
        vec!["orchestrator.rs::orchestrate -> maybe_cleanup".to_owned()],
        "moving and renaming both the bulk pass and its wrapper must not launder a second path"
    );

    let inlined_second_pass = [(
        "target.rs",
        "fn unrelated_name(projection: &Projection) {
             apply_changes();
             for _ in projection.targets { remove_orphan_path(); }
         }",
    )];
    assert_eq!(
        forbidden_post_apply_removal_paths(&inlined_second_pass),
        vec!["target.rs::unrelated_name -> remove_orphan_path".to_owned()],
        "inlining the second pass under an unrelated symbol in another module must still fail"
    );

    let unified_remove = [(
        "apply.rs",
        "fn apply_remove(change: &SyncChange) {
             if let SyncChange::Remove { .. } = change { remove_orphan_path(); }
         }
         fn apply_workspace(projection: &Projection) {
             apply_deploy_changes();
             apply_remove();
         }",
    )];
    assert!(
        forbidden_post_apply_removal_paths(&unified_remove).is_empty(),
        "a helper applying one reconciled SyncChange::Remove is part of the unified workspace pass"
    );
}
