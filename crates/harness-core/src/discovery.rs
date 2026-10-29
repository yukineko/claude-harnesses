//! Machine-scope shared "discovery record" store. Append-only JSONL.
//!
//! Concurrent compass/scout sessions on one machine use this to avoid duplicating
//! discovered tasks. Records are keyed by task fingerprint (content hash of title)
//! and annotated with the discovering session and current status (Discovered or Selected).
//!
//! The store lives at `~/.compass/<project_key(main worktree root)>/discovery.jsonl`,
//! where the root comes from [`crate::projkey::main_worktree_root`]. That identity is
//! the same from the main checkout, from every linked worktree and from any
//! subdirectory of either, so all of them share ONE store.
//!
//! Failure is reported, never turned into "nothing found" (CLAUDE.md section 3):
//! - an undetermined main-worktree root has no store path, so every operation is
//!   `Undetermined` — there is no fallback to `git rev-parse --show-toplevel` or to
//!   the cwd, because that would write to / read from a guessed store;
//! - [`load`] returns `Known(vec![])` ONLY for a store that was never written
//!   (file absent). A read error (permissions, the path being a directory, non-UTF-8
//!   contents) is `Undetermined`;
//! - [`already_discovered_by_other`] is a `Determination<bool>`, so an unreadable
//!   store cannot be read as "no duplicate";
//! - [`append`] / [`mark_selected`] report a dropped write.
//!
//! A corrupt (non-blank, non-record) JSONL line also makes the load `Undetermined`:
//! a skipped row could be the one that marks a task as a duplicate. Blank lines are fine.
//!
//! Legacy records: before the store was keyed on the main worktree root it was keyed
//! on `git rev-parse --show-toplevel` (see [`resolve_repo_root`]), i.e. on the linked
//! worktree itself. [`load`] therefore also reads THIS cwd's legacy worktree-keyed
//! file (when its key differs from the canonical one). Locating that file needs
//! git: when git cannot answer inside a git checkout, the load is `Undetermined`
//! rather than looking the file up under a guessed key. Legacy files of OTHER or
//! since-removed worktrees are not reachable from here and are not read; copy them
//! into the canonical store with [`migrate_legacy`]. A row present in both files is
//! listed once.

use std::path::{Path, PathBuf};

use crate::verdict::Determination;

use serde::{Deserialize, Serialize};

/// Status of a discovered task: either newly Discovered or Selected by a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Discovered,
    Selected,
}

/// A single discovery record: fingerprint, session, status, timestamp, and title.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryRecord {
    pub fingerprint: String,
    pub session_id: String,
    pub status: Status,
    pub created_at: u64,
    pub title: String,
}

/// Resolve the `git rev-parse --show-toplevel` root for a cwd, canonicalized.
///
/// NOTE: in a LINKED worktree this is the worktree itself, NOT the main
/// checkout — two worktrees of one repo resolve to two different roots. It is the
/// key the discovery store used before it moved to
/// [`crate::projkey::main_worktree_root`] (see the module docs, "Legacy records"),
/// and it is still used by other callers (e.g. backlog) that want the
/// per-checkout root. The discovery store itself no longer keys on this.
///
/// Strategy: ask git for the toplevel (`git -C <cwd> rev-parse
/// --show-toplevel`), trim it, and canonicalize. Total (never errors): if git is
/// missing, the dir isn't a repo, or any IO/parse error occurs, fall back to
/// `cwd.canonicalize()`, and finally to a purely lexical normalization of the
/// raw cwd (see [`lexical_normalize`]) for paths that don't exist locally at
/// all. NEVER panics. The fallback is a guess, which is why the discovery store
/// does not use it for its canonical path.
pub fn resolve_repo_root(cwd: &Path) -> PathBuf {
    if let Some(top) = git_toplevel(cwd) {
        if let Ok(canon) = top.canonicalize() {
            return canon;
        }
        return top;
    }
    cwd.canonicalize()
        .unwrap_or_else(|_| lexical_normalize(cwd))
}

/// Purely lexical path normalization: resolves `.` and `..` components via
/// `Path::components()` without touching the filesystem (no `canonicalize`,
/// no `stat`).
///
/// Why this exists: `resolve_repo_root`'s last-resort fallback fires when the
/// path is NOT a git repo (or git is absent) AND `fs::canonicalize` fails
/// because nothing exists at that path on THIS machine/worktree — e.g. a
/// project key was recorded by a different machine/session referencing a path
/// that isn't present here. Before this function existed, that fallback
/// returned the raw, unresolved `cwd` verbatim: no `.`/`..` collapsing, no
/// trailing-slash stripping. So `/foo/bar/`, `/foo/./bar`, and
/// `/foo/baz/../bar` — three spellings of the SAME logical path — would each
/// produce a DIFFERENT resolved key, silently fragmenting one project's
/// tasks/records across multiple stores. Since no filesystem lookup is
/// possible in this branch (that's exactly why we're here), the only
/// available fix is lexical: collapse what can be collapsed from the string
/// alone, so the same logical path collapses to the same key regardless of
/// which machine/worktree wrote it.
///
/// Semantics: `CurDir` (`.`) components are dropped. A `ParentDir` (`..`)
/// pops the last resolved `Normal` component if one exists; otherwise, for an
/// absolute path, it is dropped (you can't go above root); for a relative
/// path with nothing left to pop, the `..` is kept verbatim since it can't be
/// resolved without a filesystem or a known base directory.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {
                    // Can't go above root; drop the `..`.
                }
                _ => {
                    out.push("..");
                }
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Best-effort `git -C <cwd> rev-parse --show-toplevel`. Returns `None` on any
/// spawn/exit/parse failure (not a repo, git absent, empty output, ...).
fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    git_toplevel_detailed(cwd).ok()
}

/// `git -C <cwd> rev-parse --show-toplevel`, with the reason when git could not
/// answer (spawn failure such as git absent, non-zero exit, non-UTF-8 or empty
/// output).
fn git_toplevel_detailed(cwd: &Path) -> Result<PathBuf, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|e| format!("cannot run `git rev-parse --show-toplevel` ({e})"))?;
    if !output.status.success() {
        return Err(format!(
            "`git rev-parse --show-toplevel` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let top = String::from_utf8(output.stdout)
        .map_err(|_| "`git rev-parse --show-toplevel` output is not UTF-8".to_string())?;
    let top = top.trim();
    if top.is_empty() {
        return Err("`git rev-parse --show-toplevel` printed nothing".to_string());
    }
    Ok(PathBuf::from(top))
}

/// Path to the CANONICAL discovery.jsonl store for a given cwd:
/// `~/.compass/<project_key(main worktree root)>/discovery.jsonl`.
///
/// `Undetermined` when [`crate::projkey::main_worktree_root`] cannot decide the
/// root (e.g. a corrupt `.git` gitfile). There is deliberately no fallback to
/// [`resolve_repo_root`] or to the cwd: a guessed key would silently split or
/// misplace the shared store.
pub fn record_path(cwd: &Path) -> Determination<PathBuf> {
    crate::projkey::main_worktree_root(cwd).map(|root| store_path_for_root(&root))
}

fn store_path_for_root(root: &Path) -> PathBuf {
    crate::config::base_dir("compass")
        .join(crate::store::project_key(root))
        .join("discovery.jsonl")
}

/// The pre-canonical, worktree-keyed store file for this cwd (what an older binary
/// wrote from a linked worktree), or `Known(None)` when it is the same file as
/// `canonical`.
///
/// The legacy key is the `git rev-parse --show-toplevel` root. When git answers,
/// that root is used. When git cannot answer:
/// - and no ancestor of `cwd` has a `.git` entry (not a checkout at all), the
///   older binary's own fallback (`cwd` canonicalized, see [`resolve_repo_root`])
///   is exactly the key it wrote under, so it is reproduced;
/// - but the filesystem shows a git checkout, the key the older binary used is
///   the toplevel git would have printed, and `cwd.canonicalize()` is only a
///   guess (from a subdirectory it is the subdirectory, so the legacy file would
///   be looked up under the wrong key and its rows silently dropped). That is
///   `Undetermined`, never a guessed path.
fn legacy_path(cwd: &Path, canonical: &Path) -> Determination<Option<PathBuf>> {
    let root = match git_toplevel_detailed(cwd) {
        Ok(top) => top.canonicalize().unwrap_or(top),
        Err(why) => {
            let fs_root = crate::projkey::repo_root(cwd);
            if fs_root.join(".git").exists() {
                return Determination::undetermined(format!(
                    "discovery store: the legacy worktree-keyed file for {} cannot be \
                     located: {} is a git checkout but {why}; its rows cannot be read, so \
                     the store's contents cannot be decided",
                    cwd.display(),
                    fs_root.display()
                ));
            }
            resolve_repo_root(cwd)
        }
    };
    let legacy = store_path_for_root(&root);
    Determination::known((legacy != canonical).then_some(legacy))
}

/// Deterministic content hash of a title. Returns a hex string of the FNV-1a 64-bit
/// hash, formatted as 16 lowercase hex digits (64 bits).
pub fn fingerprint(title: &str) -> String {
    let h = crate::hash::fnv1a64(title.as_bytes());
    format!("{:016x}", h)
}

// Test-only race-window widener for the gap between `mark_selected_at`'s load
// and its later rewrite. Real process-spawn overhead dwarfs that gap, so two
// racing CLI invocations essentially never interleave inside it by chance — a
// concurrency regression test needs a deterministic way to force the
// interleave. No-op unless `DISCOVERY_TEST_LOAD_DELAY_MS` is set (never set
// outside `compass`'s `discovery_lock_concurrency` integration test). Mirrors
// `hypothesis::store::artificial_race_delay` / `overwatch::lease::artificial_race_delay`.
fn artificial_race_delay() {
    if let Some(ms) = std::env::var("DISCOVERY_TEST_LOAD_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Append a discovery record to the canonical store.
///
/// `Known(())` once the line is written. `Undetermined` (with the reason) when the
/// store path cannot be resolved, the lock cannot be taken, or the write fails —
/// the record is NOT stored in that case and the caller must say so.
pub fn append(cwd: &Path, rec: &DiscoveryRecord) -> Determination<()> {
    match record_path(cwd) {
        Determination::Known(path) => append_at(&path, rec),
        Determination::Undetermined(why) => why.into_determination(),
    }
}

/// Internal: append to an explicit path. Used by append() and by tests.
///
/// Holds the same advisory lock as [`mark_selected_at`] across the write:
/// `append_line`'s single `O_APPEND` write is atomic against other appends on
/// its own, but a concurrent `mark_selected_at` reads a snapshot of the whole
/// file and later rewrites it wholesale — without a shared lock, a rewrite
/// that started before this append lands would silently clobber it on rename
/// (lost update, not just a torn write).
fn append_at(path: &Path, rec: &DiscoveryRecord) -> Determination<()> {
    let json = match serde_json::to_string(rec) {
        Ok(j) => j,
        Err(e) => {
            return Determination::undetermined(format!(
                "discovery store: could not serialize the record ({e}); not stored"
            ))
        }
    };

    // The lock file lives next to the store; acquire_lock creates the parent dir.
    let Some(_guard) = crate::lessons::acquire_lock(path) else {
        return Determination::undetermined(format!(
            "discovery store: could not take the lock for {}; record not stored",
            path.display()
        ));
    };
    // Single atomic append (body + '\n' in one write) — see issue #15.
    match crate::append::append_line(path, &json) {
        Ok(()) => Determination::known(()),
        Err(e) => Determination::undetermined(format!(
            "discovery store: could not append to {} ({e}); record not stored",
            path.display()
        )),
    }
}

/// Load all discovery records visible from `cwd`: the canonical store first, then
/// this cwd's legacy worktree-keyed file (see the module docs).
///
/// `Known(vec![])` only when neither file exists (never written). Any root /
/// legacy-path (git unavailable inside a checkout) / read failure — including a path that is a directory or is unreadable — is
/// `Undetermined`: an empty `Vec` here would be read downstream as "no duplicates".
/// A corrupt non-blank line in either file is also `Undetermined`. A row present in
/// both files (same fingerprint, session, created_at, title) is returned once.
pub fn load(cwd: &Path) -> Determination<Vec<DiscoveryRecord>> {
    let canonical = match record_path(cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => return why.into_determination(),
    };
    let mut records = match load_path(&canonical) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return why.into_determination(),
    };
    let legacy = match legacy_path(cwd, &canonical) {
        Determination::Known(l) => l,
        Determination::Undetermined(why) => return why.into_determination(),
    };
    if let Some(legacy) = legacy {
        match load_path(&legacy) {
            Determination::Known(r) => {
                // A migrated row exists in both files; list it once.
                let seen: std::collections::HashSet<RowId> = records.iter().map(row_id).collect();
                records.extend(r.into_iter().filter(|x| !seen.contains(&row_id(x))));
            }
            Determination::Undetermined(why) => return why.into_determination(),
        }
    }
    Determination::known(records)
}

/// Internal: load from an explicit path. Absent file => `Known(empty)`; any other
/// read error or corrupt non-blank line => `Undetermined`.
fn load_path(path: &Path) -> Determination<Vec<DiscoveryRecord>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::known(Vec::new())
        }
        Err(e) => {
            return Determination::undetermined(format!(
                "discovery store: cannot read {} ({e}); whether it holds records cannot be decided",
                path.display()
            ))
        }
    };

    let mut records = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A non-blank line that is not a record means the store is damaged: a
        // skipped row could be the very row that makes a task a duplicate, so
        // the whole load is undetermined rather than silently shorter.
        match serde_json::from_str::<DiscoveryRecord>(line) {
            Ok(rec) => records.push(rec),
            Err(e) => {
                return Determination::undetermined(format!(
                    "discovery store: {} line {} is not a discovery record ({e}); \
                     the store's contents cannot be decided",
                    path.display(),
                    idx + 1
                ))
            }
        }
    }
    Determination::known(records)
}

/// Row identity used to de-duplicate a legacy row against the canonical store.
type RowId = (String, String, u64, String);

fn row_id(r: &DiscoveryRecord) -> RowId {
    (
        r.fingerprint.clone(),
        r.session_id.clone(),
        r.created_at,
        r.title.clone(),
    )
}

/// What [`migrate_legacy`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationSummary {
    /// Legacy rows newly appended to the canonical store.
    pub migrated: usize,
    /// Legacy rows skipped because an identical row (fingerprint, session,
    /// created_at, title) was already in the canonical store or an earlier source.
    pub already_present: usize,
    /// Each legacy file that exists, with its parsed row count.
    pub sources: Vec<MigrationSource>,
}

/// One legacy file read by [`migrate_legacy`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationSource {
    pub path: PathBuf,
    pub rows: usize,
}

/// Copy the legacy worktree-keyed rows of every LINKED worktree of this repo into
/// the canonical store. All-or-nothing: if the root, `git worktree list`, any
/// worktree path, any legacy file (unreadable, or with a corrupt line) or the
/// canonical store cannot be determined, the result is `Undetermined` and nothing
/// is written. Legacy files are never modified. Idempotent: rows already present
/// (identity = fingerprint, session, created_at, title) are not appended again.
pub fn migrate_legacy(cwd: &Path) -> Determination<MigrationSummary> {
    let root = match crate::projkey::main_worktree_root(cwd) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return why.into_determination(),
    };
    let canonical = store_path_for_root(&root);

    let out = match std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["worktree", "list", "--porcelain"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Determination::undetermined(format!(
                "discovery migrate: cannot run `git worktree list` ({e})"
            ))
        }
    };
    if !out.status.success() {
        return Determination::undetermined(format!(
            "discovery migrate: `git worktree list --porcelain` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let Ok(text) = String::from_utf8(out.stdout) else {
        return Determination::undetermined(
            "discovery migrate: `git worktree list` output is not UTF-8",
        );
    };
    let main_canon = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!(
                "discovery migrate: cannot canonicalize the main worktree {} ({e})",
                root.display()
            ))
        }
    };

    let _guard = match crate::lessons::acquire_lock(&canonical) {
        Some(g) => g,
        None => {
            return Determination::undetermined(format!(
                "discovery migrate: could not take the lock for {}; nothing migrated",
                canonical.display()
            ))
        }
    };
    let mut seen: std::collections::HashSet<RowId> = match load_path(&canonical) {
        Determination::Known(r) => r.iter().map(row_id).collect(),
        Determination::Undetermined(why) => return why.into_determination(),
    };

    let mut sources = Vec::new();
    let mut new_lines = Vec::new();
    let (mut migrated, mut already_present) = (0usize, 0usize);
    for wt in text.lines().filter_map(|l| l.strip_prefix("worktree ")) {
        let wt_canon = match Path::new(wt).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return Determination::undetermined(format!(
                    "discovery migrate: cannot canonicalize worktree {wt} ({e}); its legacy \
                     file cannot be located (run `git worktree prune` if it was removed)"
                ))
            }
        };
        if wt_canon == main_canon {
            continue;
        }
        let legacy = store_path_for_root(&wt_canon);
        if legacy == canonical {
            continue;
        }
        let rows = match load_path(&legacy) {
            Determination::Known(r) => r,
            Determination::Undetermined(why) => return why.into_determination(),
        };
        if !legacy.exists() {
            continue;
        }
        sources.push(MigrationSource {
            path: legacy,
            rows: rows.len(),
        });
        for rec in rows {
            if seen.insert(row_id(&rec)) {
                match serde_json::to_string(&rec) {
                    Ok(j) => new_lines.push(j),
                    Err(e) => {
                        return Determination::undetermined(format!(
                            "discovery migrate: cannot serialize a row ({e}); nothing migrated"
                        ))
                    }
                }
                migrated += 1;
            } else {
                already_present += 1;
            }
        }
    }

    if !new_lines.is_empty() {
        // One write for every new row: all of them land, or (on error) the
        // caller is told and gets nothing reported as migrated.
        if let Err(e) = crate::append::append_line(&canonical, &new_lines.join("\n")) {
            return Determination::undetermined(format!(
                "discovery migrate: could not append to {} ({e}); migration not applied",
                canonical.display()
            ));
        }
    }
    Determination::known(MigrationSummary {
        migrated,
        already_present,
        sources,
    })
}

/// Whether a *different* session owns `fingerprint` — i.e. should this session
/// suppress it from its surface (cross-session dedup).
///
/// Ownership is by *earliest discovery*: the owner is the session with the
/// earliest row for the fingerprint (`created_at`, then `session_id` for a
/// deterministic tie-break), **regardless of status**. A `Selected` row still
/// counts, so a task another session is actively working keeps suppressing this
/// one. `Known(true)` iff that owner is a different session than `my_session`.
///
/// This realises the DoD's "the *later* discovery does not re-surface the
/// duplicate" rule: the first discoverer keeps the task and every later session
/// drops it (stable single owner — no mutual annihilation where both sessions
/// see each other's row and both drop it). A never-written store has no owner:
/// `Known(false)`. An unreadable / undeterminable store is `Undetermined` — NOT
/// `false`; the caller decides what to do and must say that dedup did not run.
pub fn already_discovered_by_other(
    cwd: &Path,
    fingerprint: &str,
    my_session: &str,
) -> Determination<bool> {
    load(cwd).map(|records| owned_by_other(&records, fingerprint, my_session))
}

/// Pure ownership check over an already-loaded record set — the core of
/// [`already_discovered_by_other`], split out so it is testable without touching
/// the real store. The owner is the earliest row for `fingerprint` (by
/// `created_at`, then `session_id`); returns true iff that owner is a different
/// session than `my_session`. No matching row → no owner → false.
fn owned_by_other(records: &[DiscoveryRecord], fingerprint: &str, my_session: &str) -> bool {
    records
        .iter()
        .filter(|r| r.fingerprint == fingerprint)
        .min_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        })
        .map(|owner| owner.session_id != my_session)
        .unwrap_or(false)
}

/// Mark a task as selected in the CANONICAL store, rewriting it atomically. All
/// records matching the fingerprint have their status set to Selected; all others
/// remain unchanged. Legacy worktree-keyed files are not rewritten (ownership is
/// status-agnostic, so a legacy `Discovered` row still dedups identically).
///
/// `Known(())` when the rewrite landed. `Undetermined` when the root cannot be
/// resolved, the lock or the read fails, or the rewrite fails — in which case the
/// file is left as it was.
pub fn mark_selected(cwd: &Path, fingerprint: &str) -> Determination<()> {
    match record_path(cwd) {
        Determination::Known(path) => mark_selected_at(&path, fingerprint),
        Determination::Undetermined(why) => why.into_determination(),
    }
}

/// Internal: mark selected on an explicit path. Used by mark_selected() and by tests.
///
/// Holds the same advisory lock as [`append_at`] across the whole
/// load->mutate->rewrite cycle: without it, a concurrent `append_at` (or
/// another `mark_selected_at`) landing between this fn's load and its rename
/// would be silently discarded by the wholesale rewrite (lost update).
fn mark_selected_at(path: &Path, fingerprint: &str) -> Determination<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    let Some(_guard) = crate::lessons::acquire_lock(path) else {
        return Determination::undetermined(format!(
            "discovery store: could not take the lock for {}; nothing marked",
            path.display()
        ));
    };

    // Load all records. An unreadable store must NOT be rewritten as empty.
    let mut records = match load_path(path) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return why.into_determination(),
    };
    artificial_race_delay();

    // Update matching records to Selected.
    for r in &mut records {
        if r.fingerprint == fingerprint {
            r.status = Status::Selected;
        }
    }

    // Atomic write: write to a temp file, then rename.
    // Mirrors the style of store::save_json's atomic write pattern.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("jsonl.tmp.{}.{}", std::process::id(), seq));

    let fail = |what: &str, e: &dyn std::fmt::Display| {
        let _ = std::fs::remove_file(&tmp_path);
        Determination::undetermined(format!(
            "discovery store: {what} for {} ({e}); store left unchanged",
            path.display()
        ))
    };

    let mut tmp_file = match std::fs::File::create(&tmp_path) {
        Ok(f) => f,
        Err(e) => return fail("cannot create temp file", &e),
    };
    for rec in &records {
        let json = match serde_json::to_string(rec) {
            Ok(j) => j,
            Err(e) => return fail("cannot serialize a record", &e),
        };
        if let Err(e) = writeln!(tmp_file, "{}", json) {
            return fail("cannot write temp file", &e);
        }
    }

    // Drop the file handle so the rename can succeed on Windows.
    drop(tmp_file);

    // Atomically rename the temp file to the target.
    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Determination::known(()),
        Err(e) => fail("cannot rename temp file into place", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: a write that must have landed.
    fn stored(d: Determination<()>) {
        if let Determination::Undetermined(why) = d {
            panic!("write must succeed: {why}");
        }
    }

    /// Test helper: a store path that must be determined.
    fn known_path(cwd: &Path) -> PathBuf {
        match record_path(cwd) {
            Determination::Known(p) => p,
            Determination::Undetermined(why) => panic!("expected a determined root: {why}"),
        }
    }

    /// Test helper: the records of a store that must be Known.
    fn load_at(path: &Path) -> Vec<DiscoveryRecord> {
        match load_path(path) {
            Determination::Known(r) => r,
            Determination::Undetermined(why) => panic!("expected a determined store: {why}"),
        }
    }

    #[test]
    fn status_transition_mark_selected_at() {
        // Create a temp directory and file.
        let tempdir = tempfile::tempdir().expect("tempfile::tempdir");
        let path = tempdir.path().join("discovery.jsonl");

        // Append 3 records from 2 sessions with different fingerprints.
        let rec_a = DiscoveryRecord {
            fingerprint: "aaa".to_string(),
            session_id: "session1".to_string(),
            status: Status::Discovered,
            created_at: 1000,
            title: "Task A".to_string(),
        };
        let rec_b = DiscoveryRecord {
            fingerprint: "bbb".to_string(),
            session_id: "session2".to_string(),
            status: Status::Discovered,
            created_at: 1001,
            title: "Task B".to_string(),
        };
        let rec_c = DiscoveryRecord {
            fingerprint: "ccc".to_string(),
            session_id: "session1".to_string(),
            status: Status::Discovered,
            created_at: 1002,
            title: "Task C".to_string(),
        };

        stored(append_at(&path, &rec_a));
        stored(append_at(&path, &rec_b));
        stored(append_at(&path, &rec_c));

        // Mark C as selected.
        stored(mark_selected_at(&path, "ccc"));

        // Reload and verify: only C should be Selected, A and B remain Discovered.
        let records = load_at(&path);
        assert_eq!(records.len(), 3);

        let rec_a_loaded = &records[0];
        assert_eq!(rec_a_loaded.fingerprint, "aaa");
        assert_eq!(rec_a_loaded.status, Status::Discovered);

        let rec_b_loaded = &records[1];
        assert_eq!(rec_b_loaded.fingerprint, "bbb");
        assert_eq!(rec_b_loaded.status, Status::Discovered);

        let rec_c_loaded = &records[2];
        assert_eq!(rec_c_loaded.fingerprint, "ccc");
        assert_eq!(rec_c_loaded.status, Status::Selected);
    }

    #[test]
    fn fail_soft_missing_path() {
        // load_at on a missing path should return an empty Vec, not panic.
        let records = load_at(Path::new("/nonexistent/discovery.jsonl"));
        assert_eq!(records.len(), 0);

        // mark_selected_at on an unwritable location must report it, not panic.
        assert!(matches!(
            mark_selected_at(Path::new("/nonexistent/discovery.jsonl"), "some-fp"),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn corrupt_line_is_undetermined_not_skipped() {
        // Valid / corrupt / valid lines.
        let tempdir = tempfile::tempdir().expect("tempfile::tempdir");
        let path = tempdir.path().join("discovery.jsonl");

        let valid_rec = DiscoveryRecord {
            fingerprint: "valid".to_string(),
            session_id: "s1".to_string(),
            status: Status::Discovered,
            created_at: 1000,
            title: "Valid".to_string(),
        };

        // Write manually: valid line, corrupt line, valid line.
        let valid_json = serde_json::to_string(&valid_rec).expect("to_string");
        let corrupt_line = r#"{"not": "valid json""#;
        let content = format!("{}\n{}\n{}\n", valid_json, corrupt_line, valid_json);
        std::fs::write(&path, content).expect("write");

        // A corrupt non-blank line makes the whole store undetermined (it is
        // NOT skipped): the dropped row could be the one that marks a duplicate.
        match load_path(&path) {
            Determination::Undetermined(why) => {
                assert!(why.as_str().contains("line 2"), "reason: {why}")
            }
            Determination::Known(r) => panic!("corrupt line must be undetermined, got {r:?}"),
        }

        // Blank lines are not corrupt.
        std::fs::write(&path, format!("{valid_json}\n\n  \n{valid_json}\n")).expect("write");
        assert_eq!(load_at(&path).len(), 2);
    }

    #[test]
    fn owned_by_other_is_earliest_owner_stable_and_status_agnostic() {
        // Two sessions discover the SAME fingerprint; sessA is earlier => owner.
        let rec_a = DiscoveryRecord {
            fingerprint: "same-fp".to_string(),
            session_id: "sessA".to_string(),
            status: Status::Discovered,
            created_at: 1000,
            title: "Task X".to_string(),
        };
        let rec_b = DiscoveryRecord {
            fingerprint: "same-fp".to_string(),
            session_id: "sessB".to_string(),
            status: Status::Discovered,
            created_at: 1001,
            title: "Task X".to_string(),
        };
        let recs = vec![rec_a.clone(), rec_b.clone()];

        // Earliest discoverer (sessA) OWNS it => not "by other" => keeps surfacing.
        assert!(
            !owned_by_other(&recs, "same-fp", "sessA"),
            "the earliest discoverer owns the task and keeps it"
        );
        // The later session AND any third session suppress it (DoD: "2回目 doesn't
        // re-surface"). Stable single owner — no mutual annihilation.
        assert!(
            owned_by_other(&recs, "same-fp", "sessB"),
            "a later session must suppress the duplicate"
        );
        assert!(
            owned_by_other(&recs, "same-fp", "sessC"),
            "a third session must suppress the duplicate"
        );

        // Unknown fingerprint => no owner => not suppressed (byte-equivalent).
        assert!(
            !owned_by_other(&recs, "missing-fp", "sessB"),
            "an unseen fingerprint is owned by nobody"
        );

        // Status-agnostic: once the owner SELECTS it, it STILL suppresses others
        // (active work must keep deduping) and the owner still keeps it. This is
        // the case the old status==Discovered predicate got wrong.
        let selected_owner = DiscoveryRecord {
            status: Status::Selected,
            ..rec_a
        };
        let recs_selected = vec![selected_owner, rec_b];
        assert!(
            owned_by_other(&recs_selected, "same-fp", "sessB"),
            "a Selected owner still suppresses other sessions"
        );
        assert!(
            !owned_by_other(&recs_selected, "same-fp", "sessA"),
            "a Selected owner still keeps its own task"
        );
    }

    #[test]
    fn already_discovered_by_other_reads_the_store_end_to_end() {
        // Exercise the public, store-backed entry point (not just the pure core):
        // an earlier row from another session, persisted to a real JSONL file,
        // must be observed by `owned_by_other` via `load_at`.
        let tempdir = tempfile::tempdir().expect("tempfile::tempdir");
        let path = tempdir.path().join("discovery.jsonl");
        stored(append_at(
            &path,
            &DiscoveryRecord {
                fingerprint: "fp".to_string(),
                session_id: "owner".to_string(),
                status: Status::Discovered,
                created_at: 5,
                title: "T".to_string(),
            },
        ));
        let records = load_at(&path);
        assert!(owned_by_other(&records, "fp", "latecomer"));
        assert!(!owned_by_other(&records, "fp", "owner"));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let fp1 = fingerprint("hello");
        let fp2 = fingerprint("hello");
        assert_eq!(fp1, fp2, "same title should produce same fingerprint");

        let fp3 = fingerprint("world");
        assert_ne!(
            fp1, fp3,
            "different title should produce different fingerprint"
        );

        // Check that it's 16 hex digits (64-bit hash).
        assert_eq!(fp1.len(), 16, "fingerprint should be 16 hex digits");
        assert!(
            fp1.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint should be all hex digits"
        );
    }

    #[test]
    fn record_path_shares_one_store_across_subdirs_of_one_repo() {
        // record_path() resolves HOME (via config::base_dir("compass")), and
        // the assertion below resolves it TWICE — once per call. A concurrent
        // set_var("HOME", ..) between them gives the two sides different
        // prefixes and fails a test that is about project keying, not HOME.
        let _guard = crate::test_env::lock();
        crate::test_env::clear_inherited_git_env();
        // Two distinct cwds under the SAME git repo root (the repo root itself
        // and a nested subdir) must map to the SAME record_path. This covers
        // SUBDIRS only. That linked worktrees share the main checkout's store is
        // NOT checked here; it is covered end to end through the compass binary
        // by crates/compass/tests/discovery_shared_across_worktrees.rs.
        let tempdir = tempfile::tempdir().expect("tempfile::tempdir");
        let root = tempdir.path();

        let status = crate::git_env::scrub(&mut std::process::Command::new("git"))
            .arg("-C")
            .arg(root)
            .arg("init")
            .arg("-q")
            .status()
            .expect("git init");
        assert!(status.success(), "git init should succeed");

        let sub = root.join("a/b/c");
        std::fs::create_dir_all(&sub).expect("create_dir_all");

        assert_eq!(
            known_path(root),
            known_path(&sub),
            "repo root and a subdir of the same repo must share one record_path"
        );
        assert_eq!(resolve_repo_root(root), resolve_repo_root(&sub));
    }

    #[test]
    fn record_path_includes_project_key() {
        let cwd = Path::new("/some/project/dir");
        let path = known_path(cwd);
        // Should be ~/.compass/<project_key>/discovery.jsonl
        assert!(path.to_string_lossy().contains(".compass"));
        assert!(path.to_string_lossy().ends_with("discovery.jsonl"));
    }

    // --- lexical_normalize / resolve_repo_root fallback-branch regression tests ---
    //
    // These exercise the "path does not exist locally AND is not a git repo"
    // fallback branch of `resolve_repo_root` (git absent-or-not-a-repo, then
    // `cwd.canonicalize()` fails because nothing exists at that path on THIS
    // machine). That's exactly the situation when a project key was written by
    // a different machine/worktree referencing a path not present here. Before
    // the fix, this branch returned `cwd.to_path_buf()` completely unresolved,
    // so trailing slashes / `.`/`..` segments would NOT collapse to the same
    // key as their canonical sibling representation — silently fragmenting one
    // project's tasks/records across multiple keys. Segment names below are
    // randomized/unlikely to exist on any real machine so we actually hit the
    // "doesn't exist locally" branch rather than a real directory.

    #[test]
    fn resolve_repo_root_trailing_slash_matches_no_trailing_slash_when_unresolvable() {
        let base = "/definitely/nonexistent-xyz-q7f3k9z2/foo";
        let with_slash = PathBuf::from(format!("{base}/"));
        let without_slash = PathBuf::from(base);

        assert_eq!(
            resolve_repo_root(&with_slash),
            resolve_repo_root(&without_slash),
            "a trailing slash on an unresolvable path must not change the resolved key"
        );
    }

    #[test]
    fn resolve_repo_root_dotdot_traversal_matches_lexically_collapsed_equivalent_when_unresolvable()
    {
        let with_dotdot = PathBuf::from("/definitely/nonexistent-xyz-q7f3k9z2/foo/bar/../baz");
        let collapsed = PathBuf::from("/definitely/nonexistent-xyz-q7f3k9z2/foo/baz");

        assert_eq!(
            resolve_repo_root(&with_dotdot),
            resolve_repo_root(&collapsed),
            "a `..`-traversal path must resolve to the same key as its lexically-collapsed equivalent"
        );
    }

    #[test]
    fn lexical_normalize_collapses_dotdot_and_curdir_without_touching_filesystem() {
        assert_eq!(
            lexical_normalize(Path::new(
                "/definitely/nonexistent-xyz-q7f3k9z2/foo/bar/../baz"
            )),
            PathBuf::from("/definitely/nonexistent-xyz-q7f3k9z2/foo/baz")
        );
        assert_eq!(
            lexical_normalize(Path::new("/definitely/nonexistent-xyz-q7f3k9z2/foo/")),
            PathBuf::from("/definitely/nonexistent-xyz-q7f3k9z2/foo")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/./b")),
            PathBuf::from("/a/b")
        );
        // Leading `..` past root on an absolute path can't go higher than root; drop it.
        assert_eq!(lexical_normalize(Path::new("/../a")), PathBuf::from("/a"));
        // Relative path with nothing to pop keeps the unresolved `..` (can't
        // resolve without a filesystem or a known base).
        assert_eq!(lexical_normalize(Path::new("../a")), PathBuf::from("../a"));
    }
}
