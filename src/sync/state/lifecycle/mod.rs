//! State-dir lifecycle: record each project's root, classify every state dir, drop the dead ones.
#![allow(dead_code, reason = "stubs wired by T004 to T008")]

mod model;
mod wire;

pub use model::*;

use crate::mount::CurrentAnchor;
use crate::sync::state::StateError;

pub fn probe(_dir: &StateDirPath, _anchor: &CurrentAnchor) -> StateDirProbe {
    todo!()
}

pub fn classify(_probe: &StateDirProbe) -> StateDirClass {
    todo!()
}

pub fn sweep(_input: &SweepInput) -> SweepReport {
    todo!()
}

pub fn plan_gc(_input: &GcInput) -> GcPlan {
    todo!()
}

pub fn apply_gc(_plan: GcPlan, _confirmation: Confirmation) -> GcOutcome {
    todo!()
}

/// Takes the dir's `state.lock` with zero timeout, creating nothing outside an existing dir.
pub fn lock(_action: PlannedAction) -> Result<Held, NotHeld> {
    todo!()
}

/// Re-probes under the lock; proceeds only when the reason still equals `planned`.
pub fn confirm(_held: Held, _planned: Reason) -> Result<Confirmed, Reason> {
    todo!()
}

pub fn trash(_confirmed: Confirmed) -> Result<Trashed, StateError> {
    todo!()
}

pub fn delete(_trashed: Trashed) -> Result<(), Leftover> {
    todo!()
}
