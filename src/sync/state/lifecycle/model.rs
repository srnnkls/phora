use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::mount::{AnchorDir, DeletionProof, NormalPath, Placement, Unproven};
use crate::source::{Commit, WorktreeAdminId, WorktreeMirrorAddress};
use crate::sync::state::{ArtifactKey, ProjectId, StateError, StateLock};

/// A recorded root whose path hashes to its state dir's `ProjectId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoot(NormalPath);

impl ProjectRoot {
    pub fn recorded(root: NormalPath, id: &ProjectId) -> Option<Self> {
        (ProjectId::of_canonical(&root) == *id).then_some(Self(root))
    }

    /// The project's own root, in its own run.
    pub fn canonicalize(path: &Path) -> std::io::Result<Self> {
        let canonical = path.canonicalize()?;
        NormalPath::lexical(&canonical).map(Self).ok_or_else(|| {
            std::io::Error::other(format!(
                "canonical root {} is relative",
                canonical.display()
            ))
        })
    }

    pub fn id(&self) -> ProjectId {
        ProjectId::of_canonical(&self.0)
    }

    pub fn path(&self) -> &NormalPath {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StateRoot(NormalPath);

impl StateRoot {
    pub fn new(path: NormalPath) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &NormalPath {
        &self.0
    }
}

/// `<root>/projects/<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDirPath {
    root: StateRoot,
    id: ProjectId,
}

impl StateDirPath {
    pub fn new(root: StateRoot, id: ProjectId) -> Self {
        Self { root, id }
    }

    pub fn root(&self) -> &StateRoot {
        &self.root
    }

    pub fn id(&self) -> &ProjectId {
        &self.id
    }

    pub fn path(&self) -> NormalPath {
        self.root.0.join("projects").join(self.id.as_str())
    }
}

#[derive(Debug)]
pub struct ProjectMeta {
    root: ProjectRoot,
    placement: Placement,
    state_dir: AnchorDir,
}

impl ProjectMeta {
    pub fn new(root: ProjectRoot, placement: Placement, state_dir: AnchorDir) -> Self {
        Self {
            root,
            placement,
            state_dir,
        }
    }

    pub fn root(&self) -> &ProjectRoot {
        &self.root
    }

    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    pub fn state_dir(&self) -> &AnchorDir {
        &self.state_dir
    }
}

#[derive(Debug)]
pub enum Succession {
    Current,
    SupersededBy(StateRoot),
}

#[derive(Debug, Clone)]
pub enum WireDefect {
    Unparsable,
    UnsupportedVersion(u32),
    BadHex,
    NotAbsolute,
}

#[derive(Debug, Clone)]
pub enum MetaDefect {
    Wire(WireDefect),
    RootHashMismatch,
}

#[derive(Debug, Clone)]
pub enum RootlessMeta {
    Absent,
    Invalid(MetaDefect),
}

#[derive(Debug)]
pub enum MetaRead {
    Rootless(RootlessMeta),
    Valid {
        meta: ProjectMeta,
        succession: Succession,
    },
}

#[derive(Debug)]
pub struct StateRootIndex {
    /// The default state root's path as recorded, passed to `locate` for entries.
    anchor: AnchorDir,
    roots: BTreeMap<StateRoot, Placement>,
}

impl StateRootIndex {
    pub fn new(anchor: AnchorDir, roots: BTreeMap<StateRoot, Placement>) -> Self {
        Self { anchor, roots }
    }

    pub fn anchor(&self) -> &AnchorDir {
        &self.anchor
    }

    pub fn roots(&self) -> &BTreeMap<StateRoot, Placement> {
        &self.roots
    }

    /// Records `root` with its placement; `false` when the index already holds exactly that.
    pub fn register(&mut self, root: StateRoot, placement: Placement) -> bool {
        self.roots.insert(root, placement) != Some(placement)
    }

    pub fn unregister(&mut self, root: &StateRoot) {
        self.roots.retain(|registered, _| registered != root);
    }
}

#[derive(Debug)]
pub enum IndexRead {
    /// No registered roots.
    Missing,
    /// Sweep the default root only, drop no entry, warn.
    Corrupt(WireDefect),
    Unreadable(ErrorKind),
    Read(StateRootIndex),
}

#[derive(Debug)]
pub enum Probed {
    Present,
    Absent,
    Fault(ErrorKind),
}

#[derive(Debug, Clone)]
pub enum JournalState {
    Clear,
    Pending { entries: NonZeroUsize },
}

#[derive(Debug)]
pub struct LegacyEvidence {
    pub(super) records: usize,
    pub(super) recorded_paths: Vec<PathBuf>,
    pub(super) newest_record: Option<SystemTime>,
}

#[derive(Debug)]
pub enum StateDirProbe {
    /// `targets/`, a record, `project.toml` or the journal failed to read.
    Unreadable(ErrorKind),
    /// No records, no target meta, a clear journal; any meta.
    Empty,
    Unrooted {
        meta: RootlessMeta,
    },
    Rooted {
        meta: ProjectMeta,
        succession: Succession,
        root: RootObservation,
    },
}

#[derive(Debug)]
pub enum RootObservation {
    Present {
        manifest: Probed,
    },
    Deleted {
        proof: DeletionProof,
        remains: Remains,
    },
    Unproven(Unproven),
}

/// Every record of a root-deleted dir.
#[derive(Debug)]
pub struct Remains {
    journal: JournalState,
    records: Vec<RecordFate>,
}

impl Remains {
    pub fn new(journal: JournalState, records: Vec<RecordFate>) -> Self {
        Self { journal, records }
    }

    /// Every destination still present, copies and links alike.
    pub(super) fn present(&self) -> Vec<NormalPath> {
        self.records
            .iter()
            .filter_map(|fate| match fate {
                RecordFate::PresentOutside(path) => Some(path.clone()),
                RecordFate::Releasable(link) => Some(link.path.clone()),
                RecordFate::Ejected
                | RecordFate::Absent
                | RecordFate::Unresolvable { .. }
                | RecordFate::Fault { .. } => None,
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum RecordFate {
    Ejected,
    Absent,
    Releasable(ReleasableLink),
    PresentOutside(NormalPath),
    Unresolvable {
        key: ArtifactKey,
    },
    /// Destination stat failed other than `NotFound`.
    Fault {
        key: ArtifactKey,
        kind: ErrorKind,
    },
}

#[derive(Debug, Clone)]
pub struct ReleasableLink {
    pub(super) path: NormalPath,
    pub(super) link_target: NormalPath,
    pub(super) anchor: NormalPath,
}

#[derive(Debug)]
pub enum StateDirClass {
    Keep(KeepReason),
    Drop(DropReason),
    Release(ReleaseReason),
    Forget(ForgetReason),
}

impl StateDirClass {
    pub fn reason(&self) -> Reason {
        match self {
            Self::Keep(KeepReason::Live) => Reason::Live,
            Self::Drop(DropReason::Empty) => Reason::Empty,
            Self::Drop(DropReason::Gone { .. }) => Reason::Gone,
            Self::Release(ReleaseReason::Retired { .. }) => Reason::Retired,
            Self::Release(ReleaseReason::Stranded(_)) => Reason::Stranded,
            Self::Forget(ForgetReason::Unrooted { .. }) => Reason::Unrooted,
            Self::Forget(ForgetReason::Abandoned { .. }) => Reason::Abandoned,
            Self::Forget(ForgetReason::Unreachable(_)) => Reason::Unreachable,
        }
    }
}

/// The first matching row of the classification table.
pub fn classify(probe: &StateDirProbe) -> StateDirClass {
    let (meta, succession, observation) = match probe {
        StateDirProbe::Unreadable(kind) => {
            return StateDirClass::Forget(ForgetReason::Unreachable(Unreachable::Unreadable(
                *kind,
            )));
        }
        StateDirProbe::Empty => return StateDirClass::Drop(DropReason::Empty),
        StateDirProbe::Unrooted { meta } => {
            return StateDirClass::Forget(ForgetReason::Unrooted { meta: meta.clone() });
        }
        StateDirProbe::Rooted {
            meta,
            succession,
            root,
        } => (meta, succession, root),
    };
    let root = meta.root().clone();
    match (succession, observation) {
        (
            Succession::SupersededBy(successor),
            RootObservation::Present { .. } | RootObservation::Unproven(_),
        ) => StateDirClass::Forget(ForgetReason::Abandoned {
            root,
            successor: successor.clone(),
        }),
        (_, RootObservation::Present { manifest }) => match manifest {
            Probed::Present => StateDirClass::Keep(KeepReason::Live),
            Probed::Absent => StateDirClass::Release(ReleaseReason::Retired { root }),
            Probed::Fault(kind) => unreachable_root(root, UnreachableRoot::ManifestFault(*kind)),
        },
        (_, RootObservation::Unproven(cause)) => {
            unreachable_root(root, UnreachableRoot::Unproven(cause.clone()))
        }
        (_, RootObservation::Deleted { proof, remains }) => deleted(root, proof, remains),
    }
}

fn unreachable_root(root: ProjectRoot, cause: UnreachableRoot) -> StateDirClass {
    StateDirClass::Forget(ForgetReason::Unreachable(Unreachable::Root { root, cause }))
}

/// The `gone` and `stranded` rows for a root proven deleted.
fn deleted(root: ProjectRoot, proof: &DeletionProof, remains: &Remains) -> StateDirClass {
    let mut links = Vec::new();
    let mut present_outside = Vec::new();
    let mut unresolvable = Vec::new();
    for fate in &remains.records {
        match fate {
            RecordFate::Ejected | RecordFate::Absent => {}
            RecordFate::Releasable(link) => links.push(link.clone()),
            RecordFate::PresentOutside(path) => present_outside.push(path.clone()),
            RecordFate::Unresolvable { key } => unresolvable.push(key.clone()),
            RecordFate::Fault { key, kind } => {
                let cause = UnreachableRoot::DestinationFault {
                    key: key.clone(),
                    kind: *kind,
                };
                return unreachable_root(root, cause);
            }
        }
    }
    let proof = proof.clone();
    match StrandedEvidence::new(present_outside, unresolvable, remains.journal.clone()) {
        Some(evidence) => StateDirClass::Release(ReleaseReason::Stranded(Stranded {
            proof,
            links,
            evidence,
        })),
        None => StateDirClass::Drop(DropReason::Gone { proof, links }),
    }
}

#[derive(Debug)]
pub enum KeepReason {
    Live,
}

#[derive(Debug)]
pub enum DropReason {
    Empty,
    Gone {
        proof: DeletionProof,
        links: Vec<ReleasableLink>,
    },
}

#[derive(Debug)]
pub enum ReleaseReason {
    Retired { root: ProjectRoot },
    Stranded(Stranded),
}

#[derive(Debug)]
pub struct Stranded {
    pub(super) proof: DeletionProof,
    pub(super) links: Vec<ReleasableLink>,
    evidence: StrandedEvidence,
}

/// At least one part is non-empty.
#[derive(Debug)]
pub struct StrandedEvidence {
    present_outside: Vec<NormalPath>,
    unresolvable: Vec<ArtifactKey>,
    journal: JournalState,
}

impl StrandedEvidence {
    pub fn new(
        present_outside: Vec<NormalPath>,
        unresolvable: Vec<ArtifactKey>,
        journal: JournalState,
    ) -> Option<Self> {
        let holds = !present_outside.is_empty()
            || !unresolvable.is_empty()
            || matches!(journal, JournalState::Pending { .. });
        holds.then_some(Self {
            present_outside,
            unresolvable,
            journal,
        })
    }
}

#[derive(Debug)]
pub enum ForgetReason {
    Unrooted {
        meta: RootlessMeta,
    },
    Abandoned {
        root: ProjectRoot,
        successor: StateRoot,
    },
    Unreachable(Unreachable),
}

#[derive(Debug)]
pub enum Unreachable {
    Unreadable(ErrorKind),
    Root {
        root: ProjectRoot,
        cause: UnreachableRoot,
    },
}

#[derive(Debug)]
pub enum UnreachableRoot {
    Unproven(Unproven),
    /// Root present.
    ManifestFault(ErrorKind),
    /// Root deleted; the first `RecordFate::Fault` in `Remains`.
    DestinationFault {
        key: ArtifactKey,
        kind: ErrorKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    Empty,
    Unrooted,
    Abandoned,
    Live,
    Retired,
    Unreachable,
    Stranded,
    Gone,
}

impl Reason {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Unrooted => "unrooted",
            Self::Abandoned => "abandoned",
            Self::Live => "live",
            Self::Retired => "retired",
            Self::Unreachable => "unreachable",
            Self::Stranded => "stranded",
            Self::Gone => "gone",
        }
    }

    /// Whether a sweep that leaves a dir with this reason counts it in its warning.
    fn needs_attention(self) -> bool {
        !matches!(self, Self::Empty | Self::Live | Self::Gone)
    }
}

#[derive(Debug)]
pub enum PlannedAction {
    Drop {
        dir: StateDirPath,
        reason: DropReason,
    },
    /// Only when `stranded.links` is non-empty.
    ReleaseLinks {
        dir: StateDirPath,
        stranded: Stranded,
    },
    /// `gc --eject`.
    Eject {
        dir: StateDirPath,
        reason: ReleaseReason,
        worktrees: Vec<EjectableWorktree>,
    },
    /// `gc --eject`.
    Forget {
        dir: StateDirPath,
        reason: ForgetReason,
    },
    Report {
        dir: StateDirPath,
        attention: Attention,
    },
}

impl PlannedAction {
    pub fn dir(&self) -> &StateDirPath {
        let (Self::Drop { dir, .. }
        | Self::ReleaseLinks { dir, .. }
        | Self::Eject { dir, .. }
        | Self::Forget { dir, .. }
        | Self::Report { dir, .. }) = self;
        dir
    }
}

#[derive(Debug)]
pub enum Attention {
    Retired { root: ProjectRoot },
    Stranded(Stranded),
    Forget(ForgetReason),
}

#[derive(Debug)]
pub struct EjectableWorktree {
    pub(super) record: ArtifactKey,
    pub(super) deploy_root: NormalPath,
    pub(super) admin_id: WorktreeAdminId,
    pub(super) address: WorktreeMirrorAddress,
    pub(super) commit: Commit,
    /// Checked without locking; a worktree whose mirror is gone keeps its gitlink.
    pub(super) mirror_present: bool,
}

impl EjectableWorktree {
    #[must_use]
    pub fn deploy_root(&self) -> &NormalPath {
        &self.deploy_root
    }

    #[must_use]
    pub fn mirror_present(&self) -> bool {
        self.mirror_present
    }
}

#[derive(Debug)]
pub struct Held {
    pub(super) dir: StateDirPath,
    pub(super) lock: StateLock,
}

#[derive(Debug)]
pub enum NotHeld {
    Vanished,
    Busy,
    Fault(ErrorKind),
}

#[derive(Debug)]
pub struct Confirmed {
    pub(super) held: Held,
    pub(super) class: StateDirClass,
}

#[derive(Debug)]
pub struct Trashed {
    pub(super) entry: TrashEntry,
    pub(super) lock: StateLock,
}

#[derive(Debug)]
pub struct Leftover {
    pub(super) entry: TrashEntry,
}

/// Unique per rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashSuffix(String);

impl TrashSuffix {
    pub(super) fn new(suffix: String) -> Self {
        Self(suffix)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `<root>/trash/<id>.<suffix>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    pub(super) root: StateRoot,
    pub(super) id: ProjectId,
    pub(super) suffix: TrashSuffix,
}

#[derive(Debug)]
pub enum ProjectsEntry {
    State(StateDirPath),
    Foreign(NormalPath),
}

/// Only `Entry` is ever deleted.
#[derive(Debug)]
pub enum TrashName {
    Entry(TrashEntry),
    Foreign(OsString),
}

#[derive(Debug)]
pub enum DropOutcome {
    Deleted,
    Leftover(Leftover),
    Released(Vec<LinkOutcome>),
    Vanished,
    Busy,
    Reclassified(Reason),
    Fault(StateError),
}

/// Links come from `Confirmed.class`, so the re-probe already dropped re-pointed links.
#[derive(Debug, Clone)]
pub enum LinkOutcome {
    Unlinked,
    Kept(LinkKept),
}

#[derive(Debug, Clone)]
pub enum LinkKept {
    Confined,
    Fault(ErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    Granted,
    Withheld,
}

/// The index path and the swept roots derive from `default` and `index`.
#[derive(Debug)]
pub struct SweepInput {
    pub(super) default: StateRoot,
    pub(super) index: IndexRead,
    pub(super) own: Option<StateDirPath>,
}

#[derive(Debug)]
pub struct SweepFault {
    dir: Option<StateDirPath>,
    error: StateError,
}

impl SweepFault {
    pub(super) fn new(dir: Option<StateDirPath>, error: StateError) -> Self {
        Self { dir, error }
    }

    /// The fault, prefixed with its dir when it has one.
    fn describe(&self) -> String {
        match &self.dir {
            Some(dir) => format!("{}: {}", dir.path().as_path().display(), self.error),
            None => self.error.to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub(super) dropped: Vec<StateDirPath>,
    pub(super) released: Vec<NormalPath>,
    pub(super) links_kept: Vec<(NormalPath, LinkOutcome)>,
    pub(super) busy: Vec<StateDirPath>,
    pub(super) leftovers: Vec<TrashEntry>,
    attention: BTreeMap<Reason, NonZeroUsize>,
    faults: Vec<SweepFault>,
}

impl SweepReport {
    /// Counts a dir the sweep leaves in place, when its reason needs attention.
    pub(super) fn attend(&mut self, reason: Reason) {
        if reason.needs_attention() {
            self.attention
                .entry(reason)
                .and_modify(|count| *count = count.saturating_add(1))
                .or_insert(NonZeroUsize::MIN);
        }
    }

    pub(super) fn fault(&mut self, dir: Option<StateDirPath>, error: StateError) {
        self.faults.push(SweepFault::new(dir, error));
    }

    /// The faults and trash leftovers, each as a fault.
    pub(super) fn into_faults(self) -> Vec<SweepFault> {
        let leftovers = self.leftovers.into_iter().map(|entry| {
            let path = super::trash_path(&entry);
            SweepFault::new(
                None,
                StateError::StateStore(format!(
                    "trash entry {} left for the next pass",
                    path.display()
                )),
            )
        });
        self.faults.into_iter().chain(leftovers).collect()
    }

    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped.len()
    }

    /// The count of each reason the sweep left for attention.
    #[must_use]
    pub fn attention(&self) -> &BTreeMap<Reason, NonZeroUsize> {
        &self.attention
    }

    #[must_use]
    pub fn released_links(&self) -> usize {
        self.released.len()
    }

    #[must_use]
    pub fn busy(&self) -> usize {
        self.busy.len()
    }

    #[must_use]
    pub fn leftovers(&self) -> usize {
        self.leftovers.len()
    }

    /// Each fault, prefixed with its dir when it has one.
    pub fn faults(&self) -> impl Iterator<Item = String> + '_ {
        self.faults.iter().map(SweepFault::describe)
    }
}

#[derive(Debug)]
pub struct GcInput {
    pub(super) sweep: SweepInput,
    pub(super) eject: bool,
    pub(super) dry_run: bool,
}

impl GcInput {
    #[must_use]
    pub fn new(sweep: SweepInput, eject: bool, dry_run: bool) -> Self {
        Self {
            sweep,
            eject,
            dry_run,
        }
    }
}

#[derive(Debug)]
pub enum GcRecord {
    /// Evidence read by `plan_gc`, never by the sweep.
    Unrooted {
        action: PlannedAction,
        legacy: LegacyEvidence,
    },
    Rooted {
        action: PlannedAction,
        leftovers: Vec<NormalPath>,
    },
}

/// One fact behind a reason, as `gc` lists it.
#[derive(Debug)]
pub enum Fact {
    Text(String),
    Count(usize),
    List(Vec<String>),
}

/// What a planned action is about: the reason it carries.
enum Subject<'a> {
    Drop(&'a DropReason),
    Retired(&'a ProjectRoot),
    Stranded(&'a Stranded),
    Forget(&'a ForgetReason),
}

impl PlannedAction {
    fn subject(&self) -> Subject<'_> {
        match self {
            Self::Drop { reason, .. } => Subject::Drop(reason),
            Self::ReleaseLinks { stranded, .. }
            | Self::Eject {
                reason: ReleaseReason::Stranded(stranded),
                ..
            }
            | Self::Report {
                attention: Attention::Stranded(stranded),
                ..
            } => Subject::Stranded(stranded),
            Self::Eject {
                reason: ReleaseReason::Retired { root },
                ..
            }
            | Self::Report {
                attention: Attention::Retired { root },
                ..
            } => Subject::Retired(root),
            Self::Forget { reason, .. }
            | Self::Report {
                attention: Attention::Forget(reason),
                ..
            } => Subject::Forget(reason),
        }
    }

    /// The reason the action was planned for, which the re-probe must still find.
    #[must_use]
    pub fn reason(&self) -> Reason {
        match self.subject() {
            Subject::Drop(DropReason::Empty) => Reason::Empty,
            Subject::Drop(DropReason::Gone { .. }) => Reason::Gone,
            Subject::Retired(_) => Reason::Retired,
            Subject::Stranded(_) => Reason::Stranded,
            Subject::Forget(ForgetReason::Unrooted { .. }) => Reason::Unrooted,
            Subject::Forget(ForgetReason::Abandoned { .. }) => Reason::Abandoned,
            Subject::Forget(ForgetReason::Unreachable(_)) => Reason::Unreachable,
        }
    }

    #[must_use]
    pub fn class(&self) -> &'static str {
        match self.subject() {
            Subject::Drop(_) => "drop",
            Subject::Retired(_) | Subject::Stranded(_) => "release",
            Subject::Forget(_) => "forget",
        }
    }

    pub(super) fn present_outside(&self) -> Option<&[NormalPath]> {
        match self.subject() {
            Subject::Stranded(stranded) => Some(&stranded.evidence.present_outside),
            _ => None,
        }
    }

    /// The recorded root of a dir whose root is not proven deleted.
    pub(super) fn live_root(&self) -> Option<&ProjectRoot> {
        match self.subject() {
            Subject::Retired(root)
            | Subject::Forget(
                ForgetReason::Abandoned { root, .. }
                | ForgetReason::Unreachable(Unreachable::Root { root, .. }),
            ) => Some(root),
            _ => None,
        }
    }

    /// The history worktrees an `Eject` releases.
    #[must_use]
    pub fn worktrees(&self) -> &[EjectableWorktree] {
        match self {
            Self::Eject { worktrees, .. } => worktrees,
            _ => &[],
        }
    }

    /// The recorded root, for every reason that carries one.
    #[must_use]
    pub fn root(&self) -> Option<&NormalPath> {
        match self.subject() {
            Subject::Drop(DropReason::Empty)
            | Subject::Forget(
                ForgetReason::Unrooted { .. }
                | ForgetReason::Unreachable(Unreachable::Unreadable(_)),
            ) => None,
            Subject::Drop(DropReason::Gone { proof, .. }) => Some(proof.path()),
            Subject::Stranded(stranded) => Some(stranded.proof.path()),
            Subject::Retired(root)
            | Subject::Forget(
                ForgetReason::Abandoned { root, .. }
                | ForgetReason::Unreachable(Unreachable::Root { root, .. }),
            ) => Some(root.path()),
        }
    }

    fn evidence(&self) -> Vec<(&'static str, Fact)> {
        match self.subject() {
            Subject::Drop(DropReason::Empty) => vec![(
                "state",
                Fact::Text("no records, target meta or pending journal".to_owned()),
            )],
            Subject::Drop(DropReason::Gone { links, .. }) => {
                vec![root_deleted(), ("releasable_links", link_list(links))]
            }
            Subject::Stranded(stranded) => {
                let evidence = &stranded.evidence;
                let entries = match evidence.journal {
                    JournalState::Clear => 0,
                    JournalState::Pending { entries } => entries.get(),
                };
                vec![
                    root_deleted(),
                    ("releasable_links", link_list(&stranded.links)),
                    ("present_outside", path_list(&evidence.present_outside)),
                    (
                        "unresolvable_records",
                        Fact::List(evidence.unresolvable.iter().map(record_label).collect()),
                    ),
                    ("pending_journal_entries", Fact::Count(entries)),
                ]
            }
            Subject::Retired(_) => vec![
                ("root", Fact::Text("present".to_owned())),
                ("manifest", Fact::Text("phora.toml absent".to_owned())),
            ],
            Subject::Forget(ForgetReason::Unrooted { meta }) => {
                vec![("meta", Fact::Text(rootless_label(meta)))]
            }
            Subject::Forget(ForgetReason::Abandoned { successor, .. }) => vec![
                ("root", Fact::Text("not proven deleted".to_owned())),
                (
                    "superseded_by",
                    Fact::Text(successor.path().as_path().display().to_string()),
                ),
            ],
            Subject::Forget(ForgetReason::Unreachable(Unreachable::Unreadable(kind))) => {
                vec![("state_dir", Fact::Text(format!("unreadable: {kind}")))]
            }
            Subject::Forget(ForgetReason::Unreachable(Unreachable::Root { cause, .. })) => {
                vec![("root", Fact::Text(unreachable_label(cause)))]
            }
        }
    }
}

fn root_deleted() -> (&'static str, Fact) {
    (
        "root",
        Fact::Text(
            "deleted: absent, recorded on the state dir's mount, state-dir path unchanged, \
             nearest ancestor on that mount, unencrypted, no symlinked component"
                .to_owned(),
        ),
    )
}

fn link_list(links: &[ReleasableLink]) -> Fact {
    Fact::List(
        links
            .iter()
            .map(|link| {
                format!(
                    "{} -> {}",
                    link.path.as_path().display(),
                    link.link_target.as_path().display()
                )
            })
            .collect(),
    )
}

fn record_label(key: &ArtifactKey) -> String {
    format!("{}/{}/{}", key.target, key.source, key.artifact)
}

fn path_list(paths: &[NormalPath]) -> Fact {
    Fact::List(
        paths
            .iter()
            .map(|path| path.as_path().display().to_string())
            .collect(),
    )
}

fn rootless_label(meta: &RootlessMeta) -> String {
    match meta {
        RootlessMeta::Absent => "project.toml absent".to_owned(),
        RootlessMeta::Invalid(MetaDefect::RootHashMismatch) => {
            "project.toml root does not hash to the dir name".to_owned()
        }
        RootlessMeta::Invalid(MetaDefect::Wire(defect)) => match defect {
            WireDefect::Unparsable => "project.toml unparsable".to_owned(),
            WireDefect::UnsupportedVersion(version) => {
                format!("project.toml version {version} unsupported")
            }
            WireDefect::BadHex => "project.toml path has bad hex".to_owned(),
            WireDefect::NotAbsolute => "project.toml path is not absolute".to_owned(),
        },
    }
}

fn unreachable_label(cause: &UnreachableRoot) -> String {
    match cause {
        UnreachableRoot::Unproven(unproven) => match unproven {
            Unproven::RecordedElsewhere => {
                "absent; recorded off the state dir's mount (on_state_mount false)".to_owned()
            }
            Unproven::AnchorMoved { now } => format!(
                "absent; the state dir moved, now at {}",
                now.as_path().display()
            ),
            Unproven::OffMount => {
                "absent; nearest existing ancestor is off the state dir's mount".to_owned()
            }
            Unproven::Encrypted => "absent; nearest existing ancestor is encrypted".to_owned(),
            Unproven::SymlinkedComponent(path) => {
                format!("symlinked component {}", path.as_path().display())
            }
            Unproven::CanonicalDiffers { now } => {
                format!("present, but resolves to {}", now.as_path().display())
            }
            Unproven::Fault(kind) => format!("stat failed: {kind}"),
        },
        UnreachableRoot::ManifestFault(kind) => format!("present; phora.toml stat failed: {kind}"),
        UnreachableRoot::DestinationFault { key, kind } => format!(
            "deleted; the destination stat of record {} failed: {kind}",
            record_label(key)
        ),
    }
}

impl GcRecord {
    #[must_use]
    pub fn action(&self) -> &PlannedAction {
        let (Self::Unrooted { action, .. } | Self::Rooted { action, .. }) = self;
        action
    }

    pub(super) fn into_action(self) -> PlannedAction {
        let (Self::Unrooted { action, .. } | Self::Rooted { action, .. }) = self;
        action
    }

    /// The facts behind the reason; for `unrooted`, the legacy evidence read from the records.
    #[must_use]
    pub fn evidence(&self) -> Vec<(&'static str, Fact)> {
        let mut facts = self.action().evidence();
        if let Self::Unrooted { legacy, .. } = self {
            facts.push(("records", Fact::Count(legacy.records)));
            facts.push((
                "recorded_paths",
                Fact::List(
                    legacy
                        .recorded_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                ),
            ));
            let newest = legacy.newest_record.map_or_else(
                || "unknown".to_owned(),
                |time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339(),
            );
            facts.push(("newest_record", Fact::Text(newest)));
        }
        facts
    }

    /// Every present destination the action leaves behind; for `unrooted`, every recorded path.
    #[must_use]
    pub fn leftovers(&self) -> Vec<&Path> {
        match self {
            Self::Unrooted { legacy, .. } => {
                legacy.recorded_paths.iter().map(PathBuf::as_path).collect()
            }
            Self::Rooted { leftovers, .. } => leftovers.iter().map(NormalPath::as_path).collect(),
        }
    }
}

#[derive(Debug)]
pub struct GcPlan {
    pub(super) records: Vec<GcRecord>,
    /// Registered roots that are absent and not provably deleted.
    pub(super) index_removals: Vec<StateRoot>,
    pub(super) foreign: Vec<NormalPath>,
    pub(super) foreign_trash: Vec<OsString>,
    pub(super) default: StateRoot,
    pub(super) roots: Vec<StateRoot>,
    pub(super) index_read: bool,
    pub(super) dry_run: bool,
    pub(super) faults: Vec<SweepFault>,
}

impl GcPlan {
    #[must_use]
    pub fn records(&self) -> &[GcRecord] {
        &self.records
    }

    #[must_use]
    pub fn index_removals(&self) -> &[StateRoot] {
        &self.index_removals
    }

    /// Entries under `projects/` whose name is not a state dir; never touched.
    #[must_use]
    pub fn foreign(&self) -> &[NormalPath] {
        &self.foreign
    }

    /// Entries under each `trash/` that phora did not put there; never touched.
    #[must_use]
    pub fn foreign_trash(&self) -> &[OsString] {
        &self.foreign_trash
    }

    /// Whether applying the plan needs a confirmation beyond its drops and link releases.
    #[must_use]
    pub fn needs_confirmation(&self) -> bool {
        !self.index_removals.is_empty()
            || self.records.iter().any(|record| {
                matches!(
                    record.action(),
                    PlannedAction::Eject { .. } | PlannedAction::Forget { .. }
                )
            })
    }

    pub fn faults(&self) -> impl Iterator<Item = String> + '_ {
        self.faults.iter().map(SweepFault::describe)
    }
}

#[derive(Debug, Default)]
pub struct GcOutcome {
    pub(super) applied: Vec<(StateDirPath, DropOutcome)>,
    pub(super) links_kept: Vec<(StateDirPath, NormalPath, LinkKept)>,
    pub(super) pending: Vec<StateDirPath>,
    pub(super) faults: Vec<SweepFault>,
}

impl GcOutcome {
    #[must_use]
    pub fn applied(&self) -> &[(StateDirPath, DropOutcome)] {
        &self.applied
    }

    /// Dirs whose drop waits for a confirmation.
    #[must_use]
    pub fn pending(&self) -> &[StateDirPath] {
        &self.pending
    }

    pub fn faults(&self) -> impl Iterator<Item = String> + '_ {
        self.faults.iter().map(SweepFault::describe)
    }

    /// The links the dir's release left in place.
    pub fn links_kept<'a>(
        &'a self,
        dir: &'a StateDirPath,
    ) -> impl Iterator<Item = (&'a NormalPath, &'a LinkKept)> + 'a {
        self.links_kept
            .iter()
            .filter(move |(kept_dir, _, _)| kept_dir == dir)
            .map(|(_, path, kept)| (path, kept))
    }

    /// The dir's applied result followed by each link it kept; `None` when nothing was applied.
    #[must_use]
    pub fn describe(&self, dir: &StateDirPath) -> Option<String> {
        let (_, result) = self.applied.iter().find(|(applied, _)| applied == dir)?;
        let parts: Vec<String> = std::iter::once(result.describe())
            .chain(self.links_kept(dir).map(|(path, kept)| {
                format!("link {} kept ({})", path.as_path().display(), kept.label())
            }))
            .collect();
        Some(parts.join("; "))
    }

    /// Whether an operation errored, a trash entry was left or a link failed to come out.
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.faults.is_empty()
            || self.applied.iter().any(|(_, outcome)| {
                matches!(outcome, DropOutcome::Fault(_) | DropOutcome::Leftover(_))
            })
            || self
                .links_kept
                .iter()
                .any(|(_, _, kept)| matches!(kept, LinkKept::Fault(_)))
    }
}

impl LinkKept {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Confined => "confined".to_owned(),
            Self::Fault(kind) => kind.to_string(),
        }
    }
}

impl DropOutcome {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Deleted => "dropped".to_owned(),
            Self::Leftover(leftover) => format!(
                "dropped; trash entry {} left for the next pass",
                super::trash_path(&leftover.entry).display()
            ),
            Self::Released(links) => {
                let kept: Vec<String> = links
                    .iter()
                    .filter_map(|link| match link {
                        LinkOutcome::Unlinked => None,
                        LinkOutcome::Kept(kept) => Some(kept.label()),
                    })
                    .collect();
                let released = links.len() - kept.len();
                if kept.is_empty() {
                    format!("kept; {released} link(s) released")
                } else {
                    format!(
                        "kept; {released} link(s) released, {} kept ({})",
                        kept.len(),
                        kept.join(", ")
                    )
                }
            }
            Self::Vanished => "vanished".to_owned(),
            Self::Busy => "skipped; its state.lock is held".to_owned(),
            Self::Reclassified(reason) => format!("kept; now {}", reason.label()),
            Self::Fault(error) => format!("kept; error: {error}"),
        }
    }
}
