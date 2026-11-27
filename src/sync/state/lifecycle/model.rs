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
        self.roots.insert(root, placement.clone()) != Some(placement)
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
    path: NormalPath,
    link_target: NormalPath,
    anchor: NormalPath,
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
        todo!()
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
    proof: DeletionProof,
    links: Vec<ReleasableLink>,
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
        _present_outside: Vec<NormalPath>,
        _unresolvable: Vec<ArtifactKey>,
        _journal: JournalState,
    ) -> Option<Self> {
        todo!()
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
    dir: StateDirPath,
    lock: StateLock,
}

#[derive(Debug)]
pub enum NotHeld {
    Vanished,
    Busy,
    Fault(ErrorKind),
}

#[derive(Debug)]
pub struct Confirmed {
    held: Held,
    class: StateDirClass,
}

#[derive(Debug)]
pub struct Trashed {
    entry: TrashEntry,
    lock: StateLock,
}

#[derive(Debug)]
pub struct Leftover {
    entry: TrashEntry,
}

/// Unique per rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashSuffix(String);

/// `<root>/trash/<id>.<suffix>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    root: StateRoot,
    id: ProjectId,
    suffix: TrashSuffix,
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
    default: StateRoot,
    index: IndexRead,
    own: Option<StateDirPath>,
}

#[derive(Debug)]
pub struct SweepFault {
    dir: Option<StateDirPath>,
    error: StateError,
}

#[derive(Debug)]
pub struct SweepReport {
    dropped: Vec<StateDirPath>,
    released: Vec<NormalPath>,
    links_kept: Vec<(NormalPath, LinkOutcome)>,
    busy: Vec<StateDirPath>,
    leftovers: Vec<TrashEntry>,
    attention: BTreeMap<Reason, NonZeroUsize>,
    faults: Vec<SweepFault>,
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
