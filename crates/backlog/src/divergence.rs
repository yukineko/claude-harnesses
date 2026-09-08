//! Store divergence: telling "the queue is empty" apart from "you are reading
//! the wrong file" (backlog 5ba13c3e, completion criterion 2).
//!
//! The queue used to live in one global `~/.backlog/tasks.toml`. It now
//! resolves per repo (`config::locate` → `<root>/.backlog/tasks.toml`).
//! Nothing migrated the old file when that changed, so for months a reader
//! standing in this repo got a perfectly ordinary `no tasks` / `[]` while 490
//! items — including three p0s — sat in the legacy store. The answer was not
//! wrong in the sense of being corrupt: it was the *correct* answer to a
//! question about the wrong file, and it was rendered identically to a
//! genuinely empty queue. That is the collapse CLAUDE.md §3 forbids — "I could
//! not observe your queue" answered as "there is nothing to do".
//!
//! This module makes that state expressible. It answers with three values, not
//! two: [`Divergence::None`] (nothing to say), [`Divergence::Warn`] (say it,
//! but the reader is still being handed real work) and
//! [`Divergence::Undetermined`] (the empty answer cannot be trusted at all).
//!
//! # Why `Undetermined` exits non-zero rather than only warning
//!
//! Both were on the table; the downstream consumers of `backlog list`/`next`
//! decide it. They were enumerated (not assumed) before this was written:
//!
//! | consumer | on non-zero exit | on stderr |
//! |---|---|---|
//! | `crates/autoflow/src/backlog.rs::find_open` | `Determination::Undetermined` → the Stop is **blocked visibly** | echoed, but **only** when the exit is non-zero |
//! | `crates/specguard/src/forge/queue.rs` (`stdout_of` → `stdout_on_success`) | `Determination::Undetermined` → refuses to queue on an unknown queue | ignored |
//! | `crates/overwatch/src/aggregate.rs::shell_soft` | `None` → renders `(none)` | ignored |
//! | `crates/condukt/skills/condukt/SKILL.md` (`… 2>/dev/null \|\| true`) | empty var | **discarded** |
//! | `crates/backlog/src/hooks/session_start.rs` | n/a (in-process) | invisible to the agent |
//!
//! Two facts follow. First, **stderr alone reaches nobody**: three of the five
//! consumers drop it outright, and autoflow only surfaces it on a non-zero
//! exit. A warning-only design would be "we told someone" in the same sense
//! the empty `[]` was "we answered". Second, a non-zero exit **costs nothing in
//! the blocked case specifically**: the block only fires when the resolved
//! store holds no pending work for this project, so there are no items for
//! `shell_soft`'s `(none)` or condukt's empty var to lose — they would have
//! rendered empty anyway. The one consumer whose behaviour actually changes is
//! autoflow, and it changes from "silently concludes there is no work" to
//! "blocks and says why", which is the entire point.
//!
//! The converse is why [`Divergence::Warn`] deliberately stays on exit 0: once
//! the resolved store *does* hold pending work, a non-zero exit would erase
//! that real work from `shell_soft` (→ `(none)`) and from condukt's
//! `|| true`-swallowed capture. Escalating there would manufacture the very
//! false-empty this module exists to prevent. So the rule is: refuse to answer
//! only when the answer would have been empty; otherwise answer, and say what
//! else is out there.
//!
//! # Why a missing legacy store must stay silent
//!
//! A fresh clone on a machine that never had `~/.backlog` has nothing to
//! diverge from. Warning there would fire on every repo forever and train
//! every reader to ignore the diagnostic, which is how a gate becomes
//! decoration. [`LegacyStore::Absent`] is therefore a real observation and
//! resolves to [`Divergence::None`] — as distinct from [`LegacyStore::Unreadable`],
//! which is *not* an observation that the legacy store is empty and must never
//! be collapsed into it.

use std::path::{Path, PathBuf};

/// What could be established about the legacy `~/.backlog/tasks.toml`.
///
/// Three answers, not two. `Absent` and `Unreadable` are the pair that must
/// never merge: "there is no legacy store on this machine" is a fact that
/// justifies silence, while "there is one but I could not read it" justifies
/// nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyStore {
    /// No legacy store file exists (a fresh clone / a machine that only ever
    /// had per-repo stores). Nothing to compare against.
    Absent,
    /// The file exists but could not be read or parsed. Its contents are
    /// unknown — specifically NOT known to be empty.
    Unreadable(String),
    /// The file was read. `matched` counts tasks still queued (pending or
    /// failed, per `Task::is_pending`) whose `project` genuinely scopes to the
    /// checkout being served.
    ///
    /// `unresolved` counts queued tasks flagged `project_unresolved` — their
    /// stored label is a guess, so it proves nothing about whether they belong
    /// here. They are reported but deliberately never escalate: a single
    /// foreign guessed task would otherwise block `backlog list` in every
    /// checkout on the machine.
    Scanned { matched: usize, unresolved: usize },
}

/// What to do about the two stores' disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// Nothing to report; answer exactly as before.
    None,
    /// Report on stderr and carry on (exit 0). The reader is still being
    /// handed real work, so refusing to answer would destroy more information
    /// than it protects — see the module docs.
    Warn(String),
    /// The empty answer cannot be trusted. Callers must NOT print an empty
    /// result; they surface this as a non-zero exit with the reason on stderr.
    Undetermined(String),
}

impl Divergence {
    /// The message, when there is one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Divergence::None => None,
            Divergence::Warn(m) | Divergence::Undetermined(m) => Some(m),
        }
    }
}

/// Decide, from what was observed about both stores.
///
/// `resolved_pending` is the number of tasks still queued for THIS project in
/// the store that was actually resolved — deliberately not "how many rows did
/// the caller's query return", so a `--status done` listing and a bare `list`
/// reach the same verdict about the same pair of files. Divergence is a
/// property of the stores, not of the question asked.
pub fn assess(
    legacy_path: &Path,
    resolved_path: &Path,
    resolved_pending: usize,
    legacy: &LegacyStore,
) -> Divergence {
    let legacy_display = legacy_path.display();
    let resolved_display = resolved_path.display();

    match legacy {
        LegacyStore::Absent => Divergence::None,

        // An unreadable legacy store establishes nothing. With the resolved
        // store also holding no queued work, NOTHING has been established
        // about whether work exists, so an empty answer would be a guess
        // dressed as an observation.
        LegacyStore::Unreadable(why) => {
            let msg = format!(
                "backlog: the legacy store {legacy_display} exists but could not be read \
                 ({why}); whether it still holds queued work for this project is UNKNOWN, \
                 not known to be nothing"
            );
            if resolved_pending == 0 {
                Divergence::Undetermined(format!(
                    "{msg}. The resolved store {resolved_display} holds no queued work either, \
                     so an empty result here would be indistinguishable from a genuinely empty \
                     queue. Fix or remove the legacy store, or pass --all to read the resolved \
                     store unconditionally."
                ))
            } else {
                Divergence::Warn(msg)
            }
        }

        LegacyStore::Scanned {
            matched,
            unresolved,
        } => {
            if *matched == 0 {
                // Tasks whose project label is a guess are surfaced but never
                // escalated (see `LegacyStore::Scanned`).
                if *unresolved > 0 && resolved_pending == 0 {
                    return Divergence::Warn(format!(
                        "backlog: the legacy store {legacy_display} holds {unresolved} queued \
                         task(s) whose project could not be resolved, so they may or may not \
                         belong to this checkout. This queue reports empty; check them before \
                         concluding there is no work."
                    ));
                }
                return Divergence::None;
            }

            if resolved_pending == 0 {
                Divergence::Undetermined(format!(
                    "backlog: this project's store {resolved_display} holds no queued work, but \
                     the legacy store {legacy_display} holds {matched} queued task(s) for the \
                     SAME project. An empty result here would be indistinguishable from a \
                     genuinely empty queue, so it is not reported as one. Migrate the legacy \
                     tasks into the repo store (or move them out of the way) — see backlog \
                     5ba13c3e."
                ))
            } else {
                Divergence::Warn(format!(
                    "backlog: the legacy store {legacy_display} still holds {matched} queued \
                     task(s) for this project that are NOT in {resolved_display}; this listing \
                     is incomplete. See backlog 5ba13c3e."
                ))
            }
        }
    }
}

/// Read the legacy store and count what is still queued for `project`.
///
/// A file that is not there is [`LegacyStore::Absent`]; a file that is there
/// but unreadable/unparseable is [`LegacyStore::Unreadable`]. `store::load`
/// folds the first case into `Ok(vec![])`, which is right for a store being
/// *used* and wrong here — the whole question is whether the old file exists —
/// so existence is checked before loading rather than inferred from emptiness.
pub fn scan_legacy(legacy_path: &Path, project: Option<&str>) -> LegacyStore {
    if !legacy_path.exists() {
        return LegacyStore::Absent;
    }
    let tasks = match crate::store::load(legacy_path) {
        Ok(t) => t,
        Err(e) => return LegacyStore::Unreadable(format!("{e:#}")),
    };
    let filter = project.map(crate::store::canonicalize_project);
    let mut matched = 0usize;
    let mut unresolved = 0usize;
    for t in tasks.iter().filter(|t| t.is_pending()) {
        match filter.as_deref() {
            None => matched += 1,
            Some(f) => {
                if crate::store::project_matches(&t.project, f) {
                    matched += 1;
                } else if t.project_unresolved {
                    unresolved += 1;
                }
            }
        }
    }
    LegacyStore::Scanned {
        matched,
        unresolved,
    }
}

/// Count what is still queued for `project` in the resolved store.
///
/// A resolved store that cannot be read is reported as 0 queued items on
/// purpose: that is the *pessimistic* input to [`assess`] (it can only push the
/// verdict towards `Undetermined`, never away from it). The caller's own
/// `store::list` surfaces the read error separately.
pub fn resolved_pending(resolved_path: &Path, project: Option<&str>) -> usize {
    match scan_legacy(resolved_path, project) {
        LegacyStore::Scanned { matched, .. } => matched,
        LegacyStore::Absent | LegacyStore::Unreadable(_) => 0,
    }
}

/// The legacy store's path: `~/.backlog/tasks.toml`, resolved through
/// `harness_core::config` (which prefers `$HOME`, so a HOME-swapped test never
/// touches the real user's queue).
pub fn legacy_path() -> PathBuf {
    harness_core::config::base_dir("backlog").join("tasks.toml")
}

/// The whole check, for a caller that has already resolved its store path and
/// project scope.
///
/// Short-circuits before any file is read when the resolved store IS the
/// legacy store (a cwd outside any repo, or an operator-pinned `store_dir`
/// naming it): a store cannot diverge from itself, and comparing it to itself
/// would warn about precisely the tasks it just listed.
///
/// The two stores are counted under the scopes their callers actually read
/// them under. They are not the same question, and collapsing them into one
/// `project` (as a single-scope `check` did until 2026-08-20) was safe only
/// while both stores were filtered identically:
///
///   * the LEGACY store is cross-project by construction, so it must be
///     filtered — an unfiltered count there would report another project's
///     backlog as this checkout's missing work;
///   * a REPO store is the scope (see `main`'s `read_project_scope`), so its
///     reader applies NO row filter. Counting it with one would make
///     `resolved_pending` smaller than what was actually listed, and
///     [`assess`] escalates to [`Divergence::Undetermined`] — a non-zero exit
///     with nothing on stdout — precisely when that count is 0. A listing
///     holding real tasks would then be destroyed by the guard meant to
///     protect it, which is the failure this module's docs rule out for
///     [`Divergence::Warn`]. That is not hypothetical here: at measurement
///     point `89feaddb` (2026-08-20) this repo's store held 340 pending-or-
///     failed rows, of which only 70 carried this checkout's own label — so
///     the filtered count for the other checkout is 0 while the listing shows
///     hundreds.
///
/// `resolved_project` is therefore `None` for a repo store and the checkout's
/// own identity for a pinned/legacy one.
pub fn check(
    resolved_path: &Path,
    legacy_project: Option<&str>,
    resolved_project: Option<&str>,
) -> Divergence {
    let legacy = legacy_path();
    if same_file(resolved_path, &legacy) {
        return Divergence::None;
    }
    let scanned = scan_legacy(&legacy, legacy_project);
    if scanned == LegacyStore::Absent {
        // The overwhelmingly common case on a fresh machine: one `stat`, no
        // parse of the (potentially large) resolved store.
        return Divergence::None;
    }
    let resolved = resolved_pending(resolved_path, resolved_project);
    assess(&legacy, resolved_path, resolved, &scanned)
}

/// Same file, comparing canonical paths when both exist and falling back to a
/// literal comparison when they do not.
fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> PathBuf {
        PathBuf::from("/home/u/.backlog/tasks.toml")
    }
    fn resolved() -> PathBuf {
        PathBuf::from("/repo/.backlog/tasks.toml")
    }

    /// The fault this module exists for: the resolved store answers empty while
    /// the legacy store holds queued work for the same project.
    #[test]
    fn empty_resolved_plus_queued_legacy_is_undetermined() {
        let d = assess(
            &legacy(),
            &resolved(),
            0,
            &LegacyStore::Scanned {
                matched: 490,
                unresolved: 0,
            },
        );
        assert!(matches!(d, Divergence::Undetermined(_)), "{d:?}");
        assert!(d.message().unwrap().contains("490"));
    }

    /// A populated resolved store must keep answering (exit 0) — escalating
    /// would erase the real items from consumers that map a non-zero exit to
    /// "none". It still says the listing is incomplete.
    #[test]
    fn populated_resolved_plus_queued_legacy_only_warns() {
        let d = assess(
            &legacy(),
            &resolved(),
            12,
            &LegacyStore::Scanned {
                matched: 3,
                unresolved: 0,
            },
        );
        assert!(matches!(d, Divergence::Warn(_)), "{d:?}");
    }

    /// Anti-vacuity control: no legacy store ⇒ absolutely nothing is said,
    /// whether or not the resolved store is empty. An implementation that
    /// always warns fails here.
    #[test]
    fn an_absent_legacy_store_is_silent_even_when_the_queue_is_empty() {
        assert_eq!(
            assess(&legacy(), &resolved(), 0, &LegacyStore::Absent),
            Divergence::None
        );
        assert_eq!(
            assess(&legacy(), &resolved(), 9, &LegacyStore::Absent),
            Divergence::None
        );
    }

    /// Anti-vacuity control: a legacy store with nothing queued for THIS
    /// project is silent. Otherwise every repo on a machine that has any
    /// legacy store at all would be blocked.
    #[test]
    fn a_legacy_store_with_no_matching_work_is_silent() {
        assert_eq!(
            assess(
                &legacy(),
                &resolved(),
                0,
                &LegacyStore::Scanned {
                    matched: 0,
                    unresolved: 0,
                }
            ),
            Divergence::None
        );
    }

    /// Unreadable is not empty. With the resolved store also empty, nothing is
    /// established, so it resolves to the restrictive side (CLAUDE.md §3).
    #[test]
    fn an_unreadable_legacy_store_is_undetermined_not_empty() {
        let d = assess(
            &legacy(),
            &resolved(),
            0,
            &LegacyStore::Unreadable("bad toml".into()),
        );
        assert!(matches!(d, Divergence::Undetermined(_)), "{d:?}");
        assert!(d.message().unwrap().contains("UNKNOWN"));
    }

    /// …but when the resolved store is answering with real work, an unreadable
    /// legacy store is said out loud rather than escalated (same reason as
    /// `populated_resolved_plus_queued_legacy_only_warns`).
    #[test]
    fn an_unreadable_legacy_store_with_work_present_only_warns() {
        let d = assess(
            &legacy(),
            &resolved(),
            4,
            &LegacyStore::Unreadable("bad toml".into()),
        );
        assert!(matches!(d, Divergence::Warn(_)), "{d:?}");
    }

    /// A guessed project label proves nothing about belonging here, so it is
    /// surfaced but never escalated to a refusal — one foreign guessed task
    /// would otherwise block `backlog list` in every checkout on the machine.
    #[test]
    fn unresolved_project_labels_warn_but_never_block() {
        let d = assess(
            &legacy(),
            &resolved(),
            0,
            &LegacyStore::Scanned {
                matched: 0,
                unresolved: 2,
            },
        );
        assert!(matches!(d, Divergence::Warn(_)), "{d:?}");
    }

    /// The scan distinguishes absent from unreadable — the pair that must
    /// never merge.
    #[test]
    fn scan_reports_absent_and_unreadable_distinctly() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        assert_eq!(scan_legacy(&missing, None), LegacyStore::Absent);

        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "not { toml [[[").unwrap();
        assert!(matches!(
            scan_legacy(&bad, None),
            LegacyStore::Unreadable(_)
        ));
    }

    /// `done` is not queued work; only pending/failed count.
    #[test]
    fn scan_counts_only_queued_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.toml");
        std::fs::write(
            &path,
            "[[task]]\nid = \"a\"\ntitle = \"t\"\nproject = \"proj\"\ntags = []\n\
             status = \"done\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\n\n\
             [[task]]\nid = \"b\"\ntitle = \"t\"\nproject = \"proj\"\ntags = []\n\
             status = \"pending\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\n",
        )
        .unwrap();
        assert_eq!(
            scan_legacy(&path, Some("proj")),
            LegacyStore::Scanned {
                matched: 1,
                unresolved: 0,
            }
        );
    }

    /// A store never diverges from itself.
    #[test]
    fn a_store_compared_against_itself_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.toml");
        std::fs::write(&path, "").unwrap();
        assert!(same_file(&path, &path));
    }
}
