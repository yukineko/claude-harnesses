//! End-to-end tests: drive the real built `backlog` binary and assert on exit
//! code + stdout. `backlog` is a subcommand CLI; `session-start` is a hook whose
//! invariant is to always exit 0 (never break a turn).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME so `~/.backlog` (the task store) is fresh and the
/// real queue is never read or written.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-it-{}-{}-{}",
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

/// An isolated temp dir that IS a repo root, for pinning a child's cwd.
///
/// A bare temp dir no longer resolves to any store: `config::locate` returns
/// `StoreLocation::NoProject` for a cwd with no repo above it and the CLI
/// refuses instead of falling back to the cross-project `~/.backlog`. These
/// tests pinned a bare temp dir precisely to stay out of the real repo's
/// tracked store, so they now need a repo of their own rather than the absence
/// of one. A `.git` DIRECTORY is enough for both the ancestor scan that picks
/// the store and the identity scan that reads it as a main working tree, so no
/// `git` binary is needed.
fn temp_repo(tag: &str) -> PathBuf {
    let dir = temp_home(tag);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

/// The canonical project label for a `temp_repo` — what `add --project` must
/// name, since a repo store refuses a `--project` it cannot be scoped to.
fn project_of(repo: &std::path::Path) -> String {
    repo.canonicalize().unwrap().to_string_lossy().into_owned()
}

/// Run `backlog <args>` with `payload` on stdin under an isolated HOME.
fn run(args: &[&str], payload: &str, home: &PathBuf) -> (i32, String) {
    run_in(args, payload, home, None)
}

/// Same as `run`, but lets the caller pin the child process's cwd — needed to
/// exercise cwd-derived project resolution (`list` with no `--project`/`--all`)
/// deterministically regardless of where `cargo test` itself is invoked from.
fn run_in(args: &[&str], payload: &str, home: &PathBuf, cwd: Option<&PathBuf>) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_backlog");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_lists_the_about_line() {
    let home = temp_home("help");
    let (code, stdout) = run(&["--help"], "", &home);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Cross-project task queue"),
        "expected the about line, got: {stdout}"
    );
}

#[test]
fn list_on_empty_store_says_no_tasks() {
    // Read-only subcommand against a fresh, isolated store. The store's
    // location is resolved from the CHILD PROCESS's cwd (not `--project`,
    // not the cwd `cargo test` happens to be invoked from), so this must
    // pin an explicit cwd — otherwise the child inherits this test binary's
    // own cwd, which sits inside the harness repo checkout and resolves to
    // that repo's real, non-empty, tracked `.backlog/tasks.toml`. The pinned
    // cwd is a repo (`temp_repo`) because a cwd with no repo above it now has
    // no store at all and `list` refuses there rather than reporting empty —
    // which is a different assertion, covered in `tests/project_scope.rs`.
    let home = temp_home("list");
    let cwd = temp_repo("list-cwd");
    let (code, stdout) = run_in(&["list"], "", &home, Some(&cwd));
    assert_eq!(code, 0, "list on an empty store must succeed");
    assert!(
        stdout.contains("no tasks"),
        "empty store should report 'no tasks', got: {stdout}"
    );
}

#[test]
fn list_json_emits_machine_readable_array() {
    // The contract autoflow depends on: `list --json` prints a JSON array whose
    // tasks carry `title` and `status`. Add one task, then read it back as JSON.
    // The store's location is resolved from the child process's own cwd, so
    // `add` and `list` must share the SAME pinned, isolated, non-repo cwd —
    // otherwise both silently resolve to this test binary's inherited cwd
    // (the real harness repo checkout), landing the write in the tracked
    // `.backlog/tasks.toml` and accumulating across runs, which breaks the
    // exact-one-task assertion below.
    let home = temp_home("list-json");
    let cwd = temp_repo("list-json-cwd");
    // The store is this repo's, so the project IS this repo: a `--project`
    // naming anything else (the old `/p`) is now refused, on the write side
    // as well as the read side.
    let project = project_of(&cwd);
    let (add_code, _) = run_in(
        &[
            "add",
            "--title",
            "JSON task",
            "--project",
            &project,
            "--priority",
            "p1",
        ],
        "",
        &home,
        Some(&cwd),
    );
    assert_eq!(add_code, 0, "add must succeed");

    let (code, stdout) = run_in(
        &[
            "list",
            "--project",
            &project,
            "--status",
            "pending",
            "--json",
        ],
        "",
        &home,
        Some(&cwd),
    );
    assert_eq!(code, 0, "list --json must succeed");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("list --json must emit valid JSON ({e}): {stdout}"));
    let arr = v.as_array().expect("top-level must be an array");
    assert_eq!(arr.len(), 1, "exactly the one pending task");
    assert_eq!(arr[0]["title"], "JSON task");
    assert_eq!(arr[0]["status"], "pending");
}

/// CONTRACT INVERSION (2026-08-20). This test used to assert that a task
/// written into repo A's store under project `/other/project` was accepted and
/// then filtered out of A's listing. Both halves were the defect:
///
///   * accepting the write put a row in a tracked, repo-local file that does
///     not belong to that repo — a store whose contents no longer answer the
///     question the file's location asks;
///   * filtering the read is what hid 258 of this repo's own pending tasks
///     from the other checkout of the SAME repo (measured 2026-08-20), because
///     the filter compared absolute checkout paths, not project identity.
///
/// The read is now unfiltered (`tests/project_scope.rs`) and the write is
/// refused, which is what this asserts. The refusal has to be a non-zero exit
/// with a reason, not a silently dropped row: "this store cannot hold that"
/// and "stored fine" must not render the same (CLAUDE.md §3).
#[test]
fn add_naming_a_foreign_project_is_refused_by_a_repo_store() {
    let home = temp_home("list-scope-cwd");
    let cwd_a = temp_repo("list-scope-cwd-repo-a");
    let project_a = project_of(&cwd_a);

    let (add_a, _) = run_in(
        &["add", "--title", "Task A", "--project", &project_a],
        "",
        &home,
        Some(&cwd_a),
    );
    assert_eq!(add_a, 0, "add of THIS repo's own project must succeed");

    let (add_b, out_b) = run_in(
        &["add", "--title", "Task B", "--project", "/other/project"],
        "",
        &home,
        Some(&cwd_a),
    );
    assert_ne!(
        add_b, 0,
        "a repo store must refuse a task belonging to another project, got exit 0: {out_b}"
    );
    assert!(
        out_b.trim().is_empty(),
        "a refused add must print no `added:` line on stdout, got: {out_b}"
    );

    // And the refusal is a refusal, not a delete: A's own task is still there,
    // and B never landed.
    let (code, stdout) = run_in(&["list"], "", &home, Some(&cwd_a));
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Task A"),
        "the store's own task must survive the refused write, got: {stdout}"
    );
    assert!(
        !stdout.contains("Task B"),
        "the refused task must not have been written, got: {stdout}"
    );
}

#[test]
fn list_all_flag_returns_every_project_in_the_resolved_store() {
    // `--all` is scoped to ONE store. The store is resolved per repo
    // (`<root>/.backlog/tasks.toml`), so "every project" means every project
    // key recorded in THAT file — NOT a cross-repo search, which has no index
    // to walk.
    //
    // A repo-resolved store cannot exercise this: every project path inside
    // one repo collapses to that repo's root, so such a store holds exactly
    // one key and `--all` would be indistinguishable from the default (that is
    // the design consequence, not a gap in the test). The stores that DO hold
    // several keys are the ones reached without repo resolution — a pinned
    // `store_dir` and the legacy fallback. A pinned store is used here because
    // it fixes the path for every subcommand regardless of cwd, so the test
    // cannot pass or fail for reasons of where `cargo test` was invoked.
    let home = temp_home("list-all-flag");
    let pinned = temp_home("list-all-flag-store");
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!("store_dir = {:?}\n", pinned.to_string_lossy()),
    )
    .unwrap();

    let (add_a, _) = run(
        &["add", "--title", "Task A", "--project", "/proj/a"],
        "",
        &home,
    );
    assert_eq!(add_a, 0, "add A must succeed");
    let (add_b, _) = run(
        &["add", "--title", "Task B", "--project", "/proj/b"],
        "",
        &home,
    );
    assert_eq!(add_b, 0, "add B must succeed");
    assert!(
        pinned.join("tasks.toml").exists(),
        "both adds must have landed in the pinned store, else the rest of this \
         test would be asserting against the wrong file"
    );

    let (code, stdout) = run(&["list", "--all"], "", &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Task A") && stdout.contains("Task B"),
        "--all must return every project in the resolved store, got: {stdout}"
    );

    // Anti-vacuity: the same store, same cwd, WITHOUT `--all` must not return
    // them. Neither `/proj/a` nor `/proj/b` is the cwd's project, so a `--all`
    // that was ignored entirely would leave this list empty — and the
    // assertion above would then have to fail.
    let (code, scoped) = run(&["list"], "", &home);
    assert_eq!(code, 0);
    assert!(
        !scoped.contains("Task A") && !scoped.contains("Task B"),
        "default scope must stay project-scoped, got: {scoped}"
    );
}

#[test]
fn list_project_flag_wins_over_all_when_both_given() {
    // `--project` and `--all` together: --project must win (not an error).
    //
    // This precedence only exists where a project FILTER still exists, i.e. a
    // store that can hold several projects. A repo store cannot: it is the
    // scope, `--project` there is an assertion about which store you meant,
    // and `--all` is the default (see `main::read_project_scope` and
    // `tests/project_scope.rs`). So this pins the store explicitly, the same
    // way `list_all_flag_returns_every_project_in_the_resolved_store` does —
    // which also fixes the path for every subcommand regardless of cwd.
    let home = temp_home("list-project-wins");
    let pinned = temp_home("list-project-wins-store");
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!("store_dir = {:?}\n", pinned.to_string_lossy()),
    )
    .unwrap();

    let (add_a, _) = run(
        &["add", "--title", "Task A", "--project", "/proj/a"],
        "",
        &home,
    );
    assert_eq!(add_a, 0, "add A must succeed");
    let (add_b, _) = run(
        &["add", "--title", "Task B", "--project", "/proj/b"],
        "",
        &home,
    );
    assert_eq!(add_b, 0, "add B must succeed");
    assert!(
        pinned.join("tasks.toml").exists(),
        "both adds must have landed in the pinned store, else this test would \
         be asserting against the wrong file"
    );

    let (code, stdout) = run(&["list", "--project", "/proj/a", "--all"], "", &home);
    assert_eq!(code, 0, "--project + --all must not error");
    assert!(
        stdout.contains("Task A"),
        "--project must still scope to its project, got: {stdout}"
    );
    assert!(
        !stdout.contains("Task B"),
        "--project must win over --all (not a union), got: {stdout}"
    );
}

#[test]
fn lock_status_reports_none_when_unheld() {
    let home = temp_home("lock");
    let (code, stdout) = run(&["lock", "status"], "", &home);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("none"),
        "no lock held → 'none', got: {stdout}"
    );
}

#[test]
fn session_start_with_valid_payload_exits_zero() {
    // SessionStart hook with a well-formed payload; fresh store → no pending
    // tasks → no additionalContext, but it must exit 0 cleanly.
    let home = temp_home("ss-valid");
    let payload = r#"{"hook_event_name":"SessionStart","session_id":"it-1","cwd":"/tmp"}"#;
    let (code, _stdout) = run(&["session-start"], payload, &home);
    assert_eq!(code, 0, "SessionStart hook must always exit 0");
}

#[test]
fn session_start_with_malformed_stdin_exits_zero() {
    // Fail-soft invariant: malformed stdin is skipped (logged to stderr), hook
    // still exits 0 and never breaks the turn.
    let home = temp_home("ss-bad");
    let (code, stdout) = run(&["session-start"], "not json", &home);
    assert_eq!(code, 0, "malformed stdin must still exit 0");
    assert!(stdout.trim().is_empty(), "got: {stdout}");
}
