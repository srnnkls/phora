//! PAR-001 parallel-resolve speedup.
//!
//! Real fetch/resolve/digest is IO-bound, so an in-memory backend would overlap
//! nothing and show no win; `LatencyBackend` injects a fixed per-call delay to
//! model that cost.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use phora::config::{Config, Refspec};
use phora::kernel::{ArtifactName, Selection, SourceName};
use phora::source::{ExportRequest, ExportResult, SourceBackend, SourceError};
use phora::sync::resolve_sources_for_bench;

type R<T> = std::result::Result<T, SourceError>;

const SOURCES: usize = 12;
const LATENCY: Duration = Duration::from_millis(3);

/// Fixed-latency stand-in for a network backend.
struct LatencyBackend {
    latency: Duration,
}

impl SourceBackend for LatencyBackend {
    fn fetch(&self, _source: &SourceName, _url: &str) -> R<()> {
        sleep(self.latency);
        Ok(())
    }

    fn resolve(&self, _source: &SourceName, _url: &str, _refspec: &Refspec) -> R<String> {
        sleep(self.latency);
        Ok("0".repeat(40))
    }

    fn compute_digest(
        &self,
        _source: &SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _selection: &Selection,
    ) -> R<String> {
        sleep(self.latency);
        Ok("digest".to_owned())
    }

    fn commit_time(&self, _source: &SourceName, _url: &str, _commit: &str) -> R<u64> {
        unreachable!("resolve path does not call commit_time")
    }

    fn discover_artifacts(
        &self,
        _source: &SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _selection: &Selection,
    ) -> R<Vec<ArtifactName>> {
        unreachable!("resolve path does not call discover_artifacts")
    }

    fn export_artifact(&self, _req: &ExportRequest<'_>) -> R<ExportResult> {
        unreachable!("resolve path does not call export_artifact")
    }

    fn list_artifact_files(
        &self,
        _source: &SourceName,
        _url: &str,
        _commit: &str,
        _root: Option<&Path>,
        _artifact: &ArtifactName,
        _selection: &Selection,
    ) -> R<Vec<PathBuf>> {
        unreachable!("resolve path does not call list_artifact_files")
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
