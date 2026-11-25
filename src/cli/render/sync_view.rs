//! The human view of a sync run.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;
use std::time::Duration;

use super::{format_elapsed, format_sync_warning, plural};

use crate::config::{BuildInputs, Config, ParsedSource, Remote};
use crate::sync::progress::{CommandRun, CommandSubject, Phase, Severity};
use crate::sync::{
    AppliedChange, HookScope, RemovalReason, SkippedChange, SyncSummary, SyncWarning,
};

const CHANGES_PER_TARGET: usize = 8;
const OUTPUT_LINES: usize = 20;
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Debug, Clone, Copy)]
pub(in crate::cli) struct Palette {
    colors: bool,
}

impl Palette {
    pub(in crate::cli) fn stderr() -> Self {
        Self {
            colors: console::colors_enabled_stderr(),
        }
    }

    pub(in crate::cli) fn stdout() -> Self {
        Self {
            colors: console::colors_enabled(),
        }
    }

    #[cfg(test)]
    pub(in crate::cli) fn plain() -> Self {
        Self { colors: false }
    }

    fn paint(self, text: &str, style: console::Style) -> String {
        style.force_styling(self.colors).apply_to(text).to_string()
    }

    fn bold(self, text: &str) -> String {
        self.paint(text, console::Style::new().bold())
    }

    fn green(self, text: &str) -> String {
        self.paint(text, console::Style::new().green())
    }

    fn red(self, text: &str) -> String {
        self.paint(text, console::Style::new().red())
    }

    fn yellow(self, text: &str) -> String {
        self.paint(text, console::Style::new().yellow())
    }

    fn cyan(self, text: &str) -> String {
        self.paint(text, console::Style::new().cyan())
    }

    fn dim(self, text: &str) -> String {
        self.paint(text, console::Style::new().dim())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Mark {
    Done,
    Failed,
    Caution,
}

impl Mark {
    fn paint(self, palette: Palette) -> String {
        match self {
            Self::Done => palette.green("✓"),
            Self::Failed => palette.red("✗"),
            Self::Caution => palette.yellow("!"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(in crate::cli) struct RunContext {
    pub command: &'static str,
    pub project: String,
    pub sources: BTreeMap<String, String>,
    pub targets: BTreeMap<String, String>,
    pub bindings: BTreeMap<String, Vec<String>>,
    pub hooks: Vec<String>,
}

impl RunContext {
    pub(in crate::cli) fn new(command: &'static str, config: &Config, project: &Path) -> Self {
        let sources = config
            .sources
            .iter()
            .map(|(name, source)| {
                let description = ParsedSource::parse(name, source)
                    .map(|parsed| describe_source(&parsed))
                    .unwrap_or_default();
                (name.clone(), description)
            })
            .collect();
        let targets = config
            .targets
            .iter()
            .map(|(name, target)| (name.clone(), display_path(&target.expanded_path(), project)))
            .collect();
        let bindings = config
            .targets
            .iter()
            .map(|(name, target)| {
                (
                    name.clone(),
                    target.declared_sources().map(str::to_owned).collect(),
                )
            })
            .collect();
        let global = config
            .hooks
            .iter()
            .flat_map(|hooks| hooks.pre_sync.iter().chain(&hooks.post_sync).flatten());
        let per_target = config
            .targets
            .values()
            .filter_map(|target| target.hooks.as_ref())
            .flat_map(|hooks| hooks.on_change.iter().chain(&hooks.pre_deploy).flatten());
        Self {
            command,
            project: display_path(project, Path::new("")),
            sources,
            targets,
            bindings,
            hooks: global
                .chain(per_target)
                .map(crate::config::HookCommand::display)
                .collect(),
        }
    }
}

fn describe_source(source: &ParsedSource) -> String {
    let mut text = match &source.remote {
        Remote::Path(path) => format!("local {path}"),
        Remote::Git(url) => format!("git {url}"),
        Remote::Host { host, repo, .. } => format!("{host} {repo}"),
        Remote::Url { url, .. } => format!("url {}", url.rsplit('/').next().unwrap_or(url)),
        Remote::Build(spec) => match &spec.inputs {
            BuildInputs::Sources(inputs) => format!("build from {}", inputs.join(", ")),
            BuildInputs::Root(input) => format!("build from {input}"),
            BuildInputs::Repo => "build from its repo".to_owned(),
        },
    };
    if let Some(reference) = source
        .tag
        .as_ref()
        .or(source.rev.as_ref())
        .or(source.branch.as_ref())
    {
        let _ = write!(text, " @ {reference}");
    }
    text
}

pub(in crate::cli) fn display_path(path: &Path, base: &Path) -> String {
    if !base.as_os_str().is_empty()
        && let Ok(relative) = path.strip_prefix(base)
    {
        return relative.display().to_string();
    }
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return Path::new("~").join(relative).display().to_string();
    }
    path.display().to_string()
}

/// A composed source key `<instance>%<member>` reads as its member name.
pub(in crate::cli) fn source_name(key: &str) -> &str {
    match key.split_once(crate::config::transitive::MEMBER_SEPARATOR) {
        Some((instance, "" | "%")) => instance,
        Some((_, member)) => member,
        None => key,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct Change {
    pub sign: char,
    pub target: String,
    pub source: String,
    pub artifact: String,
    pub note: Option<String>,
}

impl Change {
    pub(in crate::cli) fn applied(change: &AppliedChange) -> Self {
        let (sign, target, source, artifact, note) = match change {
            AppliedChange::Deployed {
                target,
                source,
                artifact,
            } => ('+', target, source, artifact, None),
            AppliedChange::Updated {
                target,
                source,
                artifact,
            }
            | AppliedChange::Overwritten {
                target,
                source,
                artifact,
            } => ('~', target, source, artifact, None),
            AppliedChange::OverlayRewritten {
                target,
                source,
                artifact,
            } => ('~', target, source, artifact, Some("history overlay")),
            AppliedChange::Ejected {
                target,
                source,
                artifact,
            } => ('~', target, source, artifact, Some("ejected")),
            AppliedChange::Removed {
                target,
                source,
                artifact,
                reason,
                ..
            } => (
                '-',
                target,
                source,
                artifact,
                (*reason != RemovalReason::Pruned).then(|| reason.describe()),
            ),
        };
        Self {
            sign,
            target: target.clone(),
            source: source_name(source).to_owned(),
            artifact: artifact.clone(),
            note: note.map(str::to_owned),
        }
    }

    pub(in crate::cli) fn skipped(change: &SkippedChange) -> Self {
        let (target, source, artifact, note) = match change {
            SkippedChange::Conflict {
                target,
                source,
                artifact,
                ..
            } => (
                target,
                source,
                artifact,
                "skipped, modified locally".to_owned(),
            ),
            SkippedChange::Failed {
                target,
                source,
                artifact,
                message,
            } => (target, source, artifact, format!("failed: {message}")),
            SkippedChange::ReadonlyOverlayRewrite {
                target,
                source,
                artifact,
            } => (
                target,
                source,
                artifact,
                "skipped, state is read-only".to_owned(),
            ),
        };
        Self {
            sign: '!',
            target: target.clone(),
            source: source_name(source).to_owned(),
            artifact: artifact.clone(),
            note: Some(note),
        }
    }

    fn paint_sign(&self, palette: Palette) -> String {
        let sign = self.sign.to_string();
        match self.sign {
            '+' => palette.green(&sign),
            '~' => palette.yellow(&sign),
            _ => palette.red(&sign),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::cli) struct TargetOutcome {
    pub changes: Vec<Change>,
}

struct BindingRow<'a> {
    label: String,
    path: &'a str,
    changes: Vec<&'a Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Fetched {
    Network,
    Cached,
    Failed,
}

#[derive(Debug, Clone)]
pub(in crate::cli) struct SyncView {
    palette: Palette,
    context: RunContext,
    name_width: usize,
    description_width: usize,
    hook_width: usize,
}

const NAME_WIDTH_CAP: usize = 24;
const DESCRIPTION_WIDTH_CAP: usize = 36;
const PATH_WIDTH_CAP: usize = 36;
const HOOK_WIDTH_CAP: usize = 48;

fn widest<'a>(items: impl Iterator<Item = &'a str>, cap: usize) -> usize {
    items
        .map(|item| item.chars().count())
        .max()
        .unwrap_or(0)
        .min(cap)
}

fn pad(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

impl SyncView {
    pub(in crate::cli) fn new(palette: Palette, context: RunContext) -> Self {
        let name_width = widest(
            context
                .sources
                .keys()
                .chain(context.targets.keys())
                .map(|name| source_name(name)),
            NAME_WIDTH_CAP,
        );
        let description_width = widest(
            context.sources.values().map(String::as_str),
            DESCRIPTION_WIDTH_CAP,
        );
        let hook_width = widest(context.hooks.iter().map(String::as_str), HOOK_WIDTH_CAP);
        Self {
            palette,
            context,
            name_width,
            description_width,
            hook_width,
        }
    }

    pub(in crate::cli) fn header(&self) -> String {
        format!(
            "{}  {}",
            self.palette
                .bold(&format!("phora {}", self.context.command)),
            self.palette.dim(&self.context.project)
        )
    }

    pub(in crate::cli) fn section(&self, title: &str) -> String {
        format!("\n{}", self.palette.bold(title))
    }

    fn description(&self, key: &str) -> String {
        self.context
            .sources
            .get(key)
            .cloned()
            .unwrap_or_else(|| "imported".to_owned())
    }

    fn time(&self, elapsed: Duration) -> String {
        self.palette.dim(&format!("{:>6}", format_elapsed(elapsed)))
    }

    fn row(&self, mark: Mark, name: &str, description: &str, note: &str) -> String {
        format!(
            "  {} {}  {}  {}",
            mark.paint(self.palette),
            pad(name, self.name_width),
            self.palette.dim(&pad(description, self.description_width)),
            note
        )
    }

    pub(in crate::cli) fn fetched(&self, key: &str, how: Fetched, elapsed: Duration) -> String {
        let (mark, note) = match how {
            Fetched::Network => (Mark::Done, pad("fetched", 7)),
            Fetched::Cached => (Mark::Done, self.palette.dim(&pad("cached", 7))),
            Fetched::Failed => (Mark::Failed, self.palette.red(&pad("failed", 7))),
        };
        format!(
            "{}  {}",
            self.row(mark, source_name(key), &self.description(key), &note),
            self.time(elapsed)
        )
    }

    pub(in crate::cli) fn cached_summary(&self, keys: &[String]) -> String {
        let names: Vec<&str> = keys.iter().map(|key| source_name(key)).collect();
        format!(
            "  {} {} source{} unchanged  {}",
            Mark::Done.paint(self.palette),
            names.len(),
            plural(names.len()),
            self.palette.dim(&names.join(", "))
        )
    }

    pub(in crate::cli) fn command_run(&self, run: &CommandRun) -> String {
        let mut text = match &run.subject {
            CommandSubject::Build { source } => {
                let (mark, note) = if run.success {
                    (Mark::Done, pad("built", 7))
                } else {
                    (Mark::Failed, self.palette.red(&pad("failed", 7)))
                };
                format!(
                    "{}  {}",
                    self.row(mark, source_name(source), &self.description(source), &note),
                    self.time(run.elapsed)
                )
            }
            CommandSubject::Hook { scope } => {
                let mark = if run.success {
                    Mark::Done
                } else {
                    Mark::Failed
                };
                let mut line = format!(
                    "  {} {}  {}",
                    mark.paint(self.palette),
                    self.palette
                        .dim(&pad(scope_name(*scope), self.name_width.max(10))),
                    pad(&run.command, self.hook_width),
                );
                let note = if run.success {
                    pad("", 7)
                } else {
                    self.palette.red(&pad("failed", 7))
                };
                let _ = write!(line, "  {note}  {}", self.time(run.elapsed));
                line
            }
        };
        if !run.success {
            let bar = self.palette.dim("│");
            if matches!(run.subject, CommandSubject::Build { .. }) {
                let _ = write!(
                    text,
                    "\n    {bar} {}",
                    self.palette.dim(&format!("$ {}", run.command))
                );
            }
            let lines: Vec<&str> = run.output.trim_end().lines().collect();
            let hidden = lines.len().saturating_sub(OUTPUT_LINES);
            if hidden > 0 {
                let _ = write!(
                    text,
                    "\n    {bar} {}",
                    self.palette
                        .dim(&format!("… {hidden} earlier line{}", plural(hidden)))
                );
            }
            for line in &lines[hidden..] {
                let _ = write!(text, "\n    {bar} {line}");
            }
        }
        text
    }

    pub(in crate::cli) fn build_kept(&self) -> String {
        format!("    {}", self.palette.dim("kept the previous output"))
    }

    pub(in crate::cli) fn targets(&self, outcomes: &BTreeMap<String, TargetOutcome>) -> String {
        let mut names: Vec<&String> = self.context.targets.keys().collect();
        names.extend(
            outcomes
                .keys()
                .filter(|name| !self.context.targets.contains_key(*name)),
        );
        names.sort();
        names.dedup();
        let rows: Vec<BindingRow<'_>> = names
            .into_iter()
            .flat_map(|name| self.binding_rows(name, outcomes.get(name)))
            .collect();
        let label_width = widest(
            rows.iter().map(|row| row.label.as_str()),
            NAME_WIDTH_CAP * 2,
        );
        let path_width = widest(rows.iter().map(|row| row.path), PATH_WIDTH_CAP);
        rows.iter()
            .map(|row| self.binding(row, label_width, path_width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn binding_rows<'a>(
        &'a self,
        target: &'a str,
        outcome: Option<&'a TargetOutcome>,
    ) -> Vec<BindingRow<'a>> {
        let path = self.context.targets.get(target).map_or("", String::as_str);
        let mut by_source: BTreeMap<&str, Vec<&Change>> = BTreeMap::new();
        for change in outcome.iter().flat_map(|o| &o.changes) {
            by_source.entry(&change.source).or_default().push(change);
        }
        let quiet: Vec<&str> = self
            .context
            .bindings
            .get(target)
            .into_iter()
            .flatten()
            .map(|source| source_name(source))
            .filter(|source| !by_source.contains_key(source))
            .collect();
        let mut rows: Vec<BindingRow<'a>> = by_source
            .into_iter()
            .map(|(source, changes)| BindingRow {
                label: format!("{source} → {target}"),
                path,
                changes,
            })
            .collect();
        if !quiet.is_empty() || rows.is_empty() {
            let label = if quiet.is_empty() {
                target.to_owned()
            } else {
                format!("{} → {target}", quiet.join(", "))
            };
            rows.push(BindingRow {
                label,
                path,
                changes: Vec::new(),
            });
        }
        rows
    }

    fn binding(&self, row: &BindingRow<'_>, label_width: usize, path_width: usize) -> String {
        let mark = if row.changes.iter().any(|c| c.sign == '!') {
            Mark::Caution
        } else {
            Mark::Done
        };
        let label = match row.label.split_once(" → ") {
            Some((sources, target)) => format!(
                "{sources} {} {target}{}",
                self.palette.dim("→"),
                " ".repeat(label_width.saturating_sub(row.label.chars().count()))
            ),
            None => pad(&row.label, label_width),
        };
        let mut text = format!(
            "  {} {label}  {}  ",
            mark.paint(self.palette),
            self.palette.dim(&pad(row.path, path_width))
        );
        if row.changes.is_empty() {
            text.push_str(&self.palette.dim("up to date"));
            return text;
        }
        let owned: Vec<Change> = row.changes.iter().map(|c| (*c).clone()).collect();
        text.push_str(&self.counts(&owned));
        let mut changes = row.changes.clone();
        changes.sort_by(|a, b| a.artifact.cmp(&b.artifact));
        let width = widest(
            changes
                .iter()
                .take(CHANGES_PER_TARGET)
                .map(|c| c.artifact.as_str()),
            PATH_WIDTH_CAP,
        );
        for change in changes.iter().take(CHANGES_PER_TARGET) {
            let sign = change.paint_sign(self.palette);
            let _ = match &change.note {
                None => write!(text, "\n    {sign} {}", change.artifact),
                Some(note) => {
                    let note = if change.sign == '!' {
                        self.palette.red(note)
                    } else {
                        self.palette.dim(note)
                    };
                    write!(
                        text,
                        "\n    {sign} {}  {note}",
                        pad(&change.artifact, width)
                    )
                }
            };
        }
        if changes.len() > CHANGES_PER_TARGET {
            let _ = write!(
                text,
                "\n    {}",
                self.palette
                    .dim(&format!("… {} more", changes.len() - CHANGES_PER_TARGET))
            );
        }
        text.trim_end().to_owned()
    }

    fn counts(&self, changes: &[Change]) -> String {
        let count = |sign: char| changes.iter().filter(|c| c.sign == sign).count();
        let parts: Vec<String> = [
            (
                count('+'),
                "added",
                Palette::green as fn(Palette, &str) -> String,
            ),
            (count('~'), "updated", Palette::yellow),
            (count('-'), "removed", Palette::red),
            (count('!'), "skipped", Palette::red),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, what, paint)| paint(self.palette, &format!("{n} {what}")))
        .collect();
        parts.join(&self.palette.dim(" · "))
    }

    pub(in crate::cli) fn status(&self, text: &str, elapsed: Duration) -> String {
        let frame = (elapsed.as_millis() / 80) as usize % SPINNER.len();
        format!(
            "{} {text}  {}",
            self.palette.cyan(&SPINNER[frame].to_string()),
            self.palette.dim(&format!("{:.1}s", elapsed.as_secs_f64()))
        )
    }

    pub(in crate::cli) fn diagnostic(&self, severity: Severity, message: &str) -> String {
        let mark = match severity {
            Severity::Warn => Mark::Caution,
            Severity::Error => Mark::Failed,
        };
        format!("  {} {message}", mark.paint(self.palette))
    }
}

fn scope_name(scope: HookScope) -> &'static str {
    match scope {
        HookScope::PreSync => "pre_sync",
        HookScope::PreDeploy => "pre_deploy",
        HookScope::OnChange => "on_change",
        HookScope::PostSync => "post_sync",
    }
}

pub(in crate::cli) fn status_for_phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Build => "Building",
        Phase::Compose => "Composing imported sources",
        Phase::Resolve => "Resolving sources",
        Phase::Project => "Planning",
        Phase::Observe => "Checking targets",
        Phase::Apply => "Deploying",
        Phase::Prune => "Pruning",
        Phase::Hooks => "Running hooks",
    }
}

pub(in crate::cli) fn status_for_command(subject: &CommandSubject, command: &str) -> String {
    match subject {
        CommandSubject::Build { source } => {
            format!("Building {}  {command}", source_name(source))
        }
        CommandSubject::Hook { scope } => {
            format!("Running {} hook  {command}", scope_name(*scope))
        }
    }
}

pub(in crate::cli) fn status_for_resolve(done: usize, planned: usize, active: &[&str]) -> String {
    let mut text = format!("Resolving sources {done}/{planned}");
    if !active.is_empty() {
        let _ = write!(text, "  {}", active.join(", "));
    }
    text
}

pub(in crate::cli) fn status_for_deploy(done: usize, planned: usize, current: &str) -> String {
    format!("Deploying {done}/{planned}  {current}")
}

pub(in crate::cli) fn warnings(palette: Palette, warnings: &[SyncWarning]) -> Option<String> {
    let lines: Vec<String> = warnings
        .iter()
        .filter(|warning| !matches!(warning, SyncWarning::BuildFailed { .. }))
        .filter_map(|warning| match warning {
            SyncWarning::ReferenceMoved {
                source,
                target,
                from,
                to,
            } => Some(format!(
                "{} moved for target {target}: {from} → {to}",
                source_name(source)
            )),
            other => format_sync_warning(other),
        })
        .map(|text| notice(palette, &text))
        .collect();
    (!lines.is_empty()).then(|| format!("\n{}\n{}", palette.bold("Warnings"), lines.join("\n")))
}

pub(in crate::cli) fn notice(palette: Palette, message: &str) -> String {
    let mut lines = message.lines();
    let mut out = format!(
        "  {} {}",
        Mark::Caution.paint(palette),
        lines.next().unwrap_or_default()
    );
    for line in lines {
        let _ = write!(out, "\n    {}", line.trim_start());
    }
    out
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::cli) struct Failures {
    pub builds: usize,
    pub deploys: usize,
    pub hooks: usize,
    pub aborted: bool,
}

pub(in crate::cli) fn failure(palette: Palette, failures: Failures) -> String {
    let parts: Vec<String> = [
        (failures.builds, "build"),
        (failures.deploys, "artifact"),
        (failures.hooks, "hook"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}{} failed", plural(count)))
    .collect();
    let dot = palette.dim(" · ");
    let what = match (parts.is_empty(), failures.aborted) {
        (true, _) => "Sync failed".to_owned(),
        (false, true) => format!("{}{dot}nothing was deployed", parts.join(&dot)),
        (false, false) => format!("{}{dot}everything else is synced", parts.join(&dot)),
    };
    format!("{} {}", palette.red("✗"), palette.bold(&what))
}

pub(in crate::cli) fn outcome(palette: Palette, summary: &SyncSummary) -> String {
    let elapsed = format_elapsed(summary.elapsed);
    let dot = palette.dim(" · ");
    let updated =
        summary.updated + summary.overwritten + summary.overlays_rewritten + summary.ejected;
    let parts: Vec<String> = [
        (summary.deployed, "added"),
        (updated, "updated"),
        (summary.removed, "removed"),
        (summary.conflicts, "skipped"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect();
    let targets = match summary.targets {
        0 => "no targets".to_owned(),
        n => format!("{n} target{}", plural(n)),
    };
    let mut line = if parts.is_empty() {
        format!("{}{dot}{targets}", palette.bold("Up to date"))
    } else if summary.targets == 0 {
        format!("{}{dot}{}", palette.bold("Synced"), parts.join(&dot))
    } else {
        format!(
            "{}{dot}{}",
            palette.bold(&format!("Synced {targets}")),
            parts.join(&dot)
        )
    };
    if summary.unchanged > 0 {
        let _ = write!(
            line,
            "{dot}{}",
            palette.dim(&format!("{} unchanged", summary.unchanged))
        );
    }
    let _ = write!(line, "{dot}{}", palette.dim(&elapsed));
    format!("{} {line}", palette.green("✓"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RunContext {
        RunContext {
            command: "sync",
            project: "~/projects/phora-lab".to_owned(),
            sources: BTreeMap::from([
                ("upstream".to_owned(), "local ./upstream".to_owned()),
                ("generated".to_owned(), "build from upstream".to_owned()),
            ]),
            targets: BTreeMap::from([
                ("home".to_owned(), "deployed/home".to_owned()),
                ("built".to_owned(), "deployed/built".to_owned()),
            ]),
            bindings: BTreeMap::from([
                (
                    "home".to_owned(),
                    vec!["upstream".to_owned(), "dotfiles".to_owned()],
                ),
                ("built".to_owned(), vec!["generated".to_owned()]),
            ]),
            hooks: vec!["sh hooks/noisy.sh".to_owned()],
        }
    }

    fn view() -> SyncView {
        SyncView::new(Palette::plain(), context())
    }

    fn change(sign: char, target: &str, artifact: &str) -> Change {
        Change {
            sign,
            target: target.to_owned(),
            source: "upstream".to_owned(),
            artifact: artifact.to_owned(),
            note: None,
        }
    }

    #[test]
    fn source_rows_align_name_description_note_and_time() {
        assert_eq!(
            view().fetched("upstream", Fetched::Network, Duration::from_millis(90)),
            "  ✓ upstream   local ./upstream     fetched    90ms"
        );
    }

    #[test]
    fn a_composed_source_reads_as_its_member_name() {
        assert_eq!(source_name("6c47a54af1ab7fe5%harnesses"), "harnesses");
        assert!(
            view()
                .fetched("6c47a54af1ab7fe5%loqui", Fetched::Cached, Duration::ZERO)
                .contains("loqui      imported")
        );
    }

    #[test]
    fn a_failed_build_shows_its_command_and_output_under_a_bar() {
        let run = CommandRun {
            subject: CommandSubject::Build {
                source: "generated".to_owned(),
            },
            command: "sh build.sh".to_owned(),
            success: false,
            elapsed: Duration::from_secs(2),
            output: "compiling\nerror: unit 3\n\n".to_owned(),
        };
        assert_eq!(
            view().command_run(&run),
            "  ✗ generated  build from upstream  failed     2.0s\n    \
             │ $ sh build.sh\n    │ compiling\n    │ error: unit 3"
        );
    }

    #[test]
    fn a_long_failure_keeps_its_last_lines() {
        let run = CommandRun {
            subject: CommandSubject::Hook {
                scope: HookScope::PostSync,
            },
            command: "sh hooks/noisy.sh".to_owned(),
            success: false,
            elapsed: Duration::from_secs(1),
            output: (1..=25).fold(String::new(), |mut out, n| {
                let _ = writeln!(out, "line {n}");
                out
            }),
        };
        let text = view().command_run(&run);
        assert!(
            text.contains("│ … 5 earlier lines\n    │ line 6\n"),
            "{text}"
        );
        assert!(text.ends_with("line 25"));
    }

    #[test]
    fn hooks_align_scope_command_and_time() {
        let run = CommandRun {
            subject: CommandSubject::Hook {
                scope: HookScope::PostSync,
            },
            command: "sh hooks/noisy.sh".to_owned(),
            success: true,
            elapsed: Duration::from_millis(1040),
            output: "noise\n".to_owned(),
        };
        assert_eq!(
            view().command_run(&run),
            "  ✓ post_sync   sh hooks/noisy.sh             1.0s"
        );
    }

    #[test]
    fn targets_read_as_source_to_target_with_changes_nested() {
        let outcomes = BTreeMap::from([(
            "home".to_owned(),
            TargetOutcome {
                changes: vec![change('+', "home", "shell"), change('-', "home", "editor")],
            },
        )]);
        assert_eq!(
            view().targets(&outcomes),
            "  ✓ generated → built  deployed/built  up to date\n  \
             ✓ upstream → home    deployed/home   1 added · 1 removed\n    \
             - editor\n    \
             + shell\n  \
             ✓ dotfiles → home    deployed/home   up to date"
        );
    }

    #[test]
    fn a_target_caps_its_change_list() {
        let changes = (0..11)
            .map(|n| change('+', "home", &format!("a{n:02}")))
            .collect();
        let outcomes = BTreeMap::from([("home".to_owned(), TargetOutcome { changes })]);
        assert!(view().targets(&outcomes).contains("+ a07\n    … 3 more\n"));
    }

    #[test]
    fn the_outcome_states_what_changed_or_that_nothing_did() {
        let mut summary = SyncSummary {
            targets: 2,
            unchanged: 612,
            elapsed: Duration::from_millis(9800),
            ..SyncSummary::default()
        };
        assert_eq!(
            outcome(Palette::plain(), &summary),
            "✓ Up to date · 2 targets · 612 unchanged · 9.8s"
        );
        summary.overwritten = 3;
        summary.removed = 5;
        assert_eq!(
            outcome(Palette::plain(), &summary),
            "✓ Synced 2 targets · 3 updated · 5 removed · 612 unchanged · 9.8s"
        );
    }

    #[test]
    fn a_run_without_targets_never_claims_to_sync_them() {
        let mut summary = SyncSummary {
            elapsed: Duration::from_millis(5),
            ..SyncSummary::default()
        };
        assert_eq!(
            outcome(Palette::plain(), &summary),
            "✓ Up to date · no targets · 5ms"
        );
        summary.removed = 2;
        assert_eq!(
            outcome(Palette::plain(), &summary),
            "✓ Synced · 2 removed · 5ms"
        );
    }

    #[test]
    fn a_prune_only_target_counts_its_removals() {
        let outcomes = BTreeMap::from([(
            "home".to_owned(),
            TargetOutcome {
                changes: vec![change('-', "home", "editor"), change('-', "home", "lint")],
            },
        )]);
        assert!(
            view().targets(&outcomes).contains(
                "✓ upstream → home    deployed/home   2 removed\n    - editor\n    - lint"
            ),
            "{}",
            view().targets(&outcomes)
        );
    }

    #[test]
    fn failures_are_counted_by_kind() {
        let failures = Failures {
            builds: 1,
            hooks: 2,
            ..Failures::default()
        };
        assert_eq!(
            failure(Palette::plain(), failures),
            "✗ 1 build failed · 2 hooks failed · everything else is synced"
        );
        assert_eq!(
            failure(
                Palette::plain(),
                Failures {
                    hooks: 1,
                    aborted: true,
                    ..Failures::default()
                }
            ),
            "✗ 1 hook failed · nothing was deployed"
        );
    }

    #[test]
    fn warnings_drop_the_phora_prefix_and_kept_builds() {
        let block = warnings(
            Palette::plain(),
            &[
                SyncWarning::UnboundSource {
                    source: "fzf".to_owned(),
                },
                SyncWarning::BuildFailed {
                    source: "generated".to_owned(),
                    detail: "exit 1".to_owned(),
                },
            ],
        );
        assert_eq!(
            block.as_deref(),
            Some("\nWarnings\n  ! source `fzf` is bound by no target or build")
        );
    }

    #[test]
    fn the_home_directory_reads_as_a_bare_tilde() {
        let home = dirs::home_dir().expect("home directory");
        assert_eq!(display_path(&home, Path::new("")), "~");
        assert_eq!(
            display_path(&home.join("dotfiles"), Path::new("")),
            "~/dotfiles"
        );
    }

    #[test]
    fn paths_read_relative_to_the_project() {
        assert_eq!(
            display_path(Path::new("/p/lab/deployed/home"), Path::new("/p/lab")),
            "deployed/home"
        );
    }
}
