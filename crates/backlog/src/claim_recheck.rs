//! `next --claim` with the stored repro re-run before the ticket is handed out
//! (backlog 78cee053). Split out of `main.rs` to keep it under its line cap;
//! the logic is unchanged.

use crate::{claim_ledger, evidence, live_lease_view, store, task};
use anyhow::Result;
use harness_core::verdict::Determination;
use std::collections::HashSet;
use std::path::Path;

/// How many times `next --claim` re-checks when the locked pick is not the
/// candidate whose repro it just re-ran (backlog 78cee053). Exhausting it is a
/// non-zero refusal, never a claim of an un-rechecked ticket.
const RECHECK_ATTEMPTS: usize = 3;

/// `next --claim` with the stored repro RE-RUN before the ticket is handed out
/// (backlog 78cee053, audit B item 6: a RED ticket is often already fixed).
///
/// Each round: peek the top candidate (a pure read, NO lock held), re-run its
/// stored repro at HEAD (`evidence::run_repro`, can be a long `cargo test`, so
/// it runs outside the ledger lock and the tasks-file lock), then:
///   - `not-reproduced` (GREEN): not handed out. A notice naming the id, cmd
///     and rev goes to stderr, the ticket joins `skip`, and the next candidate
///     is considered. Nothing is written to the store.
///   - `undetermined`: REFUSED (user ruling 2026-11-07, "Refuse the claim"):
///     non-zero, the runner's detail in the error, nothing claimed.
///   - `reproduced` (RED): claimed through `claim_ledger::claim_pinned`, which
///     reserves ONLY this candidate. If the locked pick differs, nothing was
///     reserved and the round repeats, at most [`RECHECK_ATTEMPTS`] times.
///
/// `Ok(None)` only when the peek finds no eligible candidate: an observation
/// of an empty queue (after the green skips), the same as before.
pub(crate) fn claim_rechecked(
    tasks_path: &Path,
    tag: Option<&str>,
    project: Option<&str>,
    identity: &str,
    session_id: Option<&str>,
) -> Result<Option<task::Task>> {
    const REFUSED: &str = "backlog next --claim REFUSED to claim (this is NOT \"no pending \
                           tasks\" — nothing was claimed and the queue was not judged empty)";
    let cwd = std::env::current_dir()?;
    let mut skip: HashSet<String> = HashSet::new();
    let mut moved_rounds = 0usize;
    loop {
        let mut excluded: HashSet<String> = live_lease_view(project, "next --claim")?
            .into_keys()
            .collect();
        excluded.extend(skip.iter().cloned());
        let Some(cand) = store::next_excluding(tasks_path, tag, project, &excluded)? else {
            return Ok(None);
        };
        // `pick_next` only returns rows with a reproduced repro, so a missing
        // record here is a broken invariant, refused rather than claimed.
        let Some(stored) = cand.repro.as_ref() else {
            return Err(anyhow::anyhow!(
                "{REFUSED}: candidate {} carries no stored repro to re-run",
                cand.id
            ));
        };
        let rerun = evidence::run_repro(&cwd, &stored.cmd, store::now_unix());
        let rev = rerun.rev.as_deref().unwrap_or("<unresolved>");
        match rerun.outcome.as_str() {
            task::REPRO_REPRODUCED => {}
            task::REPRO_NOT_REPRODUCED => {
                eprintln!(
                    "backlog next --claim: NOT handing out {}: its stored repro no longer \
                     reproduces (cmd: {}, rev: {rev}): {}. It is likely fixed; verify and close \
                     it. Considering the next candidate.",
                    cand.id, rerun.cmd, rerun.detail
                );
                skip.insert(cand.id.clone());
                continue;
            }
            // `undetermined` and any outcome this build does not know.
            other => {
                return Err(anyhow::anyhow!(
                    "{REFUSED}: the re-run of candidate {}'s stored repro could not be \
                     determined (outcome {other}, cmd: {}, rev: {rev}): {}",
                    cand.id,
                    rerun.cmd,
                    rerun.detail
                ));
            }
        }
        let claimed = claim_ledger::claim_pinned(
            tasks_path, tag, project, identity, session_id, None, &skip, &cand.id,
        )?;
        match claimed {
            Determination::Known(claim_ledger::PinnedClaim::Claimed(t)) => return Ok(Some(*t)),
            Determination::Known(claim_ledger::PinnedClaim::Moved(picked)) => {
                moved_rounds += 1;
                if moved_rounds >= RECHECK_ATTEMPTS {
                    return Err(anyhow::anyhow!(
                        "{REFUSED}: the locked pick kept differing from the re-checked candidate \
                         ({moved_rounds} attempts; last re-checked {}, locked pick {}); a ticket \
                         whose repro was not re-run is never handed out",
                        cand.id,
                        picked.as_deref().unwrap_or("<none eligible>")
                    ));
                }
            }
            // REFUSED, not empty. Non-zero exit, reason on stderr, nothing on
            // stdout — the identical shape `guard_store_divergence` uses, and
            // the shape `autoflow::backlog::find_open` reads as
            // `Determination::Undetermined` rather than "no work".
            Determination::Undetermined(why) => {
                return Err(anyhow::anyhow!("{REFUSED}: {why}"));
            }
        }
    }
}
