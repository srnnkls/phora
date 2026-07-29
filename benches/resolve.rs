//! PAR-001 parallel-resolve speedup.
//!
//! Real fetch/resolve/digest is IO-bound, so an in-memory backend would overlap
//! nothing and show no win; `LatencyBackend` injects a fixed per-call delay to
//! model that cost.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use phora::config::Config;
use phora::source::{
    Commit, MirrorKey, NormalizedUrl, ResolvePolicy, ResolveRequest, ResolvedRevision,
    ResolvedSource, SnapshotId, SourceDirectoryEntry, SourceEntry, SourceError, SourceIdentity,
    SourceInventory, SourceLocation, SourcePath, SourceStore, SourceTimestamp,
};
use phora::sync::resolve_sources_for_bench;

type R<T> = std::result::Result<T, SourceError>;

const SOURCES: usize = 12;
const LATENCY: Duration = Duration::from_millis(3);

/// Fixed-latency stand-in for a network backend.
struct LatencyBackend {
    latency: Duration,
}

impl SourceStore for LatencyBackend {
    fn resolve(&self, request: &ResolveRequest, _policy: ResolvePolicy) -> R<ResolvedSource> {
        sleep(self.latency);
        let commit: Commit = "0".repeat(40).parse().expect("fixed commit is valid");
        let url = match &request.location {
            SourceLocation::Git { url } | SourceLocation::Url { url } => url,
            SourceLocation::Worktree { .. } => {
                unreachable!("the benchmark generates only Git sources")
            }
        };
        let normalized = NormalizedUrl::parse(url);
        Ok(ResolvedSource {
            name: request.name.clone(),
            snapshot: SnapshotId::Git {
                mirror: MirrorKey::from_url(&normalized),
                commit: commit.clone(),
            },
            revision: ResolvedRevision::Commit(commit),
            authored_at: SourceTimestamp::from_unix_seconds(0),
            normalized_location: SourceIdentity::Git(normalized),
        })
    }

    fn inventory(&self, _snapshot: &SnapshotId, _root: Option<&SourcePath>) -> R<SourceInventory> {
        sleep(self.latency);
        Ok(SourceInventory::default())
    }

    fn read(&self, _snapshot: &SnapshotId, _path: &SourcePath) -> R<SourceEntry> {
        sleep(self.latency);
        unreachable!("an empty inventory never asks the latency fake to read a leaf")
    }

    fn list_directory(
        &self,
        _snapshot: &SnapshotId,
        _path: Option<&SourcePath>,
    ) -> R<Vec<SourceDirectoryEntry>> {
        sleep(self.latency);
        Ok(Vec::new())
    }
}

/// `n` git-mode sources with distinct URLs — distinct mirrors fetch in parallel.
fn config_with_sources(n: usize) -> Config {
    use std::fmt::Write;
    let mut toml = String::from("version = 1\n");
    for i in 0..n {
        write!(
            toml,
            "\n[sources.src{i}]\ngit = \"https://example.com/src{i}.git\"\nbranch = \"main\"\n"
        )
        .expect("writing to a String is infallible");
    }
    Config::parse(&toml).expect("generated config parses")
}

fn bench_resolve(c: &mut Criterion) {
    let config = config_with_sources(SOURCES);
    let parsed = config.parsed_sources().expect("sources parse");
    let remotes: BTreeMap<String, String> = parsed
        .keys()
        .map(|name| (name.clone(), format!("https://example.com/{name}.git")))
        .collect();
    let backend = LatencyBackend { latency: LATENCY };

    let mut group = c.benchmark_group("resolve_sources");
    group.sample_size(20);
    for (label, jobs) in [("serial", Some(1usize)), ("parallel", None)] {
        group.bench_with_input(BenchmarkId::from_parameter(label), &jobs, |b, &jobs| {
            b.iter(|| {
                resolve_sources_for_bench(&config, &parsed, &remotes, None, &backend, false, jobs)
                    .expect("resolve succeeds")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
