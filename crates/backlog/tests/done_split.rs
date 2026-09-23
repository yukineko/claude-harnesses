//! Terminal rows (`done` / `cancelled`) live in a sibling done file.
//!
//! Spec (backlog 45c3a699, condukt task `backlog-done-split`): the store at
//! `<root>/.backlog/tasks.toml` keeps only non-terminal rows; terminal rows
//! are moved to `<same dir>/<stem>.done.toml` (here `tasks.done.toml`), which
//! is append-only. Readers see the UNION of both files, terminal status is
//! monotonic (a reverting merge cannot resurrect a done task), and a done file
//! that exists but cannot be parsed is UNDETERMINED — a non-zero exit, never a
//! listing that silently lacks the done rows (CLAUDE.md §3).
//!
//! These tests drive the real built binary against an isolated HOME and an
//! isolated repo (a `.git` DIRECTORY is enough for `config::locate` to pick
//! `<repo>/.backlog/tasks.toml`, see `tests/integration.rs::temp_repo`), and
//! assert only on the CLI surface and the on-disk files, so they compile
//! against the pre-split code and fail at runtime.
//!
//! Written by an independent test writer (CLAUDE.md §2(a)) before the
//! implementation existed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-donesplit-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An isolated HOME plus an isolated repo root whose store is
/// `<repo>/.backlog/tasks.toml`.
struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let home = unique_dir(&format!("{tag}-home"));
        let repo = unique_dir(&format!("{tag}-repo"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // Canonicalize so the project label matches what the binary resolves
        // (macOS temp dirs sit behind the /var -> /private/var symlink).
        let repo = repo.canonicalize().unwrap();
        Fixture { home, repo }
    }

    fn project(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    fn store_dir(&self) -> PathBuf {
        self.repo.join(".backlog")
    }

    fn tasks_path(&self) -> PathBuf {
        self.store_dir().join("tasks.toml")
    }

    fn done_path(&self) -> PathBuf {
        self.store_dir().join("tasks.done.toml")
    }

    fn run(&self, args: &[&str]) -> Out {
        let bin = env!("CARGO_BIN_EXE_backlog");
        let mut child = Command::new(bin)
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"");
        }
        let out = child.wait_with_output().expect("binary runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// `backlog add` and return the new id (parsed from `added: <id>`).
    fn add(&self, title: &str) -> String {
        let project = self.project();
        let out = self.run(&["add", "--title", title, "--project", &project]);
        assert_eq!(
            out.code, 0,
            "add {title:?} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
        out.stdout
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .unwrap_or_else(|| panic!("no `added: <id>` line in {:?}", out.stdout))
            .trim()
            .to_string()
    }

    fn done(&self, id: &str) {
        let out = self.run(&["done", id]);
        assert_eq!(
            out.code, 0,
            "done {id} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
    }

    /// `list --all --json` (optionally with `--status`), parsed.
    fn list_json(&self, status: Option<&str>) -> Vec<serde_json::Value> {
        let mut args = vec!["list", "--all", "--json"];
        if let Some(s) = status {
            args.push("--status");
            args.push(s);
        }
        let out = self.run(&args);
        assert_eq!(
            out.code, 0,
            "list {args:?} must succeed; stdout={} stderr={}",
            out.stdout, out.stderr
        );
        serde_json::from_str::<Vec<serde_json::Value>>(out.stdout.trim())
            .unwrap_or_else(|e| panic!("list --json is not a JSON array ({e}): {:?}", out.stdout))
    }

    /// Write a raw `[[task]]` document into the store dir.
    fn write_store_file(&self, name: &str, body: &str) {
        std::fs::create_dir_all(self.store_dir()).unwrap();
        std::fs::write(self.store_dir().join(name), body).unwrap();
    }
}

/// One `[[task]]` row in EXACTLY the shape the pre-split binary writes
/// (copied from a real `backlog add` + `backlog done` run in a temp repo).
fn row(id: &str, title: &str, project: &str, status: &str) -> String {
    format!(
        "[[task]]\n\
         id = \"{id}\"\n\
         title = \"{title}\"\n\
         project = \"{project}\"\n\
         project_unresolved = false\n\
         tags = []\n\
         touched_files = []\n\
         status = \"{status}\"\n\
         notes = \"\"\n\
         created_at = 1790184889\n\
         updated_at = 1790184890\n\
         weight = 0.0\n"
    )
}

/// Parse a store file into `(id, status)` pairs, in file order. A missing
/// file yields an empty list; an unparseable one panics (a test-side fault).
fn rows_in(path: &Path) -> Vec<(String, String)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read {}: {e}", path.display()),
    };
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    doc.get("task")
        .and_then(|t| t.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    (
                        r["id"].as_str().unwrap().to_string(),
                        r["status"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn has(rows: &[(String, String)], id: &str) -> bool {
    rows.iter().any(|(i, _)| i == id)
}

fn status_of<'a>(rows: &'a [(String, String)], id: &str) -> Option<&'a str> {
    rows.iter().find(|(i, _)| i == id).map(|(_, s)| s.as_str())
}

// ---------------------------------------------------------------------------
// 1. `done` moves the row to the done file.
// ---------------------------------------------------------------------------

#[test]
fn done_moves_row_into_done_file_and_out_of_tasks_file() {
    let fx = Fixture::new("move");
    let a = fx.add("Alpha split task");
    let b = fx.add("Beta split task");
    fx.done(&a);

    let done_rows = rows_in(&fx.done_path());
    let live_rows = rows_in(&fx.tasks_path());
    assert_eq!(
        status_of(&done_rows, &a),
        Some("done"),
        "the completed row must be written to {} with status done; done file rows: {done_rows:?}",
        fx.done_path().display()
    );
    assert!(
        !has(&live_rows, &a),
        "the completed row must be ABSENT from tasks.toml; tasks.toml rows: {live_rows:?}"
    );
    assert_eq!(
        status_of(&live_rows, &b),
        Some("pending"),
        "the pending row must stay in tasks.toml; tasks.toml rows: {live_rows:?}"
    );
    assert!(
        !has(&done_rows, &b),
        "a pending row must not appear in the done file; done file rows: {done_rows:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Readers see the union of both files.
// ---------------------------------------------------------------------------

/// Hand-written fixture: the done row exists ONLY in `tasks.done.toml`. This
/// isolates the reader contract from the writer.
#[test]
fn readers_see_rows_that_live_only_in_the_done_file() {
    let fx = Fixture::new("union");
    let p = fx.project();
    fx.write_store_file(
        "tasks.toml",
        &row("aaaa0001", "Still pending", &p, "pending"),
    );
    fx.write_store_file(
        "tasks.done.toml",
        &row("dddd0001", "Finished elsewhere", &p, "done"),
    );

    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == "dddd0001" && t["status"] == "done"),
        "`list --status done --json` must include the row stored only in tasks.done.toml; got {done:?}"
    );

    let all = fx.list_json(None);
    let ids: Vec<&str> = all.iter().filter_map(|t| t["id"].as_str()).collect();
    assert!(
        ids.contains(&"aaaa0001") && ids.contains(&"dddd0001"),
        "`list --all --json` must be the union of both files; got ids {ids:?}"
    );

    // id resolution reaches the done file: re-completing is idempotent success.
    let again = fx.run(&["done", "dddd0001"]);
    assert_eq!(
        again.code, 0,
        "`done` on an id that lives in the done file must resolve and succeed idempotently; stdout={} stderr={}",
        again.stdout, again.stderr
    );

    // …and a non-status edit of it resolves too, and sticks.
    let edit = fx.run(&["edit", "dddd0001", "--notes", "post-hoc note"]);
    assert_eq!(
        edit.code, 0,
        "`edit` must resolve an id that lives in the done file; stdout={} stderr={}",
        edit.stdout, edit.stderr
    );
    let done = fx.list_json(Some("done"));
    let t = done
        .iter()
        .find(|t| t["id"] == "dddd0001")
        .unwrap_or_else(|| panic!("edited done task vanished from `list --status done`: {done:?}"));
    assert_eq!(
        t["notes"], "post-hoc note",
        "edit of a done-file row must persist"
    );
    assert!(
        !has(&rows_in(&fx.tasks_path()), "dddd0001"),
        "editing a terminal row must not move it back into tasks.toml"
    );
}

/// PRE-EXISTING contract pin, expected to pass on the old code as well.
///
/// The spec text for this task says the duplicate guard "still rejects" an add
/// matching a task that lives only in the done file. That contradicts the
/// current documented contract (`store::check_duplicate`: "a `done` task with
/// the same title does NOT block a re-add") and the observed behaviour. This
/// test pins the CURRENT contract — the split must not change what the guard
/// decides merely because the row moved files. If the intended contract really
/// is "done blocks re-add", this test is the one to change, deliberately.
#[test]
fn duplicate_guard_decision_is_unchanged_for_a_row_in_the_done_file() {
    let fx = Fixture::new("dedup");
    let p = fx.project();
    fx.write_store_file("tasks.toml", "");
    fx.write_store_file(
        "tasks.done.toml",
        &row("dddd0002", "Already shipped", &p, "done"),
    );
    let out = fx.run(&["add", "--title", "Already shipped", "--project", &p]);
    assert_eq!(
        out.code, 0,
        "a done task (wherever it is stored) does not block a re-add under the current \
         contract; stdout={} stderr={}",
        out.stdout, out.stderr
    );
}

// ---------------------------------------------------------------------------
// 3. Legacy done rows in tasks.toml are read, then migrated on the next write.
// ---------------------------------------------------------------------------

#[test]
fn legacy_done_row_in_tasks_file_is_listed_and_migrated_on_next_write() {
    let fx = Fixture::new("legacy");
    let p = fx.project();
    fx.write_store_file(
        "tasks.toml",
        &format!(
            "{}\n{}",
            row("1e9a0001", "Legacy finished", &p, "done"),
            row("1e9a0002", "Legacy pending", &p, "pending")
        ),
    );

    // Read side (pre-existing behaviour, must be kept): still listed as done.
    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == "1e9a0001"),
        "a legacy done row in tasks.toml must still be listed as done; got {done:?}"
    );

    // Any locked write migrates it.
    fx.add("Unrelated new task");

    let live = rows_in(&fx.tasks_path());
    let done_rows = rows_in(&fx.done_path());
    assert!(
        !has(&live, "1e9a0001"),
        "after a locked write the legacy done row must be MIGRATED out of tasks.toml; tasks.toml rows: {live:?}"
    );
    assert_eq!(
        status_of(&done_rows, "1e9a0001"),
        Some("done"),
        "after a locked write the legacy done row must be in the done file; done file rows: {done_rows:?}"
    );
    assert_eq!(
        status_of(&live, "1e9a0002"),
        Some("pending"),
        "the legacy pending row must stay in tasks.toml; tasks.toml rows: {live:?}"
    );
    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == "1e9a0001"),
        "the migrated row must still be listed as done; got {done:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Terminal status is monotonic.
// ---------------------------------------------------------------------------

/// A reverting merge re-introduced the row as `pending` in tasks.toml while
/// the done file says `done`. Done wins: listed once, as done, and never
/// handed out by `next`.
#[test]
fn done_in_done_file_beats_pending_in_tasks_file() {
    let fx = Fixture::new("monotonic");
    let p = fx.project();
    fx.write_store_file(
        "tasks.toml",
        &row("ee000001", "Reverted by a merge", &p, "pending"),
    );
    fx.write_store_file(
        "tasks.done.toml",
        &row("ee000001", "Reverted by a merge", &p, "done"),
    );

    let all = fx.list_json(None);
    let matches: Vec<_> = all.iter().filter(|t| t["id"] == "ee000001").collect();
    assert_eq!(
        matches.len(),
        1,
        "the id must be listed exactly once (union, not concatenation); got {all:?}"
    );
    assert_eq!(
        matches[0]["status"], "done",
        "done in the done file must win over pending in tasks.toml; got {:?}",
        matches[0]
    );

    let next = fx.run(&["next", "--all"]);
    assert_eq!(
        next.code, 0,
        "next must succeed; stdout={} stderr={}",
        next.stdout, next.stderr
    );
    assert!(
        !next.stdout.contains("ee000001"),
        "`next` must never hand out a task that is done in the done file; got {}",
        next.stdout
    );
}

#[test]
fn edit_cannot_revert_a_done_task_to_pending() {
    let fx = Fixture::new("norevert");
    let a = fx.add("Must stay done");
    fx.done(&a);

    let out = fx.run(&["edit", &a, "--status", "pending"]);
    assert_ne!(
        out.code, 0,
        "`edit --status pending` on a done task must be REFUSED; stdout={} stderr={}",
        out.stdout, out.stderr
    );
    let done = fx.list_json(Some("done"));
    assert!(
        done.iter().any(|t| t["id"] == a.as_str()),
        "the task must still be done after the refused revert; got {done:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. An unparseable done file is undetermined; a missing one is fine.
// ---------------------------------------------------------------------------

#[test]
fn unparseable_done_file_makes_list_and_next_fail_closed() {
    let fx = Fixture::new("garbage");
    let p = fx.project();
    fx.write_store_file("tasks.toml", &row("ff000001", "Pending one", &p, "pending"));
    fx.write_store_file("tasks.done.toml", "[[task]\nthis is = = not toml ]]\n");

    for args in [
        vec!["list", "--all"],
        vec!["list", "--all", "--json"],
        vec!["list", "--all", "--status", "done", "--json"],
        vec!["next", "--all"],
    ] {
        let out = fx.run(&args);
        assert_ne!(
            out.code,
            0,
            "`{}` must exit NON-ZERO when tasks.done.toml exists but is unparseable \
             (never a listing that silently lacks the done rows); stdout={} stderr={}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        assert!(
            out.stderr.contains("tasks.done.toml"),
            "`{}` must name the unreadable done file on stderr; stderr={}",
            args.join(" "),
            out.stderr
        );
    }
}

/// PRE-EXISTING behaviour pin (passes on the old code): no done file at all is
/// the normal state of a store that has never completed anything.
#[test]
fn missing_done_file_is_fine() {
    let fx = Fixture::new("nodone");
    let p = fx.project();
    fx.write_store_file(
        "tasks.toml",
        &row("ab000001", "Only pending", &p, "pending"),
    );
    assert!(!fx.done_path().exists());
    let all = fx.list_json(None);
    assert!(all.iter().any(|t| t["id"] == "ab000001"), "got {all:?}");
    let next = fx.run(&["next", "--all"]);
    assert_eq!(next.code, 0, "stderr={}", next.stderr);
    assert!(next.stdout.contains("ab000001"), "got {}", next.stdout);
}

// ---------------------------------------------------------------------------
// 6. The done file is append-only, in completion order.
// ---------------------------------------------------------------------------

#[test]
fn done_file_is_appended_in_completion_order() {
    let fx = Fixture::new("append");
    let a = fx.add("First to finish");
    let b = fx.add("Second to finish");
    let _c = fx.add("Never finished");

    fx.done(&a);
    let before = std::fs::read(fx.done_path()).unwrap_or_else(|e| {
        panic!(
            "after the first `done`, {} must exist: {e}",
            fx.done_path().display()
        )
    });
    fx.done(&b);
    let after = std::fs::read(fx.done_path()).unwrap();

    assert!(
        after.starts_with(&before),
        "the second completion must be a pure APPEND to the done file (old bytes a prefix of \
         the new bytes)\n--- before ---\n{}\n--- after ---\n{}",
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after)
    );
    let ids: Vec<String> = rows_in(&fx.done_path())
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    let pa = ids.iter().position(|i| *i == a);
    let pb = ids.iter().position(|i| *i == b);
    assert!(
        matches!((pa, pb), (Some(x), Some(y)) if x < y),
        "A's row must precede B's in the done file; done file ids: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// 7. `cancelled` is terminal too.
// ---------------------------------------------------------------------------

#[test]
fn cancelled_row_is_migrated_to_done_file() {
    let fx = Fixture::new("cancelled");
    let p = fx.project();
    fx.write_store_file(
        "tasks.toml",
        &format!(
            "{}\n{}",
            row("cc000001", "Abandoned", &p, "cancelled"),
            row("cc000002", "Still wanted", &p, "pending")
        ),
    );

    fx.add("Trigger a locked write");

    let live = rows_in(&fx.tasks_path());
    let done_rows = rows_in(&fx.done_path());
    assert!(
        !has(&live, "cc000001"),
        "a cancelled row is terminal and must leave tasks.toml on the next write; tasks.toml rows: {live:?}"
    );
    assert_eq!(
        status_of(&done_rows, "cc000001"),
        Some("cancelled"),
        "the cancelled row must land in the done file with its status kept; done file rows: {done_rows:?}"
    );
    assert!(
        has(&live, "cc000002"),
        "pending row must stay; tasks.toml rows: {live:?}"
    );
}
