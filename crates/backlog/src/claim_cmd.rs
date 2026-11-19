//! Claim-ledger commands that live outside `main.rs` (split for the
//! file-growth cap): the project identity the ledger is keyed by
//! ([`claim_identity`]) and `backlog unclaim` ([`run_unclaim`]).

use anyhow::Result;
use harness_core::verdict::Required;

use crate::claim_ledger::{self, ReleaseRefusal};
use crate::store;

/// The project IDENTITY the claim ledger is keyed by — the one string every
/// checkout of this project must agree on, or the ledger splits per checkout
/// and stops excluding anything (the defect it exists to close, one level up).
///
/// It is deliberately NOT the store's location (that follows the checkout by
/// design) and NOT the raw `--project` string (two checkouts spell the same
/// project differently). Both branches below end at a string that
/// `lock::project_slug` hashes, and `project_slug` canonicalizes again, so a
/// worktree and its main tree land on the identical slug:
///   - an explicit `--project`: canonicalized WITH the resolved/guessed
///     marker. A GUESSED label is refused — keying the ledger by a guess would
///     silently give this checkout a private ledger, i.e. no exclusion at all,
///     which is exactly the shape of the bug (`unresolved` here means a
///     substituted fallback, not merely "the path is absent" — an absent
///     cross-machine label normalizes identically everywhere and is fine).
///   - the default scope (`--all`, where there is no `--project` to key on, or
///     any other unscoped claim): this checkout's own canonical identity,
///     which normalizes a linked worktree to its main working tree.
///
/// An identity that cannot be determined returns `Err` — a non-zero exit with
/// the reason on stderr — rather than claiming under a guessed key.
pub(crate) fn claim_identity(effective_project: Option<&str>) -> Result<String> {
    let Some(p) = effective_project else {
        let cwd = std::env::current_dir()?;
        return match store::canonical_project_id(&cwd).require() {
            Required::Determined(root) => Ok(root.to_string_lossy().into_owned()),
            Required::Blocked(undet) => {
                let why = undet.as_str();
                Err(anyhow::anyhow!(
                    "cannot determine this checkout's project identity, so a claim could not be \
                     recorded project-wide and would be invisible to other checkouts; refusing \
                     to claim: {why}"
                ))
            }
        };
    };
    let canonical = store::canonicalize_project_with_marker(p);
    if canonical.unresolved {
        return Err(anyhow::anyhow!(
            "the project identity for {p} could not be resolved (a fallback label was \
             substituted), so a claim recorded under it may not be seen by other checkouts of \
             the same project; refusing to claim"
        ));
    }
    Ok(canonical.label)
}

/// `backlog unclaim <ID>`: release the LIVE `next --claim` lease on `id` held
/// by THIS session (`CLAUDE_CODE_SESSION_ID` == the lease's session id).
///
/// Like `next --claim`, this writes only the ledger outside every checkout
/// (never the tracked store), so it is allowed from the primary tree too: the
/// caller does not take the `write_store_path` guard. `identity` is the same
/// project identity `next --claim` recorded the lease under (a linked worktree
/// normalizes to its main working tree). The task stays pending and is
/// immediately claimable again. Every refusal is a non-zero exit with the
/// ledger untouched, and its message names the case (CLAUDE.md §3: an
/// undeterminable holder refuses).
pub(crate) fn run_unclaim(identity: Result<String>, id: &str) -> Result<()> {
    let refuse = |why: String| {
        anyhow::anyhow!("backlog unclaim REFUSED: the lease on {id} was NOT released: {why}")
    };
    let identity = identity.map_err(|e| {
        refuse(format!(
            "the project-wide claim ledger could not be located: {e}"
        ))
    })?;
    let Some(session_id) = std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return Err(refuse(
            "CLAUDE_CODE_SESSION_ID is unset or empty, so the calling session cannot be shown to \
             be the lease holder"
                .to_string(),
        ));
    };
    match claim_ledger::release_lease(&identity, id, &session_id, None) {
        Ok(()) => {
            println!("unclaimed: {id} (lease released; the task is pending)");
            Ok(())
        }
        Err(r) => Err(refuse(match r {
            ReleaseRefusal::NoLiveLease => format!(
                "there is no live lease on {id} (never claimed, or the lease already aged out \
                 after {}s)",
                store::CLAIM_STALE_SECS
            ),
            ReleaseRefusal::OtherSession(holder) => format!(
                "the live lease is held by another session ({holder}); only the holder session \
                 may release it"
            ),
            ReleaseRefusal::NoSessionId => "the live lease has no session id recorded, so its \
                                            holder cannot be determined; it ages out on its own"
                .to_string(),
            ReleaseRefusal::Ledger(why) => {
                format!("the claim ledger could not be updated: {why}")
            }
        })),
    }
}

/// `claim_identity` refusing a GUESSED project label had a kill rate of ZERO:
/// deleting the `canonical.unresolved` refusal left all 225 tests green
/// (measured 2026-08-12 on `2b8abcc6`). That arm is load-bearing — keying the
/// project-wide ledger by a guessed slug gives this checkout a PRIVATE ledger,
/// i.e. no cross-checkout exclusion at all, which is the very defect
/// `709ff549` closes. An arm with no test is an arm that can be deleted by
/// accident, so it gets its own behavioural test here.
#[cfg(test)]
mod claim_identity_tests {
    use super::*;

    /// A path that EXISTS but whose identity cannot be resolved must refuse
    /// the claim, not claim under the substituted fallback label.
    ///
    /// The undetermined-ness is injected physically: the project path lives
    /// inside a mode-000 directory, so `symlink_metadata` returns EACCES —
    /// "could not determine whether it exists", which is neither absent nor
    /// resolved.
    #[test]
    fn an_unresolvable_project_label_refuses_the_claim_rather_than_guessing() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!(
            "backlog-claim-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let outer = base.join("outer");
        let inner = outer.join("proj");
        std::fs::create_dir_all(&inner).expect("fixture dirs");

        // Deny traversal so that stat-ing `inner` fails with EACCES rather
        // than NotFound. Root ignores this, so the assertion below is skipped
        // rather than falsely passing if the probe does not actually degrade.
        std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let degraded = std::fs::symlink_metadata(&inner)
            .err()
            .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);

        let got = claim_identity(Some(&inner.to_string_lossy()));

        // Restore before asserting so a failure still cleans up.
        let _ = std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&base);

        if !degraded {
            eprintln!(
                "skipped: this environment can stat inside a mode-000 dir (running as root?)"
            );
            return;
        }
        let err = got.expect_err(
            "an unresolvable project label must REFUSE the claim: claiming under the substituted \
             fallback label keys the ledger to a private slug, so no other checkout of the \
             project ever sees the claim (backlog 709ff549)",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("refusing to claim"),
            "the refusal must say so verbatim, got: {msg}"
        );
    }
}
