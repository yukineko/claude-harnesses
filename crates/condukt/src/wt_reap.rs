//! `condukt worktree reap` — reclaim finished `session-*` worktrees — and the
//! session heartbeat that keeps a working session's worktree out of its reach.
//!
//! # The rule (user ruling 2026-09-30, backlog `491f6e94`, option A)
//!
//! `scripts/session-worktree-init.py` writes a `*.driver` registration for the
//! `session-<id>` worktree it creates, into [`SESSION_WORKTREE_BUCKET`] under
//! the backlog driver registry. A git-registered worktree on a `session-*`
//! branch is removed only when ALL of these are positively observed:
//!
//! 1. its registration has aged out under the existing death rule — i.e.
//!    `wt_reconcile` judged its occupancy `Dead`, which already requires a
//!    registration naming it with every heartbeat older than
//!    `wt_reconcile::DRIVER_STALE_TTL_SECS`, frozen signals across two probes
//!    a window apart, no running condukt task claiming it and no session
//!    transcript bound to it;
//! 2. its branch is an ancestor of the default branch (`merge-base
//!    --is-ancestor` exits 0);
//! 3. its tree is clean, untracked files included.
//!
//! Anything undetermined — no registration at all (every worktree that
//! predates the ruling), a first probe, an unreadable registry, a failing git —
//! is KEPT and reported as undetermined. Deletion is the irreversible act, so
//! keep is the restrictive side. [`reap_verdict`] is the single, pure,
//! wildcard-free place that decision is made.
//!
//! The reaper removes the worktree with `git worktree remove` WITHOUT
//! `--force`, so git re-checks cleanliness at the moment of removal and refuses
//! a tree that became dirty after it was judged. It does NOT delete the branch:
//! the ruling authorises removing the worktree, and a branch that is an
//! ancestor of main costs nothing to keep.
//!
//! Nothing runs this automatically. It is an operator command.
//!
//! # Why the heartbeat exists (and why it fires on tool use)
//!
//! A registration is only as good as its heartbeat. Without a refresh, a live
//! session's registration ages out after the TTL and its worktree becomes
//! reapable while the session is still in it. A long autonomous turn can run
//! for hours without a new user prompt, so [`refresh_session_heartbeats`] is
//! wired to PostToolUse (every tool) as well as UserPromptSubmit. It is
//! rate-limited to one write per [`HEARTBEAT_MIN_INTERVAL_SECS`] per record.
//!
//! It touches ONLY records in [`SESSION_WORKTREE_BUCKET`], and within it only
//! the records that belong to this session (same `session_id`) or that name the
//! worktree the session is working in (the git top level of the hook's `cwd`).
//! Records in backlog's own project buckets are `/flow` driver registrations
//! that `autoflow`, `daily` and the main-tree guard read as "a driver is
//! active"; refreshing those from an ordinary tool call would keep a dead
//! driver looking alive, so they are never written here.
//!
//! The heartbeat has no verdict of its own, but its output is an input to the
//! reaper's verdict: a heartbeat that silently fails makes a live worktree look
//! dead. So a failed write is never swallowed — it is returned as an error and
//! the hook exits non-zero with the reason on stderr.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;

use crate::config::Config;
use crate::wt_reconcile::{self, Attribution, Occupancy, Role};

/// The registry bucket (a directory under `~/.backlog/drivers`) that session
/// worktree registrations live in. Deliberately NOT a backlog project slug
/// (those are 16 hex digits): `backlog lock status --project <repo>` reads the
/// slug bucket of the repository, and a session registration there would make
/// `autoflow` and the main-tree guard see every open session as a `/flow`
/// driver. `wt_reconcile::registrations_in` walks every bucket, so the reaper
/// sees records here without knowing the name.
///
/// The name must also never be slug-shaped: backlog's cross-project presence
/// scan (`backlog lock status` with no `--project`, which `daily` reads) only
/// aggregates buckets named like a project slug, and that rule is what keeps a
/// live session registration from reading as an active `/flow` driver.
/// Pinned by `session_bucket_is_not_slug_shaped` below.
pub const SESSION_WORKTREE_BUCKET: &str = "session-worktrees";

/// A record whose `heartbeat_at` is younger than this is not rewritten. Far
/// below `wt_reconcile::DRIVER_STALE_TTL_SECS` (1800s), so rate limiting can
/// never by itself let a working session age out.
pub const HEARTBEAT_MIN_INTERVAL_SECS: i64 = 60;

// ── The decision ───────────────────────────────────────────────────────────

/// What the reaper does with one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reap {
    /// Every term of the rule holds.
    Remove,
    /// Positively known to be kept (the primary tree, a live session, a dirty
    /// tree, an unmerged branch, not a session worktree).
    Keep(String),
    /// Kept because a term could not be established. Reported as such, never
    /// folded into `Keep`: "checked and kept" and "could not check" are
    /// different facts.
    Undetermined(String),
}

/// **The** reap decision. Pure and exhaustively matched with no wildcard arm:
/// a new variant in any input must be classified here explicitly.
///
/// `merged` is only consulted once everything before it permits removal;
/// callers compute it lazily and pass `None` when it was not needed, which is
/// only reachable on the arms that already keep.
pub fn reap_verdict(
    role: Role,
    branch: Option<&str>,
    attribution: &Determination<Attribution>,
    occupancy: &Determination<Occupancy>,
    dirty: &Determination<bool>,
    merged: Option<&Determination<bool>>,
) -> Reap {
    match role {
        Role::Primary => Reap::Keep("the primary working tree is never reaped".into()),
        Role::Unregistered => Reap::Keep(
            "git does not register this directory; `worktree cleanup` owns unregistered \
             directories"
                .into(),
        ),
        Role::Registered => {
            let Some(branch) = branch.filter(|b| b.starts_with("session-")) else {
                return Reap::Keep(format!(
                    "not a session-* worktree (branch {})",
                    branch.unwrap_or("<detached>")
                ));
            };
            match attribution {
                Determination::Undetermined(why) => Reap::Undetermined(why.as_str().to_string()),
                Determination::Known(Attribution::OtherRepo) => {
                    Reap::Keep("belongs to another repository".into())
                }
                Determination::Known(Attribution::ThisRepo) => match occupancy {
                    Determination::Undetermined(why) => {
                        Reap::Undetermined(why.as_str().to_string())
                    }
                    Determination::Known(Occupancy::Live) => {
                        Reap::Keep("a live session or task is working here".into())
                    }
                    Determination::Known(Occupancy::Dead) => match dirty {
                        Determination::Undetermined(why) => {
                            Reap::Undetermined(why.as_str().to_string())
                        }
                        Determination::Known(true) => {
                            Reap::Keep("the tree holds uncommitted or untracked work".into())
                        }
                        Determination::Known(false) => match merged {
                            None => Reap::Undetermined(format!(
                                "whether {branch} is merged was not computed"
                            )),
                            Some(Determination::Undetermined(why)) => {
                                Reap::Undetermined(why.as_str().to_string())
                            }
                            Some(Determination::Known(false)) => Reap::Keep(format!(
                                "branch {branch} is not an ancestor of the default branch"
                            )),
                            Some(Determination::Known(true)) => Reap::Remove,
                        },
                    },
                },
            }
        }
    }
}

/// Is `branch` an ancestor of `default_branch`? Exit 0 is yes, exit 1 is no;
/// every other outcome (a missing ref exits 128, a timeout, a signal) is
/// undetermined — never "no" and never "yes".
pub fn is_merged(repo: &Path, branch: &str, default_branch: &str) -> Determination<bool> {
    let b = format!("refs/heads/{branch}");
    let d = format!("refs/heads/{default_branch}");
    match crate::worktree::git_exit_code(repo, &["merge-base", "--is-ancestor", &b, &d]) {
        Ok(0) => Determination::Known(true),
        Ok(1) => Determination::Known(false),
        Ok(code) => Determination::undetermined(format!(
            "git merge-base --is-ancestor {b} {d} exited {code}, which is neither yes (0) nor \
             no (1)"
        )),
        Err(e) => Determination::undetermined(format!(
            "git merge-base --is-ancestor {b} {d} could not run: {e:#}"
        )),
    }
}

// ── The command ────────────────────────────────────────────────────────────

/// One line of the reap report.
#[derive(Debug, Clone)]
pub struct ReapLine {
    pub path: String,
    pub branch: Option<String>,
    pub outcome: ReapOutcome,
}

#[derive(Debug, Clone)]
pub enum ReapOutcome {
    Removed,
    Kept(String),
    KeptUndetermined(String),
    /// The verdict was Remove but the removal itself failed.
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct ReapReport {
    pub lines: Vec<ReapLine>,
    /// Non-fatal follow-up problems (a registration record that could not be
    /// deleted after its worktree was removed). They still fail the command.
    pub warnings: Vec<String>,
}

impl ReapReport {
    pub fn count(&self, pred: impl Fn(&ReapOutcome) -> bool) -> usize {
        self.lines.iter().filter(|l| pred(&l.outcome)).count()
    }

    /// Exit-code policy. 0 = the pass ran to completion: every worktree was
    /// either removed or kept with a stated reason, undetermined ones included
    /// (they are printed as `kept (undetermined)` and counted in the summary).
    /// Non-zero = the pass itself did not complete as judged: a removal the
    /// verdict authorised failed, or cleanup after a removal failed.
    ///
    /// Undetermined does not make the exit non-zero because keep IS the
    /// restrictive answer for a GC, and a reaper that exits non-zero whenever
    /// any pre-ruling (unregistered) worktree exists would be non-zero forever
    /// and so carry no signal. The exit code therefore never means "everything
    /// is clean"; the summary line is where undetermined is counted.
    pub fn exit_code(&self) -> i32 {
        let failed = self.count(|o| matches!(o, ReapOutcome::Failed(_)));
        if failed > 0 || !self.warnings.is_empty() {
            1
        } else {
            0
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            let branch = l.branch.as_deref().unwrap_or("-");
            match &l.outcome {
                ReapOutcome::Removed => {
                    out.push_str(&format!("removed {} ({branch})\n", l.path));
                }
                ReapOutcome::Kept(why) => {
                    out.push_str(&format!("kept {} ({branch})\n  reason: {why}\n", l.path));
                }
                ReapOutcome::KeptUndetermined(why) => out.push_str(&format!(
                    "kept (undetermined) {} ({branch})\n  reason: {why}\n",
                    l.path
                )),
                ReapOutcome::Failed(why) => out.push_str(&format!(
                    "FAILED to remove {} ({branch})\n  reason: {why}\n",
                    l.path
                )),
            }
        }
        for w in &self.warnings {
            out.push_str(&format!("warning: {w}\n"));
        }
        out.push_str(&format!(
            "reap: removed {}, kept {} ({} undetermined), failed {}\n",
            self.count(|o| matches!(o, ReapOutcome::Removed)),
            self.count(|o| matches!(o, ReapOutcome::Kept(_) | ReapOutcome::KeptUndetermined(_))),
            self.count(|o| matches!(o, ReapOutcome::KeptUndetermined(_))),
            self.count(|o| matches!(o, ReapOutcome::Failed(_))),
        ));
        out
    }
}

/// Judge every worktree of `repo` and remove the ones [`reap_verdict`] allows.
/// The caller holds the repo-primary lock.
pub fn reap(cfg: &Config, cwd: &Path, repo: &Path) -> Result<ReapReport> {
    let report = wt_reconcile::reconcile_judgements(cfg, cwd, repo)?;
    let mut out = ReapReport::default();
    for j in report {
        let needs_merge = matches!(j.role, Role::Registered)
            && j.branch
                .as_deref()
                .is_some_and(|b| b.starts_with("session-"))
            && matches!(j.attribution, Determination::Known(Attribution::ThisRepo))
            && matches!(j.occupancy, Determination::Known(Occupancy::Dead))
            && matches!(j.dirty, Determination::Known(false));
        let merged = if needs_merge {
            j.branch
                .as_deref()
                .map(|b| is_merged(repo, b, &cfg.default_branch))
        } else {
            None
        };
        let verdict = reap_verdict(
            j.role,
            j.branch.as_deref(),
            &j.attribution,
            &j.occupancy,
            &j.dirty,
            merged.as_ref(),
        );
        let path = j.path.to_string_lossy().to_string();
        let outcome = match verdict {
            Reap::Keep(why) => ReapOutcome::Kept(why),
            Reap::Undetermined(why) => ReapOutcome::KeptUndetermined(why),
            Reap::Remove => {
                // Collected BEFORE removal: afterwards the path no longer
                // canonicalizes and could not be matched.
                let records = own_bucket_records_naming(&j.path);
                // No `--force`: git re-checks cleanliness now and refuses a
                // tree that became dirty after it was judged.
                match crate::worktree::git(repo, &["worktree", "remove", &path]) {
                    Ok(_) => {
                        for r in records {
                            if let Err(e) = std::fs::remove_file(&r) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    out.warnings.push(format!(
                                        "removed {path} but could not delete its registration \
                                         {}: {e}",
                                        r.display()
                                    ));
                                }
                            }
                        }
                        ReapOutcome::Removed
                    }
                    Err(e) => ReapOutcome::Failed(format!("git worktree remove: {e:#}")),
                }
            }
        };
        out.lines.push(ReapLine {
            path,
            branch: j.branch,
            outcome,
        });
    }
    Ok(out)
}

// ── Registration records in the session bucket ─────────────────────────────

fn session_bucket() -> Option<PathBuf> {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => Some(
            PathBuf::from(h)
                .join(".backlog")
                .join("drivers")
                .join(SESSION_WORKTREE_BUCKET),
        ),
        _ => None,
    }
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The `*.driver` files in [`SESSION_WORKTREE_BUCKET`] whose `project` names
/// `path`. Only used after the reaper has already judged `path` dead, to delete
/// the now-meaningless records; an unreadable record is simply not collected
/// (it is left behind, which is harmless: it names a path that is gone).
fn own_bucket_records_naming(path: &Path) -> Vec<PathBuf> {
    let Some(bucket) = session_bucket() else {
        return Vec::new();
    };
    let target = canon(path);
    let Ok(rd) = std::fs::read_dir(&bucket) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("driver"))
        .filter(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| v.get("project").and_then(|x| x.as_str()).map(PathBuf::from))
                .is_some_and(|proj| canon(&proj) == target)
        })
        .collect()
}

/// What one heartbeat pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HeartbeatOutcome {
    pub refreshed: usize,
    /// Matched but younger than [`HEARTBEAT_MIN_INTERVAL_SECS`].
    pub fresh: usize,
}

/// Refresh `heartbeat_at` on this session's records in `bucket`.
///
/// A record is this session's when its `session_id` equals `session_id`, or
/// when its `project` canonicalizes to `own_worktree` (the git top level of the
/// hook's cwd). Nothing else is touched, and no record is created.
///
/// A record that cannot be parsed is skipped rather than failing the pass:
/// `wt_reconcile::registrations_in` already resolves an unparseable record
/// anywhere in the registry to undetermined, and undetermined keeps every
/// worktree — so the skip cannot make anything reapable. A record that matched
/// and could not be WRITTEN is an error: that is the case where a live session
/// would silently age out.
pub fn refresh_session_heartbeats(
    bucket: &Path,
    session_id: Option<&str>,
    own_worktree: Option<&Path>,
    now: i64,
) -> Result<HeartbeatOutcome> {
    let mut outcome = HeartbeatOutcome::default();
    let rd = match std::fs::read_dir(bucket) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(outcome),
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "session registration bucket {} unreadable",
                    bucket.display()
                )
            })
        }
    };
    let own = own_worktree.map(canon);
    let mut errors = Vec::new();
    for entry in rd {
        let path = match entry {
            Ok(e) => e.path(),
            Err(e) => {
                errors.push(format!("{}: {e}", bucket.display()));
                continue;
            }
        };
        if path.extension().and_then(|e| e.to_str()) != Some("driver") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let by_session = matches!(
            (session_id, value.get("session_id").and_then(|v| v.as_str())),
            (Some(mine), Some(theirs)) if !mine.is_empty() && mine == theirs
        );
        let by_path = match (&own, value.get("project").and_then(|v| v.as_str())) {
            (Some(own), Some(p)) => canon(Path::new(p)) == *own,
            _ => false,
        };
        if !(by_session || by_path) {
            continue;
        }
        let last = value.get("heartbeat_at").and_then(|v| v.as_i64());
        if let Some(last) = last {
            if now.saturating_sub(last) < HEARTBEAT_MIN_INTERVAL_SECS {
                outcome.fresh += 1;
                continue;
            }
        }
        let Some(obj) = value.as_object_mut() else {
            continue;
        };
        obj.insert("heartbeat_at".into(), Value::from(now));
        obj.insert("pid".into(), Value::from(std::process::id()));
        match write_atomic(&path, &value) {
            Ok(()) => outcome.refreshed += 1,
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    if errors.is_empty() {
        Ok(outcome)
    } else {
        anyhow::bail!(
            "session heartbeat could not refresh {} registration(s): {}",
            errors.len(),
            errors.join("; ")
        )
    }
}

fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    let res = std::fs::rename(&tmp, path).with_context(|| format!("publish {}", path.display()));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// The `condukt session-heartbeat` hook body. Returns the error to surface, if
/// any; the caller turns it into a non-zero exit.
pub fn run_session_heartbeat_hook(stdin: &str) -> Result<HeartbeatOutcome> {
    let payload: Value = serde_json::from_str(stdin)
        .context("session heartbeat: hook payload is not JSON, so the session is unidentifiable")?;
    let session_id = payload.get("session_id").and_then(|v| v.as_str());
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let Some(bucket) = session_bucket() else {
        anyhow::bail!(
            "session heartbeat: $HOME is unset, so this session's worktree registration cannot \
             be refreshed and will age out"
        );
    };
    let own = cwd.as_deref().and_then(|c| {
        crate::worktree::git(c, &["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| PathBuf::from(s.trim()))
    });
    refresh_session_heartbeats(
        &bucket,
        session_id,
        own.as_deref(),
        crate::state::now_secs(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn und<T>(m: &str) -> Determination<T> {
        Determination::undetermined(m)
    }

    #[test]
    fn session_bucket_is_not_slug_shaped() {
        // backlog's cross-project scan skips non-slug buckets; a 16-hex name
        // here would turn every live session into a counted /flow driver.
        let n = SESSION_WORKTREE_BUCKET;
        assert!(
            !(n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit())),
            "{n} is slug-shaped"
        );
    }

    #[test]
    fn remove_needs_every_term() {
        let this = Determination::Known(Attribution::ThisRepo);
        let dead = Determination::Known(Occupancy::Dead);
        let clean = Determination::Known(false);
        let yes = Determination::Known(true);
        assert_eq!(
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                Some(&yes)
            ),
            Reap::Remove
        );
        // Each term flipped or undetermined keeps.
        let cases: Vec<Reap> = vec![
            reap_verdict(
                Role::Primary,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                Some(&yes),
            ),
            reap_verdict(
                Role::Unregistered,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("feature"),
                &this,
                &dead,
                &clean,
                Some(&yes),
            ),
            reap_verdict(Role::Registered, None, &this, &dead, &clean, Some(&yes)),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &und("x"),
                &dead,
                &clean,
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &Determination::Known(Occupancy::Live),
                &clean,
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &und("x"),
                &clean,
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &Determination::Known(true),
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &und("x"),
                Some(&yes),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                Some(&Determination::Known(false)),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                Some(&und("x")),
            ),
            reap_verdict(
                Role::Registered,
                Some("session-a"),
                &this,
                &dead,
                &clean,
                None,
            ),
        ];
        for c in cases {
            assert_ne!(c, Reap::Remove);
        }
    }

    #[test]
    fn undetermined_inputs_are_reported_as_undetermined_not_keep() {
        let this = Determination::Known(Attribution::ThisRepo);
        let v = reap_verdict(
            Role::Registered,
            Some("session-a"),
            &this,
            &und("no registration"),
            &Determination::Known(false),
            None,
        );
        assert!(matches!(v, Reap::Undetermined(ref r) if r.contains("no registration")));
    }
}
