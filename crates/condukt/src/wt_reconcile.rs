//! `condukt worktree reconcile` — cross-check the three sources that together
//! say whether a worktree is alive, and gate every deletion behind that answer.
//!
//! # The problem this exists for
//!
//! A crashed session leaves a worktree behind. Nobody can then tell whether it
//! is a dead husk or the working tree of a session that is still running, and
//! *that not-knowing is the defect*. The user's instruction was literally
//! "生きているかわからないという状態そのものをなくすか減らしてほしい。
//! worktree と condukt を照合させるなどする。もちろん main でもする" — remove
//! or reduce the state of not knowing whether it is alive, by cross-checking
//! the worktrees against condukt, main included.
//!
//! So this module reconciles three sources:
//!
//! 1. **On disk**: `git worktree list --porcelain` (the primary working tree
//!    included — "もちろん main でも") plus the directories physically present
//!    under `worktree_base` that git does not register at all.
//! 2. **condukt's run state**: every run under the state root, across ALL
//!    per-checkout namespaces (see "Cross-checkout scan" below), asked which
//!    RUNNING task points at which worktree, and whether that task is making
//!    progress (the shared multi-sample engine, `harness_core::progress`).
//! 3. **The primary tree**: identified as such and permanently excluded from
//!    deletion.
//!
//! # The direction is INVERTED relative to a gate
//!
//! For a gate, "cannot determine" resolves to blocking the user. Here the
//! irreversible operation is **deletion**, so the restrictive side is **do not
//! delete**. Every undetermined input — an unattributable directory, an
//! unreadable run-state file, a `git status` that will not run — resolves to
//! keep. [`is_removable`] is the single place that decision is made, it is a
//! pure function, and it is matched exhaustively with **no wildcard arm**, so
//! adding a variant to any of its inputs is a compile error rather than a
//! silently permissive default.
//!
//! # Frozen is not dead
//!
//! A task whose progress fingerprint has not advanced reads `Stalled`. That is
//! **not** mapped to "dead": a worker that is thinking, or editing files
//! without committing, is stalled by this measure and is very much alive.
//! `Stalled` therefore resolves to `Undetermined` occupancy, i.e. keep. Only
//! the complete absence of any RUNNING task claiming the path — established
//! from a run-state scan that was *fully readable* — yields `Dead`.
//!
//! # Cross-checkout scan
//!
//! condukt's run state is namespaced per checkout (`<state_dir>/<project-key>/`),
//! so a run created from checkout A is invisible from checkout B (backlog
//! `4708069b`). A worktree can be claimed by a run from any checkout, so this
//! scans **every namespace under the state root**, not just this checkout's.
//! If the state root is absent or unreadable, or any run file inside it cannot
//! be read or parsed, the scan is INCOMPLETE: nothing may then be concluded
//! dead, and every entry that is not positively live becomes `Undetermined`.
//! An absent state root is deliberately treated the same way — a mis-resolved
//! `$HOME` would otherwise make "no runs exist" look like "nothing is alive",
//! which is the exact collapse this module refuses.
//!
//! # What this does NOT consume
//!
//! Neither `claim::claim_progress` (a RUN-scoped, claim-registry-shaped verdict
//! that holds reap authority over claims, not over directories; it sampled a
//! REPO-WIDE git HEAD until backlog `aa6d6e43` was fixed) nor
//! `state::stuck_task_ids`
//! (an `updated_at` age filter that, since backlog `356bd51d`, additionally
//! requires a confirmed `Known(Stalled)` TASK-scoped progress verdict before it
//! authorises `state abandon --all-stuck` — a different gate over a different
//! subject, still not consumed here). The only thing
//! borrowed from `claim` is `progress_store_dir`, a path helper, so that this
//! module's samples land in the same store as the rest of the engine.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::progress;
use harness_core::verdict::Determination;

use crate::config::Config;
use crate::state::{self, RunState, Status};
use crate::worktree;

// ── The three-valued domain ────────────────────────────────────────────────

/// Where a directory sits in git's own view of the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The primary working tree ("main"). Never removable, in any state.
    Primary,
    /// A linked worktree git currently registers.
    Registered,
    /// A directory under `worktree_base` git does not register.
    Unregistered,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::Registered => "registered",
            Role::Unregistered => "unregistered",
        }
    }
}

/// Which repository a directory belongs to. The third answer — "I could not
/// tell" — is carried by `Determination::Undetermined`, never by a variant
/// here, so it cannot be confused with a decided attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// Resolves to the `.git` common dir of the repo being reconciled.
    ThisRepo,
    /// Resolves to some other repository sharing this `worktree_base`.
    OtherRepo,
}

impl Attribution {
    fn as_str(self) -> &'static str {
        match self {
            Attribution::ThisRepo => "this-repo",
            Attribution::OtherRepo => "other-repo",
        }
    }
}

/// Whether anything is working in a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    /// A RUNNING task claims this path and its own progress signals advanced.
    Live,
    /// A fully readable scan of every run namespace found no RUNNING task
    /// claiming this path. "Fully readable" is load-bearing: one unreadable
    /// run file downgrades this to `Undetermined`.
    Dead,
}

impl Occupancy {
    fn as_str(self) -> &'static str {
        match self {
            Occupancy::Live => "live",
            Occupancy::Dead => "dead",
        }
    }
}

/// **The** deletion decision. Pure, total, and exhaustively matched.
///
/// Read it as: a directory may be deleted only when every one of these is
/// positively known and permissive — it is not the primary tree, it provably
/// belongs to this repo, nothing live occupies it, and either it holds no
/// uncommitted work or that work has already been captured into a reachable
/// ref. Anything else — including every `Undetermined` — is "keep".
///
/// There is deliberately no `_ =>` arm anywhere in this function. Adding a
/// variant to [`Role`], [`Attribution`], [`Occupancy`] or `Determination` must
/// break this match at compile time and force whoever added it to say what the
/// new answer means for an irreversible delete.
///
/// `preserved` is a plain `bool` because BOTH of its failure modes are already
/// on the safe side: "not preserved" and "could not verify the preservation"
/// are both `false`, and `false` only ever makes the answer more restrictive
/// (it is consulted solely in the dirty arm, where it is the *permission* to
/// delete). It means, specifically: a `refs/preserved/...` ref exists whose
/// tree is byte-identical to the directory's current content tree — not merely
/// that some ref of that name exists.
pub fn is_removable(
    role: Role,
    attribution: &Determination<Attribution>,
    occupancy: &Determination<Occupancy>,
    dirty: &Determination<bool>,
    preserved: bool,
) -> bool {
    match role {
        // "もちろん main でもする" — main is reconciled and reported like any
        // other tree, but it is never a deletion candidate.
        Role::Primary => false,
        Role::Registered | Role::Unregistered => match attribution {
            Determination::Undetermined(_) => false,
            Determination::Known(Attribution::OtherRepo) => false,
            Determination::Known(Attribution::ThisRepo) => match occupancy {
                Determination::Undetermined(_) => false,
                Determination::Known(Occupancy::Live) => false,
                Determination::Known(Occupancy::Dead) => match dirty {
                    Determination::Undetermined(_) => false,
                    Determination::Known(true) => preserved,
                    Determination::Known(false) => true,
                },
            },
        },
    }
}

// ── Report shapes (the `--json` surface) ───────────────────────────────────

/// A three-valued answer rendered for JSON: `value` is the decided answer (a
/// bool for dirtiness, a string otherwise) or the string `"undetermined"`,
/// with `reason` always present when it is undetermined. There is no shape in
/// which "could not tell" is indistinguishable from a decided answer.
#[derive(Debug, Clone, Serialize)]
pub struct Judged {
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Judged {
    fn known(v: serde_json::Value) -> Self {
        Judged {
            value: v,
            reason: None,
        }
    }
    fn undetermined(reason: &str) -> Self {
        Judged {
            value: serde_json::Value::String("undetermined".into()),
            reason: Some(reason.to_string()),
        }
    }
    fn from_attribution(d: &Determination<Attribution>) -> Self {
        match d {
            Determination::Known(a) => Judged::known(a.as_str().into()),
            Determination::Undetermined(r) => Judged::undetermined(r.as_str()),
        }
    }
    fn from_occupancy(d: &Determination<Occupancy>) -> Self {
        match d {
            Determination::Known(o) => Judged::known(o.as_str().into()),
            Determination::Undetermined(r) => Judged::undetermined(r.as_str()),
        }
    }
    fn from_dirty(d: &Determination<bool>) -> Self {
        match d {
            Determination::Known(b) => Judged::known((*b).into()),
            Determination::Undetermined(r) => Judged::undetermined(r.as_str()),
        }
    }
}

/// The preservation status of one directory's uncommitted content.
#[derive(Debug, Clone, Serialize)]
pub struct Preserved {
    /// True ONLY when a ref exists whose tree equals the directory's current
    /// content tree. A ref that is merely present, or that predates work added
    /// since, is not a preservation.
    pub preserved: bool,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Preserved {
    fn no(reason: impl Into<String>) -> Self {
        Preserved {
            preserved: false,
            git_ref: None,
            commit: None,
            reason: Some(reason.into()),
        }
    }
}

/// A RUNNING task that claims a worktree, with its own progress verdict.
#[derive(Debug, Clone, Serialize)]
pub struct Claim {
    pub run_id: String,
    pub task_id: String,
    /// `progressing` | `stalled` | `undetermined` — the multi-sample verdict
    /// for THIS task's own signals.
    pub progress: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_reason: Option<String>,
    pub state_namespace: String,
    /// The path exactly as the task recorded it (before canonicalization), so
    /// a task pointing somewhere unexpected is visible rather than normalized
    /// away.
    pub claimed_worktree: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub attribution: Judged,
    pub occupancy: Judged,
    pub dirty: Judged,
    pub preserved: Preserved,
    /// The output of [`is_removable`] for this entry — the ONLY field any
    /// caller may act on when deleting.
    pub removable: bool,
    pub claims: Vec<Claim>,
}

/// Whether condukt's run state could be read in full. `readable == false`
/// means no directory may be concluded dead.
#[derive(Debug, Clone, Serialize)]
pub struct StateScan {
    pub readable: bool,
    pub namespaces: usize,
    pub runs: usize,
    pub running_tasks: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub repo: String,
    pub worktree_base: String,
    pub state_root: String,
    pub now: i64,
    pub window_secs: i64,
    pub state_scan: StateScan,
    pub entries: Vec<Entry>,
    pub removable_count: usize,
    /// The cross-check in the OTHER direction: RUNNING tasks whose recorded
    /// worktree is not present on disk at all. condukt believes work is
    /// happening in a directory that does not exist — which is a disagreement
    /// between the two sources, and therefore something the operator should
    /// see rather than something to silently drop.
    pub dangling_claims: Vec<Claim>,
}

// ── Source 2: condukt run state, across every checkout namespace ───────────

#[derive(Debug, Clone)]
struct TaskClaim {
    run_id: String,
    task_id: String,
    namespace: String,
    worktree: PathBuf,
    updated_at: Option<i64>,
}

/// The result of scanning every run namespace under the state root.
struct RunIndex {
    /// canonical worktree path -> the RUNNING tasks claiming it.
    by_worktree: BTreeMap<PathBuf, Vec<TaskClaim>>,
    /// False when ANY namespace or run file could not be read or parsed, or
    /// the state root itself is absent/unreadable. While false, "no task
    /// claims this path" proves nothing.
    complete: bool,
    reason: Option<String>,
    namespaces: usize,
    runs: usize,
    running_tasks: usize,
}

impl RunIndex {
    fn incomplete(&mut self, reason: String) {
        self.complete = false;
        if self.reason.is_none() {
            self.reason = Some(reason);
        }
    }
}

/// Could this JSON document be a run state? Deliberately a NEGATIVE test: it
/// answers "no" only for a document carrying neither `run_id` nor `tasks`,
/// i.e. one that provably is not run state. Anything carrying either key is
/// treated as a run that MUST deserialize, so a genuinely corrupt run file can
/// never be waved through as "some other sidecar" — the skip path is the
/// permissive one, so it is made as narrow as possible.
fn looks_like_run_state(value: &serde_json::Value) -> bool {
    value.get("run_id").is_some() || value.get("tasks").is_some()
}

fn scan_state_root(state_root: &Path) -> RunIndex {
    let mut idx = RunIndex {
        by_worktree: BTreeMap::new(),
        complete: true,
        reason: None,
        namespaces: 0,
        runs: 0,
        running_tasks: 0,
    };

    let namespaces = match std::fs::read_dir(state_root) {
        Ok(rd) => rd,
        Err(e) => {
            // Absent is treated exactly like unreadable: a mis-resolved $HOME
            // must not read as "there are no runs, so nothing is alive".
            idx.incomplete(format!(
                "condukt state root {} could not be listed: {e}",
                state_root.display()
            ));
            return idx;
        }
    };

    for ns_entry in namespaces {
        let ns_entry = match ns_entry {
            Ok(e) => e,
            Err(e) => {
                idx.incomplete(format!(
                    "unreadable entry in state root {}: {e}",
                    state_root.display()
                ));
                continue;
            }
        };
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        idx.namespaces += 1;
        let ns_name = ns_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let files = match std::fs::read_dir(&ns_path) {
            Ok(rd) => rd,
            Err(e) => {
                idx.incomplete(format!(
                    "state namespace {} could not be listed: {e}",
                    ns_path.display()
                ));
                continue;
            }
        };
        for file in files {
            let file = match file {
                Ok(f) => f,
                Err(e) => {
                    idx.incomplete(format!(
                        "unreadable entry in state namespace {}: {e}",
                        ns_path.display()
                    ));
                    continue;
                }
            };
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // The same sidecar list `all_runs` uses — shared, not re-declared,
            // so a new sidecar kind cannot start reading as a corrupt run here
            // while `all_runs` skips it.
            if state::is_run_state_sidecar(fname) {
                continue;
            }
            let txt = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    idx.incomplete(format!("run state {} unreadable: {e}", path.display()));
                    continue;
                }
            };
            // Two-step on purpose. Step 1 asks "is this JSON at all?" — a file
            // that is not is CORRUPTION, and corruption in the state root means
            // a run we cannot read might be claiming a worktree, so the scan is
            // incomplete. Step 2 asks "is this JSON a run state?" — a valid
            // JSON document that is not one (a sidecar this function has not
            // been taught to name) is simply NOT a run and is skipped. Treating
            // the second case as corruption would make every real state
            // directory permanently incomplete, and a gate that is always
            // undetermined teaches operators to ignore it.
            let value: serde_json::Value = match serde_json::from_str(&txt) {
                Ok(v) => v,
                Err(e) => {
                    idx.incomplete(format!("run state {} unparseable: {e}", path.display()));
                    continue;
                }
            };
            if !looks_like_run_state(&value) {
                continue;
            }
            let rs: RunState = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    idx.incomplete(format!(
                        "run state {} has the shape of a run but does not \
                         deserialize: {e}",
                        path.display()
                    ));
                    continue;
                }
            };
            idx.runs += 1;
            for t in rs.tasks.iter().filter(|t| t.status == Status::Running) {
                let Some(wt) = t.worktree.as_deref() else {
                    continue;
                };
                idx.running_tasks += 1;
                let raw = PathBuf::from(wt);
                let canon = raw.canonicalize().unwrap_or(raw.clone());
                idx.by_worktree.entry(canon).or_default().push(TaskClaim {
                    run_id: rs.run_id.clone(),
                    task_id: t.id.clone(),
                    namespace: ns_name.clone(),
                    worktree: raw,
                    updated_at: t.updated_at,
                });
            }
        }
    }
    idx
}

// ── Per-directory signals ──────────────────────────────────────────────────

/// Is `path` the ROOT of a usable git worktree? Returns the canonical toplevel
/// only when git runs there AND reports `path` itself as the toplevel — so a
/// bare directory sitting inside some other repository can never be judged by
/// that repository's status output.
fn git_root_here(path: &Path) -> Option<PathBuf> {
    let top = worktree::git(path, &["rev-parse", "--show-toplevel"]).ok()?;
    let top = PathBuf::from(top.trim()).canonicalize().ok()?;
    let here = path.canonicalize().ok()?;
    if top == here {
        Some(top)
    } else {
        None
    }
}

/// Uncommitted work — tracked modifications AND untracked files — as a
/// three-valued answer. `git status --porcelain` lists both; an unreadable
/// status (a stale worktree whose admin dir is gone, a directory that is not a
/// worktree root, a git failure) is `Undetermined`, never "clean".
pub fn dirtiness(path: &Path) -> Determination<bool> {
    if git_root_here(path).is_none() {
        return Determination::undetermined(format!(
            "{} is not the root of a usable git worktree — its uncommitted \
             content cannot be read, so it cannot be judged clean",
            path.display()
        ));
    }
    match worktree::git(path, &["status", "--porcelain"]) {
        Ok(s) => Determination::Known(!s.trim().is_empty()),
        Err(e) => Determination::undetermined(format!(
            "git status --porcelain failed in {}: {e}",
            path.display()
        )),
    }
}

/// A working-tree activity signal: the `git status --porcelain` text plus the
/// newest mtime among the paths it names.
///
/// `probe_run`'s docstring names the residual this closes: a worker editing
/// files **without committing** moves neither the worktree HEAD nor
/// `updated_at`, so it reads `stalled` while very much alive. Its edits do move
/// this signal (a new/removed entry changes the text; an in-place edit changes
/// that file's mtime). It is folded into the same fingerprint, so it can only
/// make a worktree look MORE alive — the keep side.
fn worktree_activity_signal(path: &Path) -> Determination<Vec<u8>> {
    let status = match worktree::git(path, &["status", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => {
            return Determination::undetermined(format!(
                "worktree activity unreadable in {}: {e}",
                path.display()
            ))
        }
    };
    let mut newest: u128 = 0;
    // Bounded: a pathological worktree with thousands of changed files must not
    // turn one probe into a stat storm. The status text itself still covers
    // entries beyond the bound.
    for line in status.lines().take(200) {
        if line.len() < 4 {
            continue;
        }
        let rest = &line[3..];
        let file = rest.rsplit(" -> ").next().unwrap_or(rest).trim_matches('"');
        if let Ok(meta) = std::fs::metadata(path.join(file)) {
            let m = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            newest = newest.max(m);
        }
    }
    Determination::Known(format!("{newest}|{status}").into_bytes())
}

/// The progress verdict for one claiming task, from the SAME multi-sample
/// engine every other reaper in this workspace uses. Task-scoped throughout:
/// the worktree's own HEAD, that task's own `updated_at`, and that worktree's
/// own activity — never a repo-wide signal (backlog `aa6d6e43`).
fn claim_progress_verdict(
    store: &Path,
    claim: &TaskClaim,
    path: &Path,
    now: i64,
    window: i64,
) -> Determination<progress::Liveness> {
    let head = progress::git_head_signal(path);
    let updated = match claim.updated_at {
        Some(ts) => Determination::Known(ts.to_string().into_bytes()),
        None => Determination::undetermined(
            "task has no updated_at (legacy) — durable progress unreadable",
        ),
    };
    let activity = worktree_activity_signal(path);
    let current = progress::fingerprint_from_signals(vec![
        ("task-worktree-head", head),
        ("task-updated-at", updated),
        ("worktree-activity", activity),
    ]);
    let key = format!(
        "wt-reconcile:{}:{}:{}",
        claim.run_id,
        claim.task_id,
        path.display()
    );
    progress::sample(store, &key, current, now, window)
}

fn liveness_word(v: &Determination<progress::Liveness>) -> (String, Option<String>) {
    match v {
        Determination::Known(progress::Liveness::Progressing) => ("progressing".into(), None),
        Determination::Known(progress::Liveness::Stalled) => ("stalled".into(), None),
        Determination::Undetermined(r) => ("undetermined".into(), Some(r.as_str().to_string())),
    }
}

/// Which repository this directory belongs to, three-valued.
///
/// `worktree::owning_repo_git_dir` returning `None` means "cannot attribute",
/// which is where the old `orphans()` fail-open lived: it pushed exactly this
/// case onto the deletion list. Here it becomes `Undetermined`, and
/// [`is_removable`] resolves that to keep.
pub fn attribution(path: &Path, repo_git_dir: Option<&PathBuf>) -> Determination<Attribution> {
    let Some(repo_git_dir) = repo_git_dir else {
        return Determination::undetermined(
            "the repository's own .git common dir could not be resolved — no \
             directory can be attributed to it",
        );
    };
    match worktree::owning_repo_git_dir(path) {
        Some(owner) if &owner == repo_git_dir => Determination::Known(Attribution::ThisRepo),
        Some(_) => Determination::Known(Attribution::OtherRepo),
        None => Determination::undetermined(format!(
            "{} has no resolvable .git — it cannot be attributed to any \
             repository, which is NOT the same as being condukt debris",
            path.display()
        )),
    }
}

/// Unregistered directories under `worktree_base` that are **not** attributed
/// to a different repository — i.e. this repo's own stale worktrees plus the
/// ones nothing can attribute at all.
///
/// This is the REPORTING list (the SessionStart notice, the completion gate's
/// "leaked worktree" reason), and its inclusion rule is chosen for that use:
/// there, mentioning a directory is the restrictive act, so an unattributable
/// directory belongs in it. It is emphatically **not** a deletion list — the
/// only thing that authorizes a delete is [`is_removable`], which resolves the
/// very same unattributable directory to keep. The two lists differ precisely
/// because the restrictive direction differs, and conflating them is what the
/// old `orphans()` did.
pub fn reportable_unregistered_dirs(repo: &Path, worktree_base: &Path) -> Result<Vec<PathBuf>> {
    let repo_git_dir = worktree::repo_git_common_dir(repo);
    Ok(worktree::unregistered_dirs(repo, worktree_base)?
        .into_iter()
        .filter(|p| {
            !matches!(
                attribution(p, repo_git_dir.as_ref()),
                Determination::Known(Attribution::OtherRepo)
            )
        })
        .collect())
}

// ── Preservation (the temp-index recipe) ───────────────────────────────────

/// The ref a directory's content is preserved under. The basename is kept for
/// legibility and suffixed with a hash of the full canonical path so two
/// same-named worktrees under different bases cannot collide.
pub fn preserved_ref_name(path: &Path) -> String {
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "worktree".into());
    let sani: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let full = path.to_string_lossy();
    format!(
        "refs/preserved/{}-{:08x}",
        sani,
        harness_core::projkey::fnv1a32(&full)
    )
}

/// A throwaway index path OUTSIDE the worktree. Writing the index inside the
/// worktree would itself dirty the thing we are trying to preserve.
fn temp_index_path(path: &Path) -> PathBuf {
    let unique = format!(
        "condukt-preserve-{}-{:08x}.index",
        std::process::id(),
        harness_core::projkey::fnv1a32(&path.to_string_lossy())
    );
    std::env::temp_dir().join(unique)
}

/// The tree object for a worktree's FULL current content (tracked
/// modifications and untracked files alike), built through a throwaway
/// `GIT_INDEX_FILE` so the worktree's real index and working tree are left
/// exactly as they were.
///
/// This is the recipe used by hand on 2026-08-11 to rescue 2404 lines of
/// untracked work from `session-b9a78b46`: seed a temp index from HEAD,
/// `add -A`, `write-tree`.
///
/// **Limit, stated rather than glossed over**: `add -A` honours `.gitignore`,
/// so git-ignored files (build output, `.scratch/`, local `.env`) are NOT
/// captured. A preservation therefore proves that no *trackable* work is lost,
/// not that the directory is byte-reproducible.
fn content_tree(path: &Path) -> Result<String> {
    let index = temp_index_path(path);
    let _ = std::fs::remove_file(&index);
    let result = (|| -> Result<String> {
        worktree::git_env(path, &["read-tree", "HEAD"], &[("GIT_INDEX_FILE", &index)])
            .context("seeding the temp index from HEAD")?;
        worktree::git_env(path, &["add", "-A"], &[("GIT_INDEX_FILE", &index)])
            .context("staging the full working tree into the temp index")?;
        let tree = worktree::git_env(path, &["write-tree"], &[("GIT_INDEX_FILE", &index)])
            .context("writing the content tree")?;
        Ok(tree.trim().to_string())
    })();
    let _ = std::fs::remove_file(&index);
    result
}

/// Is this directory's current content already captured under its preserved
/// ref? Compares TREES, not ref existence: work added after a preservation
/// re-arms the gate.
fn preservation_status(path: &Path, repo: &Path) -> Preserved {
    let name = preserved_ref_name(path);
    let existing = worktree::git(repo, &["rev-parse", "--verify", "--quiet", &name]);
    let Ok(commit) = existing else {
        return Preserved::no(format!("no preserved ref {name}"));
    };
    let commit = commit.trim().to_string();
    if commit.is_empty() {
        return Preserved::no(format!("no preserved ref {name}"));
    }
    let ref_tree = match worktree::git(repo, &["rev-parse", &format!("{name}^{{tree}}")]) {
        Ok(t) => t.trim().to_string(),
        Err(e) => return Preserved::no(format!("preserved ref {name} does not resolve: {e}")),
    };
    match content_tree(path) {
        Ok(current) if current == ref_tree => Preserved {
            preserved: true,
            git_ref: Some(name),
            commit: Some(commit),
            reason: None,
        },
        Ok(_) => Preserved {
            preserved: false,
            git_ref: Some(name.clone()),
            commit: Some(commit),
            reason: Some(format!(
                "{name} exists but its tree no longer matches this worktree's \
                 content — work has been added since it was preserved"
            )),
        },
        Err(e) => Preserved::no(format!(
            "cannot compare {name} against the current content: {e}"
        )),
    }
}

/// Commit a worktree's full content to `refs/preserved/<name>` and verify the
/// ref reads back with the tree we wrote. Leaves the working tree untouched.
pub fn preserve(path: &Path, repo: &Path, now: i64) -> Preserved {
    let name = preserved_ref_name(path);
    let tree = match content_tree(path) {
        Ok(t) => t,
        Err(e) => {
            return Preserved::no(format!("cannot build a content tree for preservation: {e}"))
        }
    };
    let msg = format!(
        "condukt preserve: {} @ {now}\n\nFull working-tree content (tracked + untracked, \
         git-ignored files excluded) captured by `condukt worktree reconcile --preserve` \
         so the directory can be deleted without losing work.",
        path.display()
    );
    // The parent is the worktree's own HEAD when it has one, so the preserved
    // commit sits on top of the history it came from; a worktree with no HEAD
    // (unborn branch) still preserves as a parentless commit rather than not
    // at all.
    let head = worktree::git(path, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let commit = match &head {
        Some(parent) => worktree::git(path, &["commit-tree", &tree, "-p", parent, "-m", &msg]),
        None => worktree::git(path, &["commit-tree", &tree, "-m", &msg]),
    };
    let commit = match commit {
        Ok(c) => c.trim().to_string(),
        Err(e) => return Preserved::no(format!("commit-tree failed: {e}")),
    };
    if let Err(e) = worktree::git(repo, &["update-ref", &name, &commit]) {
        return Preserved::no(format!("update-ref {name} failed: {e}"));
    }
    // Read it back: a preservation that is not verifiable is not a
    // preservation, and `preserved == true` is what authorizes a delete.
    match worktree::git(repo, &["rev-parse", &format!("{name}^{{tree}}")]) {
        Ok(t) if t.trim() == tree => Preserved {
            preserved: true,
            git_ref: Some(name),
            commit: Some(commit),
            reason: None,
        },
        Ok(t) => Preserved::no(format!(
            "{name} read back as tree {} but {tree} was written",
            t.trim()
        )),
        Err(e) => Preserved::no(format!("{name} does not read back after update-ref: {e}")),
    }
}

// ── The reconciliation itself ──────────────────────────────────────────────

/// Cross-check the on-disk worktrees against condukt's run state and decide,
/// for each, whether it may be deleted. `preserve_dirty` additionally captures
/// the content of every dead-but-dirty worktree of this repo into
/// `refs/preserved/...` before re-judging it.
pub fn reconcile(cfg: &Config, cwd: &Path, repo: &Path, preserve_dirty: bool) -> Result<Report> {
    let now = state::now_secs();
    let window = progress::window_secs(progress::DEFAULT_WINDOW_SECS);
    let store = crate::claim::progress_store_dir(cfg, cwd);
    let index = scan_state_root(&cfg.state_dir);
    let repo_git_dir = worktree::repo_git_common_dir(repo);

    // Source 1a: git's own registry, primary included.
    let mut listed: Vec<(PathBuf, Option<String>, Role)> = worktree::list_all(repo)?
        .into_iter()
        .map(|(p, b, is_primary)| {
            let role = if is_primary {
                Role::Primary
            } else {
                Role::Registered
            };
            (p, b, role)
        })
        .collect();
    // Source 1b: what is on disk that git does not register.
    for p in worktree::unregistered_dirs(repo, &cfg.worktree_base)? {
        listed.push((p, None, Role::Unregistered));
    }

    let mut entries = Vec::new();
    for (path, branch, role) in listed {
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        let attribution = match role {
            // git itself vouches for a tree it registers.
            Role::Primary | Role::Registered => Determination::Known(Attribution::ThisRepo),
            Role::Unregistered => attribution(&path, repo_git_dir.as_ref()),
        };

        let claims_here: Vec<TaskClaim> =
            index.by_worktree.get(&canon).cloned().unwrap_or_default();
        let mut claims = Vec::new();
        let mut any_progressing = false;
        for c in &claims_here {
            let verdict = claim_progress_verdict(&store, c, &canon, now, window);
            if matches!(
                verdict,
                Determination::Known(progress::Liveness::Progressing)
            ) {
                any_progressing = true;
            }
            let (word, reason) = liveness_word(&verdict);
            claims.push(Claim {
                run_id: c.run_id.clone(),
                task_id: c.task_id.clone(),
                progress: word,
                progress_reason: reason,
                state_namespace: c.namespace.clone(),
                claimed_worktree: c.worktree.to_string_lossy().to_string(),
            });
        }

        let occupancy: Determination<Occupancy> = if any_progressing {
            Determination::Known(Occupancy::Live)
        } else if !claims_here.is_empty() {
            // Claimed, but not observed advancing. FROZEN IS NOT DEAD: a
            // worker that is thinking, or whose signals we could not sample,
            // is stalled by this measure and alive in fact.
            Determination::undetermined(format!(
                "{} running task(s) claim this worktree but none was observed \
                 progressing; a frozen fingerprint is not proof of death",
                claims_here.len()
            ))
        } else if index.complete {
            Determination::Known(Occupancy::Dead)
        } else {
            Determination::undetermined(format!(
                "condukt's run state could not be read in full ({}), so 'no task \
                 claims this worktree' cannot be concluded",
                index
                    .reason
                    .clone()
                    .unwrap_or_else(|| "reason unavailable".into())
            ))
        };

        let dirty = dirtiness(&canon);

        let preserved = match (&role, &attribution, &occupancy, &dirty, preserve_dirty) {
            // Preservation is only meaningful where it could authorize a
            // delete: a dead, dirty, attributable, non-primary worktree.
            (
                Role::Registered | Role::Unregistered,
                Determination::Known(Attribution::ThisRepo),
                Determination::Known(Occupancy::Dead),
                Determination::Known(true),
                true,
            ) => preserve(&canon, repo, now),
            (
                Role::Registered | Role::Unregistered,
                Determination::Known(Attribution::ThisRepo),
                Determination::Known(Occupancy::Dead),
                Determination::Known(true),
                false,
            ) => preservation_status(&canon, repo),
            _ => Preserved::no("preservation not applicable to this entry"),
        };

        let removable = is_removable(role, &attribution, &occupancy, &dirty, preserved.preserved);
        entries.push(Entry {
            path: canon.to_string_lossy().to_string(),
            role: role.as_str(),
            branch,
            attribution: Judged::from_attribution(&attribution),
            occupancy: Judged::from_occupancy(&occupancy),
            dirty: Judged::from_dirty(&dirty),
            preserved,
            removable,
            claims,
        });
    }

    let removable_count = entries.iter().filter(|e| e.removable).count();

    // The reverse cross-check: run state that points at a directory that is
    // not on disk.
    let seen: std::collections::BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let mut dangling_claims = Vec::new();
    for (canon, claims) in &index.by_worktree {
        if seen.contains(canon.to_string_lossy().as_ref()) || canon.exists() {
            continue;
        }
        for c in claims {
            dangling_claims.push(Claim {
                run_id: c.run_id.clone(),
                task_id: c.task_id.clone(),
                progress: "undetermined".into(),
                progress_reason: Some(
                    "the task's recorded worktree does not exist on disk — there \
                     is no task-scoped signal to sample"
                        .into(),
                ),
                state_namespace: c.namespace.clone(),
                claimed_worktree: c.worktree.to_string_lossy().to_string(),
            });
        }
    }

    Ok(Report {
        repo: repo.to_string_lossy().to_string(),
        worktree_base: cfg.worktree_base.to_string_lossy().to_string(),
        state_root: cfg.state_dir.to_string_lossy().to_string(),
        now,
        window_secs: window,
        state_scan: StateScan {
            readable: index.complete,
            namespaces: index.namespaces,
            runs: index.runs,
            running_tasks: index.running_tasks,
            reason: index.reason,
        },
        entries,
        removable_count,
        dangling_claims,
    })
}

/// Human-readable rendering: one line per worktree, always naming the reason a
/// directory is being kept. Silence about a kept directory would be its own
/// small fail-open — the operator could not tell "checked and alive" from
/// "could not check".
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "state root {} — {}{}\n",
        report.state_root,
        if report.state_scan.readable {
            format!(
                "{} namespace(s), {} run(s), {} running task(s) with a worktree",
                report.state_scan.namespaces,
                report.state_scan.runs,
                report.state_scan.running_tasks
            )
        } else {
            "NOT FULLY READABLE".to_string()
        },
        report
            .state_scan
            .reason
            .as_ref()
            .map(|r| format!(" [{r}]"))
            .unwrap_or_default()
    ));
    for e in &report.entries {
        out.push_str(&format!(
            "{:<12} {:<10} occupancy={:<13} dirty={:<13} preserved={:<5} removable={}\n  {}\n",
            e.role,
            e.branch.clone().unwrap_or_else(|| "-".into()),
            e.occupancy.value.to_string().trim_matches('"'),
            e.dirty.value.to_string().trim_matches('"'),
            e.preserved.preserved,
            e.removable,
            e.path
        ));
        for c in &e.claims {
            out.push_str(&format!(
                "  claimed by {}::{} (namespace {}) progress={}\n",
                c.run_id, c.task_id, c.state_namespace, c.progress
            ));
        }
        if !e.removable {
            let why = if e.role == Role::Primary.as_str() {
                "the primary working tree is never removable".to_string()
            } else if let Some(r) = &e.attribution.reason {
                r.clone()
            } else if let Some(r) = &e.occupancy.reason {
                r.clone()
            } else if let Some(r) = &e.dirty.reason {
                r.clone()
            } else if e.attribution.value == "other-repo" {
                "belongs to another repository".to_string()
            } else if e.occupancy.value == "live" {
                "a running task is progressing here".to_string()
            } else if e.dirty.value == true {
                "holds uncommitted work that has not been preserved \
                 (run `condukt worktree reconcile --preserve`)"
                    .to_string()
            } else {
                "kept".to_string()
            };
            out.push_str(&format!("  keep: {why}\n"));
        }
    }
    for c in &report.dangling_claims {
        out.push_str(&format!(
            "dangling claim: {}::{} (namespace {}) records worktree {} which does not exist\n",
            c.run_id, c.task_id, c.state_namespace, c.claimed_worktree
        ));
    }
    out.push_str(&format!(
        "{} of {} entries are removable\n",
        report.removable_count,
        report.entries.len()
    ));
    out
}

// ── The decision table (`--decision-table`) ────────────────────────────────

/// Every combination of [`is_removable`]'s inputs, with the decision the real
/// function produced for it.
///
/// condukt is a binary crate with no library target, so an integration test
/// cannot call the decision function directly. Rather than let the whole input
/// space go untested from the outside — or, worse, test the *shape* of a
/// `Determination` instead of the decision (a shape assertion on an
/// `Undetermined` arm can never be observed RED) — the binary prints the table
/// and the test asserts on the booleans in it.
pub fn decision_table() -> serde_json::Value {
    let roles = [Role::Primary, Role::Registered, Role::Unregistered];
    let attributions: Vec<(&str, Determination<Attribution>)> = vec![
        ("this-repo", Determination::Known(Attribution::ThisRepo)),
        ("other-repo", Determination::Known(Attribution::OtherRepo)),
        (
            "undetermined",
            Determination::undetermined("decision-table probe: attribution"),
        ),
    ];
    let occupancies: Vec<(&str, Determination<Occupancy>)> = vec![
        ("live", Determination::Known(Occupancy::Live)),
        ("dead", Determination::Known(Occupancy::Dead)),
        (
            "undetermined",
            Determination::undetermined("decision-table probe: occupancy"),
        ),
    ];
    let dirtiness: Vec<(&str, Determination<bool>)> = vec![
        ("true", Determination::Known(true)),
        ("false", Determination::Known(false)),
        (
            "undetermined",
            Determination::undetermined("decision-table probe: dirty"),
        ),
    ];

    let mut rows = Vec::new();
    for role in roles {
        for (aname, a) in &attributions {
            for (oname, o) in &occupancies {
                for (dname, d) in &dirtiness {
                    for preserved in [false, true] {
                        rows.push(serde_json::json!({
                            "role": role.as_str(),
                            "attribution": aname,
                            "occupancy": oname,
                            "dirty": dname,
                            "preserved": preserved,
                            "removable": is_removable(role, a, o, d, preserved),
                        }));
                    }
                }
            }
        }
    }
    serde_json::json!({ "rows": rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn und<T>(msg: &str) -> Determination<T> {
        Determination::undetermined(msg)
    }

    /// The whole point, stated as a property rather than a table: no input
    /// combination containing an undetermined answer is removable. Behavioural
    /// — it asserts on the boolean decision, never on the shape of a
    /// `Determination`.
    #[test]
    fn no_undetermined_input_is_ever_removable() {
        let table = decision_table();
        let rows = table["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3 * 3 * 3 * 3 * 2);
        let mut undetermined_seen = 0;
        let mut removable_seen = 0;
        for r in rows {
            let has_und = ["attribution", "occupancy", "dirty"]
                .iter()
                .any(|k| r[*k] == "undetermined");
            let decision = r["removable"].as_bool().unwrap();
            if has_und {
                undetermined_seen += 1;
                assert!(!decision, "undetermined input judged removable: {r}");
            }
            if decision {
                removable_seen += 1;
            }
        }
        assert!(undetermined_seen > 0);
        // Anti-vacuity: a function returning `false` unconditionally would
        // satisfy every assertion above.
        assert!(
            removable_seen > 0,
            "the decision function never says yes — the assertions above prove nothing"
        );
    }

    #[test]
    fn primary_is_never_removable_in_any_combination() {
        for (_, a) in [
            ("k", Determination::Known(Attribution::ThisRepo)),
            ("u", und("x")),
        ] {
            for (_, o) in [
                ("k", Determination::Known(Occupancy::Dead)),
                ("u", und("x")),
            ] {
                for d in [Determination::Known(false), Determination::Known(true)] {
                    for preserved in [false, true] {
                        assert!(
                            !is_removable(Role::Primary, &a, &o, &d, preserved),
                            "the primary tree must never be removable"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dead_dirty_needs_preservation_dead_clean_does_not() {
        let a = Determination::Known(Attribution::ThisRepo);
        let o = Determination::Known(Occupancy::Dead);
        assert!(!is_removable(
            Role::Unregistered,
            &a,
            &o,
            &Determination::Known(true),
            false
        ));
        assert!(is_removable(
            Role::Unregistered,
            &a,
            &o,
            &Determination::Known(true),
            true
        ));
        assert!(is_removable(
            Role::Unregistered,
            &a,
            &o,
            &Determination::Known(false),
            false
        ));
    }

    #[test]
    fn live_is_never_removable_even_when_clean_and_preserved() {
        assert!(!is_removable(
            Role::Registered,
            &Determination::Known(Attribution::ThisRepo),
            &Determination::Known(Occupancy::Live),
            &Determination::Known(false),
            true
        ));
    }

    /// An absent state root must not read as "no runs exist ⇒ nothing alive".
    #[test]
    fn absent_state_root_makes_the_scan_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let idx = scan_state_root(&tmp.path().join("does-not-exist"));
        assert!(!idx.complete);
        assert!(idx.reason.is_some());
    }

    /// One unparseable run file poisons the whole scan: nothing may be
    /// concluded dead while a run we could not read might claim it.
    #[test]
    fn unparseable_run_file_makes_the_scan_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let ns = tmp.path().join("ns-1");
        std::fs::create_dir_all(&ns).unwrap();
        std::fs::write(ns.join("run-x.json"), "{ not json").unwrap();
        let idx = scan_state_root(tmp.path());
        assert!(!idx.complete);
        assert!(idx.reason.unwrap().contains("unparseable"));
    }

    /// Sidecars that never deserialize as run state are skipped, not counted
    /// as corruption (otherwise every real state dir would be "incomplete").
    #[test]
    fn sidecars_do_not_poison_the_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let ns = tmp.path().join("ns-1");
        std::fs::create_dir_all(&ns).unwrap();
        std::fs::write(ns.join("claims.json"), "{\"claims\":[]}").unwrap();
        std::fs::write(ns.join("run-x.decomposition.json"), "{\"tasks\":[]}").unwrap();
        std::fs::write(ns.join("run-x.checkpoints.json"), "[]").unwrap();
        let idx = scan_state_root(tmp.path());
        assert!(idx.complete, "reason: {:?}", idx.reason);
        assert_eq!(idx.runs, 0);
    }

    /// A RUNNING task in ANY namespace is found — the scan is not scoped to
    /// this checkout's own project key (backlog 4708069b).
    #[test]
    fn running_tasks_are_indexed_across_namespaces() {
        let tmp = tempfile::tempdir().unwrap();
        for (ns, wt) in [("ns-a", "/tmp/wt-a"), ("ns-b", "/tmp/wt-b")] {
            let dir = tmp.path().join(ns);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("run-1.json"),
                format!(
                    r#"{{"run_id":"run-1","goal":"g","tasks":[{{"id":"t","status":"running","worktree":"{wt}","updated_at":1}}]}}"#
                ),
            )
            .unwrap();
        }
        let idx = scan_state_root(tmp.path());
        assert!(idx.complete, "reason: {:?}", idx.reason);
        assert_eq!(idx.namespaces, 2);
        assert_eq!(idx.running_tasks, 2);
        assert_eq!(idx.by_worktree.len(), 2);
    }

    /// A non-running task does not make a worktree occupied.
    #[test]
    fn only_running_tasks_claim_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ns");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("run-1.json"),
            r#"{"run_id":"run-1","goal":"g","tasks":[{"id":"t","status":"verified","worktree":"/tmp/wt","updated_at":1}]}"#,
        )
        .unwrap();
        let idx = scan_state_root(tmp.path());
        assert!(idx.complete);
        assert_eq!(idx.runs, 1);
        assert!(idx.by_worktree.is_empty());
    }

    #[test]
    fn preserved_ref_names_are_path_scoped_and_ref_safe() {
        let a = preserved_ref_name(Path::new("/a/b/session-x"));
        let b = preserved_ref_name(Path::new("/c/d/session-x"));
        assert!(a.starts_with("refs/preserved/session-x-"));
        assert_ne!(a, b, "same basename under different paths must not collide");
        let weird = preserved_ref_name(Path::new("/a/b/we ird~name"));
        assert!(!weird.contains(' ') && !weird.contains('~'), "{weird}");
    }

    /// A directory that is not a git worktree root cannot be judged clean —
    /// including the case where it sits INSIDE another repository, where a
    /// naive `git status` would answer with that repository's state.
    #[test]
    fn non_worktree_dir_dirtiness_is_undetermined() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("plain");
        std::fs::create_dir_all(&d).unwrap();
        assert!(matches!(dirtiness(&d), Determination::Undetermined(_)));
    }
}
