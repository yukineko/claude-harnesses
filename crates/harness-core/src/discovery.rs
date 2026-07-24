//! Machine-scope shared "discovery record" store. Append-only JSONL, fully fail-soft.
//!
//! Concurrent compass/scout sessions on one machine use this to avoid duplicating
//! discovered tasks. Records are keyed by task fingerprint (content hash of title)
//! and annotated with the discovering session and current status (Discovered or Selected).
//!
//! All operations are fail-soft: on any IO/lock/parse error, operations degrade
//! silently and never panic. Missing/corrupt files yield sensible defaults. This is
//! load-bearing — discovery may be called from hooks.

use std::path::{Path, PathBuf};

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

/// Resolve the canonical git repo root for a cwd so that distinct worktrees /
/// subdirectories of the SAME repo key to one discovery store (the whole point
/// of the machine-scoped dedup — two git worktrees of one repo have different
/// cwds but must share one record file).
///
/// Strategy: ask git for the toplevel (`git -C <cwd> rev-parse
/// --show-toplevel`), trim it, and canonicalize. Fully fail-soft: if git is
/// missing, the dir isn't a repo, or any IO/parse error occurs, fall back to
/// `cwd.canonicalize()`, and finally to a purely lexical normalization of the
/// raw cwd (see [`lexical_normalize`]) for paths that don't exist locally at
/// all. NEVER panics.
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
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8(output.stdout).ok()?;
    let top = top.trim();
    if top.is_empty() {
        return None;
    }
    Some(PathBuf::from(top))
}

/// Path to the discovery.jsonl store for a given cwd.
/// Returns `~/.compass/<project_key>/discovery.jsonl`, keyed by the canonical
/// git repo root (see [`resolve_repo_root`]) so sibling worktrees / subdirs of
/// one repo share a single store. All other store fns route through this, so
/// they inherit the canonical-root keying.
pub fn record_path(cwd: &Path) -> PathBuf {
    crate::config::base_dir("compass")
        .join(crate::store::project_key(&resolve_repo_root(cwd)))
        .join("discovery.jsonl")
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

/// Append a discovery record to the store. Fails soft: on any IO or serialization
/// error, the record is silently dropped and discovery continues.
pub fn append(cwd: &Path, rec: &DiscoveryRecord) {
    append_at(&record_path(cwd), rec);
}

/// Internal: append to an explicit path. Used by append() and by tests.
///
/// Holds the same advisory lock as [`mark_selected_at`] across the write:
/// `append_line`'s single `O_APPEND` write is atomic against other appends on
/// its own, but a concurrent `mark_selected_at` reads a snapshot of the whole
/// file and later rewrites it wholesale — without a shared lock, a rewrite
/// that started before this append lands would silently clobber it on rename
/// (lost update, not just a torn write).
fn append_at(path: &Path, rec: &DiscoveryRecord) {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Serialize the record and append it as one atomic JSON line.
    let Ok(json) = serde_json::to_string(rec) else {
        return;
    };

    let Some(_guard) = crate::lessons::acquire_lock(path) else {
        return;
    };
    // Single atomic append (body + '\n' in one write) — see issue #15.
    crate::append::append_line(path, &json);
}

/// Load all discovery records from the store, returning only those that parsed
/// successfully. Missing file, empty file, or blank lines are silently skipped.
/// Corrupt JSON lines are also skipped. Never panics.
pub fn load(cwd: &Path) -> Vec<DiscoveryRecord> {
    load_at(&record_path(cwd))
}

/// Internal: load from an explicit path. Used by load() and by tests.
fn load_at(path: &Path) -> Vec<DiscoveryRecord> {
    let mut records = Vec::new();

    let Ok(contents) = std::fs::read_to_string(path) else {
        return records;
    };

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Skip corrupt/partial lines silently.
        if let Ok(rec) = serde_json::from_str::<DiscoveryRecord>(line) {
            records.push(rec);
        }
    }

    records
}

/// Whether a *different* session owns `fingerprint` — i.e. should this session
/// suppress it from its surface (cross-session dedup).
///
/// Ownership is by *earliest discovery*: the owner is the session with the
/// earliest row for the fingerprint (`created_at`, then `session_id` for a
/// deterministic tie-break), **regardless of status**. A `Selected` row still
/// counts, so a task another session is actively working keeps suppressing this
/// one. Returns true iff that owner is a different session than `my_session`.
///
/// This realises the DoD's "the *later* discovery does not re-surface the
/// duplicate" rule: the first discoverer keeps the task and every later session
/// drops it (stable single owner — no mutual annihilation where both sessions
/// see each other's row and both drop it). An absent/empty store has no owner,
/// so nothing is suppressed (byte-equivalent fallback).
pub fn already_discovered_by_other(cwd: &Path, fingerprint: &str, my_session: &str) -> bool {
    owned_by_other(&load(cwd), fingerprint, my_session)
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

/// Mark a task as selected, rewriting the store atomically. All records matching
/// the fingerprint have their status set to Selected; all others remain unchanged.
/// Fails soft: on any error, leaves the file as-is and returns silently.
pub fn mark_selected(cwd: &Path, fingerprint: &str) {
    mark_selected_at(&record_path(cwd), fingerprint);
}

/// Internal: mark selected on an explicit path. Used by mark_selected() and by tests.
///
/// Holds the same advisory lock as [`append_at`] across the whole
/// load->mutate->rewrite cycle: without it, a concurrent `append_at` (or
/// another `mark_selected_at`) landing between this fn's load and its rename
/// would be silently discarded by the wholesale rewrite (lost update).
fn mark_selected_at(path: &Path, fingerprint: &str) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    let Some(_guard) = crate::lessons::acquire_lock(path) else {
        return;
    };

    // Load all records.
    let mut records = load_at(path);
    artificial_race_delay();

    // Update matching records to Selected.
    for r in &mut records {
        if r.fingerprint == fingerprint {
            r.status = Status::Selected;
        }
    }

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Atomic write: write to a temp file, then rename.
    // Mirrors the style of store::save_json's atomic write pattern.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("jsonl.tmp.{}.{}", std::process::id(), seq));

    // Write all records to the temp file.
    let Ok(mut tmp_file) = std::fs::File::create(&tmp_path) else {
        return;
    };

    let mut wrote_ok = true;
    for rec in records {
        let Ok(json) = serde_json::to_string(&rec) else {
            wrote_ok = false;
            break;
        };
        if writeln!(tmp_file, "{}", json).is_err() {
            wrote_ok = false;
            break;
        }
    }

    // If write failed, clean up the temp file and return.
    if !wrote_ok {
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    // Drop the file handle so the rename can succeed on Windows.
    drop(tmp_file);

    // Atomically rename the temp file to the target.
    let _ = std::fs::rename(&tmp_path, path);
}

#[cfg(test)]
mod tests {
    use super::*;

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

        append_at(&path, &rec_a);
        append_at(&path, &rec_b);
        append_at(&path, &rec_c);

        // Mark C as selected.
        mark_selected_at(&path, "ccc");

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

        // mark_selected_at on a missing path should not panic.
        mark_selected_at(Path::new("/nonexistent/discovery.jsonl"), "some-fp");
        // If we reach here, it didn't panic.
    }

    #[test]
    fn fail_soft_corrupt_lines() {
        // Create a temp file with a mix of valid and corrupt JSON lines.
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

        // load_at should skip the corrupt line and return 2 records.
        let records = load_at(&path);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].fingerprint, "valid");
        assert_eq!(records[1].fingerprint, "valid");
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
        append_at(
            &path,
            &DiscoveryRecord {
                fingerprint: "fp".to_string(),
                session_id: "owner".to_string(),
                status: Status::Discovered,
                created_at: 5,
                title: "T".to_string(),
            },
        );
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
        // Two distinct cwds under the SAME git repo root (the repo root itself
        // and a nested subdir) must map to the SAME record_path — that's the
        // canonical-root keying that lets sibling worktrees/subdirs dedup.
        let tempdir = tempfile::tempdir().expect("tempfile::tempdir");
        let root = tempdir.path();

        let status = std::process::Command::new("git")
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
            record_path(root),
            record_path(&sub),
            "repo root and a subdir of the same repo must share one record_path"
        );
        assert_eq!(resolve_repo_root(root), resolve_repo_root(&sub));
    }

    #[test]
    fn record_path_includes_project_key() {
        let cwd = Path::new("/some/project/dir");
        let path = record_path(cwd);
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
