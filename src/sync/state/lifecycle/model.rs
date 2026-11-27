use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::mount::{AnchorDir, DeletionProof, NormalPath, Placement, Unproven};
use crate::source::WorktreeAdminId;
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
    records: usize,
    recorded_paths: Vec<PathBuf>,
    newest_record: Option<SystemTime>,
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
            RecordFate::Fault { kind, .. } => {
                return unreachable_root(root, UnreachableRoot::DestinationFault(*kind));
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
    /// Root deleted; any `RecordFate::Fault` in `Remains`.
    DestinationFault(ErrorKind),
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
    record: ArtifactKey,
    deploy_root: NormalPath,
    admin_id: WorktreeAdminId,
    cache_git_root: NormalPath,
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
#[derive(Debug)]
pub enum LinkOutcome {
    Unlinked,
    Kept(LinkKept),
}

#[derive(Debug)]
pub enum LinkKept {
    Confined,
    Fault(ErrorKind),
}

#[derive(Debug, PartialEq, Eq)]
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
        self.faults.push(SweepFault { dir, error });
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
        self.faults.iter().map(|fault| match &fault.dir {
            Some(dir) => format!("{}: {}", dir.path().as_path().display(), fault.error),
            None => fault.error.to_string(),
        })
    }
}

#[derive(Debug)]
pub struct GcInput {
    sweep: SweepInput,
    eject: bool,
    dry_run: bool,
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

#[derive(Debug)]
pub struct GcPlan {
    records: Vec<GcRecord>,
    index_removals: Vec<StateRoot>,
    foreign: Vec<NormalPath>,
    foreign_trash: Vec<OsString>,
}

#[derive(Debug)]
pub struct GcOutcome {
    applied: Vec<(StateDirPath, DropOutcome)>,
    pending: Vec<StateDirPath>,
    faults: Vec<SweepFault>,
}
