//! The "observed gone" ledger for HUMAN-closed structural findings (user
//! ruling 2026-10-03).
//!
//! A structural finding `specguard:<kind>:<key>:<epoch>` whose latest episode
//! was closed by a HUMAN verdict (confirmed / dismissed / false-positive) is
//! not re-raised while the gap is still there — the human verdict on that gap
//! stands. But once a `specguard audit` run has OBSERVED the gap gone, a later
//! detection of the same gap is a recurrence, and must start a new episode
//! exactly like the automatic `resolved` path does (see
//! [`crate::reconcile::next_episode`]).
//!
//! "Observed gone" is a FACT recorded by the audit run that observed it, never
//! inferred: one JSONL row per human-closed episode id, appended to
//! `specguard_observed_gone.jsonl` next to overwatch's `review_findings.jsonl`
//! (same per-project storage directory). A row is written only when
//!
//! * the spec map is PRESENT and parsed (an absent map loads as an empty map,
//!   which would otherwise read as "every gap gone" — the empty-set trap);
//! * the review-findings store was read strictly (`scan_review_findings`, no
//!   undecodable line) and the disposition ledger was read as `Known`;
//! * the episode is the latest one for its `(kind, key)`, every episode of it
//!   is closed, and the latest was closed by a human verdict — i.e. the
//!   verdict already existed when the absence was observed, so an absence seen
//!   before the human decided cannot later undo that decision;
//! * the entry is in the audit's scope (matches the filter) and the detection
//!   does not report `(kind, key)`, or — on a whole-map audit only — the entry
//!   was removed from the map (the same removed-entry rule `reconcile-findings`
//!   applies).
//!
//! Reading the ledger is tri-state: absent → no observation; unreadable or
//! holding an undecodable line → `Undetermined`. The episode decision treats
//! `Undetermined` as "may have been observed gone" and starts a new episode
//! (a visible duplicate the human can re-dismiss) rather than keeping the
//! finding hidden on an answer nobody could read — the same "visible
//! duplicate over dropped finding" stance `emit_audit_findings` takes for an
//! unreadable findings store. It is never written as an observation.
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const LEDGER: &str = "specguard_observed_gone.jsonl";

/// One recorded observation: the audit run at `observed_ts` saw the gap of
/// the human-closed episode `finding_id` gone.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ObservedGone {
    pub finding_id: String,
    pub kind: String,
    pub key: String,
    pub observed_ts: i64,
}

fn ledger_path(repo_root: &Path) -> anyhow::Result<PathBuf> {
    Ok(overwatch::store::review_findings_path(repo_root)?.with_file_name(LEDGER))
}

/// The episode ids an audit has observed gone. Absent ledger → `Known(empty)`;
/// unresolvable path, unreadable file or an undecodable line → `Undetermined`.
pub(crate) fn read(repo_root: &Path) -> Determination<BTreeSet<String>> {
    let path = match ledger_path(repo_root) {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!("cannot resolve the {LEDGER} path: {e:#}"))
        }
    };
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Determination::known(BTreeSet::new()),
        Determination::Known(Some(txt)) => {
            let mut ids = BTreeSet::new();
            for line in txt.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<ObservedGone>(line) {
                    Ok(o) => {
                        ids.insert(o.finding_id);
                    }
                    Err(e) => {
                        return Determination::undetermined(format!(
                            "{} holds an undecodable line: {e}",
                            path.display()
                        ))
                    }
                }
            }
            Determination::known(ids)
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Append one observation. The failure is returned, not swallowed; the
/// caller reports it (a lost observation keeps the human verdict standing,
/// which is the pre-ruling behaviour, so it must at least be visible).
pub(crate) fn append(repo_root: &Path, row: &ObservedGone) -> anyhow::Result<()> {
    let path = ledger_path(repo_root)?;
    let line = serde_json::to_string(row)?;
    harness_core::append::append_line(&path, &line)
        .map_err(|e| anyhow::anyhow!("appending to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _g = crate::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn absent_ledger_is_known_empty_and_a_row_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        with_home(tmp.path(), || {
            match read(&repo) {
                Determination::Known(s) => assert!(s.is_empty()),
                Determination::Undetermined(w) => panic!("absent must be Known: {}", w.as_str()),
            }
            append(
                &repo,
                &ObservedGone {
                    finding_id: "specguard:untested:a:1".into(),
                    kind: "untested".into(),
                    key: "a".into(),
                    observed_ts: 2,
                },
            )
            .unwrap();
            match read(&repo) {
                Determination::Known(s) => {
                    assert!(s.contains("specguard:untested:a:1"), "{s:?}")
                }
                Determination::Undetermined(w) => panic!("{}", w.as_str()),
            }
        });
    }

    #[test]
    fn an_undecodable_line_is_undetermined_not_fewer_observations() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        with_home(tmp.path(), || {
            let p = ledger_path(&repo).unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "{not json\n").unwrap();
            assert!(matches!(read(&repo), Determination::Undetermined(_)));
        });
    }
}
