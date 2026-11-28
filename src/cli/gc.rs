//! `phora gc`: review the state dirs that are not live and drop the dead ones.

use std::io::IsTerminal;

use clap::Args;

use crate::error::Result;
use crate::sync::content;
use crate::sync::state::lifecycle::{
    self, Confirmation, Fact, GcInput, GcOutcome, GcPlan, GcRecord, PlannedAction, Reason,
    StateDirPath,
};

use super::CliOutcome;

#[derive(Args, Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent CLI flags, not a state machine"
)]
pub struct GcArgs {
    /// Also eject the history worktrees of retired and stranded dirs, then drop every dir that is not live.
    #[arg(long)]
    eject: bool,
    /// Confirm `--eject` without a prompt.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Print the plan and change nothing.
    #[arg(long, short = 'n')]
    dry_run: bool,
    /// Emit one NDJSON record per state dir on stdout.
    #[arg(long)]
    json: bool,
}

pub(super) fn run_gc(args: GcArgs) -> Result<CliOutcome> {
    let cwd = std::env::current_dir()?;
    let default = super::lexical_state_root(None, &cwd)?;
    let input = GcInput::new(
        lifecycle::sweep_input(default.clone(), None),
        args.eject,
        args.dry_run,
    );
    let plan = lifecycle::plan_gc(&input);
    let mut failed = false;
    for fault in plan.faults() {
        eprintln!("phora: gc: {fault}");
        failed = true;
    }
    let listed: Vec<(StateDirPath, serde_json::Value)> = plan
        .records()
        .iter()
        .map(|record| {
            (
                record.action().dir().clone(),
                super::json::gc_record(record),
            )
        })
        .collect();
    if args.json {
        emit_untouched(&plan);
    } else {
        print_plan(&plan, args.eject);
    }
    print_notes(&plan, args.json);
    if args.dry_run {
        if args.json {
            emit_json(&listed, &GcOutcome::default(), args.dry_run);
        }
        return Ok(outcome(failed));
    }

    let confirmation = confirm(&plan, args);
    let granted = confirmation == Confirmation::Granted;
    let removals = plan.index_removals().to_vec();
    let applied = lifecycle::apply_gc(plan, confirmation);
    let pruned = super::cache::project_paths(&cwd)
        .and_then(|paths| super::cache::store_roots(&paths, &cwd))
        .and_then(|(store, projects)| content::prune_roots(&store, &projects, false));
    match pruned {
        Ok(report) => eprintln!("phora: {}", super::cache::summary(&report, false)),
        Err(error) => {
            eprintln!("phora: gc: content store prune: {error}");
            failed = true;
        }
    }
    if granted && let Err(error) = lifecycle::unregister_roots(&default, &removals) {
        eprintln!("phora: gc: unregister absent state roots: {error}");
        failed = true;
    }
    if args.json {
        emit_json(&listed, &applied, args.dry_run);
    } else {
        print_outcome(&applied);
    }
    for fault in applied.faults() {
        eprintln!("phora: gc: {fault}");
    }
    Ok(outcome(failed || applied.failed()))
}

fn outcome(failed: bool) -> CliOutcome {
    if failed {
        CliOutcome::Failure
    } else {
        CliOutcome::Success
    }
}

/// Drop-class drops and link releases need no confirmation; the rest needs `--yes` or a TTY answer.
fn confirm(plan: &GcPlan, args: GcArgs) -> Confirmation {
    if !args.eject {
        return Confirmation::Withheld;
    }
    let granted = args.yes
        || (plan.needs_confirmation()
            && std::io::stdin().is_terminal()
            && super::prompt_yes_on_stdin(
                "phora: eject the history worktrees listed above, drop every listed dir and \
                 unregister the absent state roots (see the notes above)? ",
                false,
            ));
    if granted {
        Confirmation::Granted
    } else {
        Confirmation::Withheld
    }
}

fn print_plan(plan: &GcPlan, eject: bool) {
    for record in plan.records() {
        print_record(record);
    }
    for path in plan.foreign() {
        println!("not a state dir, untouched: {}", path.as_path().display());
    }
    for name in plan.foreign_trash() {
        println!("not a phora trash entry, untouched: {}", name.display());
    }
    for root in plan.index_removals() {
        let fate = if eject {
            "unregistered once confirmed"
        } else {
            "`phora gc --eject` unregisters it"
        };
        println!(
            "registered state root absent: {} ({fate})",
            root.path().as_path().display()
        );
    }
}

fn print_record(record: &GcRecord) {
    let action = record.action();
    let root = action.root().map_or_else(
        || "-".to_owned(),
        |root| root.as_path().display().to_string(),
    );
    println!(
        "{} {} ({}) root {root}",
        action.dir().id().as_str(),
        action.reason().label(),
        action.class(),
    );
    for (key, fact) in record.evidence() {
        match fact {
            Fact::Text(text) => println!("    {key}: {text}"),
            Fact::Count(count) => println!("    {key}: {count}"),
            Fact::List(items) if items.is_empty() => {}
            Fact::List(items) => {
                for item in items {
                    println!("    {key}: {item}");
                }
            }
        }
    }
    println!("    action: {}", action_text(action));
    for worktree in action.worktrees() {
        let what = if worktree.mirror_present() {
            "eject into a standalone clone"
        } else {
            "keep linked, its mirror is gone"
        };
        println!(
            "    worktree: {} ({what})",
            worktree.deploy_root().as_path().display()
        );
    }
    for path in record.leftovers() {
        println!("    leaves: {}", path.display());
    }
}

fn action_text(action: &PlannedAction) -> &'static str {
    match action {
        PlannedAction::Drop { .. } => "drop",
        PlannedAction::ReleaseLinks { .. } => "release the dangling links, keep the dir",
        PlannedAction::Eject { .. } => "eject the history worktrees, then drop",
        PlannedAction::Forget { .. } => "drop without ejecting",
        PlannedAction::Report { .. } => "keep; `phora gc --eject` drops it",
    }
}

fn print_notes(plan: &GcPlan, json: bool) {
    let listed = |reason: Reason| {
        plan.records()
            .iter()
            .any(|record| record.action().reason() == reason)
    };
    let notes = [
        (
            Reason::Unrooted,
            "an unrooted dir may be a live project not run since the upgrade; run a locked \
             command such as `phora sync` in it first",
        ),
        (
            Reason::Unreachable,
            "an unreachable dir may belong to a project on an unmounted volume",
        ),
        (
            Reason::Retired,
            "a retired dir may be a live project on a branch without phora.toml",
        ),
    ];
    for (reason, note) in notes {
        if !listed(reason) {
            continue;
        }
        if json {
            eprintln!("phora: note: {note}");
        } else {
            println!("note: {note}");
        }
    }
}

fn print_outcome(outcome: &GcOutcome) {
    for (dir, _) in outcome.applied() {
        if let Some(result) = outcome.describe(dir) {
            println!("{}: {result}", dir.id().as_str());
        }
        for (path, _) in outcome.links_kept(dir) {
            println!("    leaves: {}", path.as_path().display());
        }
    }
    for dir in outcome.pending() {
        println!(
            "{}: pending; confirm with `phora gc --eject --yes`",
            dir.id().as_str()
        );
    }
}

/// Entries gc lists but does not drop: foreign entries and absent registered roots.
fn emit_untouched(plan: &GcPlan) {
    let foreign = plan
        .foreign()
        .iter()
        .map(|path| path.as_path().to_path_buf());
    let trash = plan.foreign_trash().iter().map(std::path::PathBuf::from);
    for path in foreign.chain(trash) {
        println!("{}", super::json::gc_entry("gc_foreign", &path));
    }
    for root in plan.index_removals() {
        println!(
            "{}",
            super::json::gc_entry("gc_absent_root", root.path().as_path())
        );
    }
}

fn emit_json(listed: &[(StateDirPath, serde_json::Value)], outcome: &GcOutcome, dry_run: bool) {
    for (listed_dir, record) in listed {
        let applied = outcome.describe(listed_dir);
        let result = match (dry_run, outcome.pending().contains(listed_dir), applied) {
            (true, _, _) => None,
            (false, true, Some(applied)) => Some(format!("pending; {applied}")),
            (false, true, None) => Some("pending".to_owned()),
            (false, false, applied) => applied,
        };
        let mut record = record.clone();
        if let Some(leftovers) = record["leftovers"].as_array_mut() {
            for (path, _) in outcome.links_kept(listed_dir) {
                let path = serde_json::Value::String(path.as_path().display().to_string());
                if !leftovers.contains(&path) {
                    leftovers.push(path);
                }
            }
        }
        record["outcome"] = result.map_or(serde_json::Value::Null, serde_json::Value::String);
        println!("{record}");
    }
}
