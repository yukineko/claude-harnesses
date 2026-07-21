//! RED: the fail-open class `python3 scripts/check-fail-open.py --all` flags in
//! harness-core (`readdir-let-else-swallow` / `readdir-flatten-swallow`) —
//! `session::load_all`, `Store::list_notes`, and `usage::subagent_usage` all
//! swallow a `read_dir` failure into an empty/partial `Vec`, making "the dir is
//! unreadable" indistinguishable from "the dir is legitimately empty".
//!
//! Each test below builds a directory that HAS valid data in it, then makes it
//! unreadable (chmod 0o000) and asserts the caller can tell "could not read"
//! apart from "read, found nothing" — i.e. that the function reports
//! [`harness_core::verdict::Determination::Undetermined`], not a bare empty/
//! partial `Vec`.
//!
//! These currently fail to compile: `load_all` / `list_notes` / `subagent_usage`
//! still return `Vec<_>`, not `Determination<Vec<_>>`. That compile failure IS
//! the RED this test is meant to produce — it is a legitimate RED (the API this
//! test needs doesn't exist yet), not a test that "sees nothing". Once the
//! producing functions are migrated to return `Determination`, this file
//! compiles and exercises both the `Known` (readable, real data survives) and
//! `Undetermined` (unreadable, reported as such) arms.
//!
//! Unix-only: unreadability here is chmod-based (`0o000`), which has no
//! equivalent on non-unix filesystems the way this repo's other chmod-based
//! tests are gated (see e.g. `specguard::decision::list_files_fails_closed_on_unreadable_dir`).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use harness_core::session::{self, SessionRecord};
use harness_core::store::Store;
use harness_core::usage;
use harness_core::verdict::Determination;

/// Makes a directory unreadable/unsearchable (`0o000`) for the lifetime of the
/// guard, restoring `0o755` on drop — including on an unwinding panic (e.g. a
/// failed `assert!` inside the guarded scope), so a red assertion never leaves
/// behind a directory the enclosing `TempDir` cannot clean up.
struct UnreadableGuard {
    dir: PathBuf,
}

impl UnreadableGuard {
    /// chmod `dir` to `0o000` and return a guard that restores it on drop.
    /// Panics (via the caller's own precondition assert, not here) if the
    /// current user can still read it afterwards (e.g. running as root) —
    /// this constructor only performs the chmod.
    fn lock(dir: &Path) -> Self {
        let mut perms = std::fs::metadata(dir)
            .expect("stat dir before chmod")
            .permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(dir, perms).expect("chmod dir to 0o000");
        UnreadableGuard {
            dir: dir.to_path_buf(),
        }
    }
}

impl Drop for UnreadableGuard {
    fn drop(&mut self) {
        // Best-effort: if this fails there is nothing further we can do, and
        // panicking from a `Drop` during unwind would abort the process.
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o755));
    }
}

/// Shared precondition check: fail loudly (not silently skip) if this
/// environment can still read a directory we just chmod'd to `0o000` — e.g.
/// running as root, or a filesystem that ignores the unix permission bits.
/// Skipping here instead of panicking would let the test go green without
/// ever having exercised the swallow this file exists to catch.
fn assert_unreadable_precondition(dir: &Path) {
    assert!(
        std::fs::read_dir(dir).is_err(),
        "precondition: {dir:?} is still readable as this user (root? unusual fs?) — \
         this test cannot observe the swallow here"
    );
}

#[test]
fn session_load_all_reports_undetermined_on_unreadable_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    // Seed one valid session record via the real write path.
    let rec = SessionRecord {
        session_id: "sess-1".to_string(),
        turns: 3,
        ..Default::default()
    };
    session::upsert(state_dir, &rec);

    // Sanity: readable store reports the seeded record as a known, non-empty
    // observation — not just "no error", but the actual data survives.
    let known = session::load_all(state_dir);
    match known {
        Determination::Known(recs) => {
            assert_eq!(
                recs.len(),
                1,
                "expected exactly the seeded record: {recs:?}"
            );
            assert_eq!(recs[0].session_id, "sess-1");
        }
        Determination::Undetermined(why) => {
            panic!("readable sessions dir must be Known, got Undetermined({why:?})")
        }
    }

    let sessions_dir = session::sessions_dir(state_dir);
    let _guard = UnreadableGuard::lock(&sessions_dir);
    assert_unreadable_precondition(&sessions_dir);

    let got = session::load_all(state_dir);
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "load_all must report Undetermined when the sessions dir cannot be read; \
         an unreadable store must not be indistinguishable from an empty one: {got:?}"
    );
}

#[test]
fn store_list_notes_reports_undetermined_on_unreadable_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Store::new(tmp.path().to_path_buf());
    let cwd = Path::new("/some/project");

    store
        .write_note(cwd, "slug-one", "hello")
        .expect("write_note");

    // Sanity: readable project dir reports the seeded note.
    let known = store.list_notes(cwd);
    match known {
        Determination::Known(notes) => {
            assert_eq!(
                notes.len(),
                1,
                "expected exactly the seeded note: {notes:?}"
            );
        }
        Determination::Undetermined(why) => {
            panic!("readable notes dir must be Known, got Undetermined({why:?})")
        }
    }

    let project_dir = store.project_dir(cwd);
    let _guard = UnreadableGuard::lock(&project_dir);
    assert_unreadable_precondition(&project_dir);

    let got = store.list_notes(cwd);
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "list_notes must report Undetermined when the project dir cannot be read; \
         an unreadable store must not be indistinguishable from an empty one: {got:?}"
    );
}

#[test]
fn usage_subagent_usage_reports_undetermined_on_unreadable_subagents_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path();
    let stem = "sess";
    let sub_dir = base.join(stem).join("subagents");
    std::fs::create_dir_all(&sub_dir).expect("mkdir subagents");

    let main_path = base.join(format!("{stem}.jsonl"));
    std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n")
        .expect("write main transcript");

    // One valid sub-agent transcript line with real usage.
    std::fs::write(
        sub_dir.join("agent-aaa.jsonl"),
        concat!(
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#,
            "\n",
        ),
    )
    .expect("write subagent transcript");

    let main_path_str = main_path.to_str().expect("utf8 path");

    // Sanity: readable subagents dir reports the seeded sub-agent's usage.
    let known = usage::subagent_usage(main_path_str);
    match known {
        Determination::Known(subs) => {
            assert_eq!(
                subs.len(),
                1,
                "expected exactly the seeded sub-agent: {subs:?}"
            );
            assert_eq!(subs[0].agent_id, "aaa");
        }
        Determination::Undetermined(why) => {
            panic!("readable subagents dir must be Known, got Undetermined({why:?})")
        }
    }

    let _guard = UnreadableGuard::lock(&sub_dir);
    assert_unreadable_precondition(&sub_dir);

    let got = usage::subagent_usage(main_path_str);
    assert!(
        matches!(got, Determination::Undetermined(_)),
        "subagent_usage must report Undetermined when the subagents dir cannot be read; \
         an unreadable store must not be indistinguishable from a session with no \
         sub-agents at all: {got:?}"
    );
}
