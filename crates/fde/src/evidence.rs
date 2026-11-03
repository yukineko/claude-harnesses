//! Evidence ledger: per-requirement red/green/pass records for a target, and
//! the checks over them (`verify`, `report`).
//!
//! SCAFFOLD: every function here answers `Undetermined`.

use std::path::Path;

use harness_core::verdict::{Determination, Verdict};

use crate::target::ResolvedTarget;

/// Which observation an `evidence` record claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Kind {
    /// `CMD` was run and observed failing (the RED half of F->P).
    Red,
    /// `CMD` was run and observed passing after a recorded RED.
    Green,
    /// `CMD` was run and observed passing (no RED claimed).
    Pass,
}

/// Run `cmd` for requirement `req` and record the observation of kind `kind`
/// in `target`'s ledger under `state_dir`. `Clean` = the observation matched
/// `kind` and was recorded; `Violation` = it did not match; anything that
/// could not be run or recorded is `Undetermined`.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn record(
    kind: Kind,
    req: &str,
    cmd: &str,
    target: &ResolvedTarget,
    state_dir: &Path,
) -> Verdict {
    let _ = (kind, req, cmd, target, state_dir);
    Verdict::undetermined("fde: evidence recording is not implemented (scaffold)")
}

/// Check `target`'s ledger. `Clean` = every requirement has complete evidence;
/// `Violation` = some requirement does not; an unreadable ledger or an empty
/// one is `Undetermined`.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn verify(target: &ResolvedTarget, state_dir: &Path) -> Verdict {
    let _ = (target, state_dir);
    Verdict::undetermined("fde: evidence verify is not implemented (scaffold)")
}

/// Render a human-readable report of `target`'s ratification and evidence.
/// An unreadable store is `Undetermined`, never an empty report.
///
/// SCAFFOLD: always `Undetermined`.
#[track_caller]
pub fn report(target: &ResolvedTarget, state_dir: &Path) -> Determination<String> {
    let _ = (target, state_dir);
    Determination::undetermined("fde: report is not implemented (scaffold)")
}
