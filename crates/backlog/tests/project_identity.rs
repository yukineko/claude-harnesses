//! Tests for project identity and worktree isolation (CLAUDE.md §8).
//!
//! These tests verify that:
//! (a) tasks written from a worktree with --project <main> land in the
//!     worktree's own .backlog (cwd-scoped store), not main's.
//! (b) a task stored with main's project path is visible from the
//!     worktree's cwd-scoped `backlog list` (no --project, no --all).
//! (c) an existing task recorded under a project's absolute path remains
//!     visible from BOTH the main tree's cwd and the worktree's cwd (the
//!     backward-compat invariant — this is NOT the same as (a)/(b)'s write
//!     path, and must not be proven via `backlog add`, which would bake
//!     in whatever `add`'s current resolution happens to do).
//! (d) when project-scope resolution cannot actually be determined (a
//!     broken `.git` link), a real, already-persisted task must not
//!     silently vanish into an empty "no tasks" result indistinguishable
//!     from a genuinely empty queue.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp HOME so `~/.backlog` is fresh and independent
/// per test run. Mixes pid + nanos (not just pid) so two tests started in
/// the same process within the same clock tick still get distinct dirs.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-project-id-{}-{}-{}",
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

/// A fresh, uniquely-named scratch directory under the system temp root.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-project-id-dir-{}-{}-{}",
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

/// Run `backlog <args>` with optional stdin payload under isolated HOME and
/// explicit cwd. Essential for testing cwd-scoped resolution.
fn run_in(args: &[&str], payload: &str, home: &PathBuf, cwd: &PathBuf) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_backlog");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Is `git` usable at all in this environment? Tests that depend on real git
/// repos/worktrees must not silently report green when git is unavailable —
/// they fail loudly instead (see call sites below), but this lets us produce
/// a clear diagnostic rather than an opaque panic from deep inside setup.
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a minimal git repository with a worktree attachment, returning
/// (main_tree_root, worktree_root). Panics loudly (not a silent skip) if git
/// itself is unavailable or any setup step fails, so a broken environment
/// shows up as a hard test failure, not a quietly-green no-op.
fn setup_repo_with_worktree(tag: &str) -> (PathBuf, PathBuf) {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let main_repo = temp_dir(&format!("repo-main-{tag}"));

    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&main_repo)
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );

    Command::new("git")
        .args(["config", "user.email", "test@test.local"])
        .current_dir(&main_repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&main_repo)
        .status()
        .unwrap();

    assert!(
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial", "-q"])
            .current_dir(&main_repo)
            .status()
            .unwrap()
            .success(),
        "git commit failed"
    );

    let worktree_root = temp_dir(&format!("repo-wt-{tag}"));
    // `git worktree add` refuses to create INTO an already-existing non-empty
    // directory in some git versions; remove the just-created empty dir first
    // so `git worktree add <path>` can create it fresh.
    std::fs::remove_dir(&worktree_root).unwrap();
    assert!(
        Command::new("git")
            .args(["worktree", "add", "-q", worktree_root.to_str().unwrap()])
            .current_dir(&main_repo)
            .status()
            .unwrap()
            .success(),
        "git worktree add failed"
    );

    (main_repo, worktree_root)
}

/// Write a minimal, valid `tasks.toml` directly (bypassing `backlog add`
/// entirely) containing exactly one task with the given id/title/project,
/// status "pending". Mirrors the exact shape `crate::store::save` writes
/// (`[[task]]` array-of-tables; see `crates/backlog/src/store.rs`'s
/// `TasksFile`/`crates/backlog/src/task.rs`'s `Task`), so a real
/// `backlog list` parses it identically to a file `add` would have produced.
fn seed_tasks_toml(store_dir: &std::path::Path, id: &str, title: &str, project: &str) {
    std::fs::create_dir_all(store_dir).unwrap();
    let toml = format!(
        r#"[[task]]
id = "{id}"
title = "{title}"
project = "{project}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = 0.0
"#
    );
    std::fs::write(store_dir.join("tasks.toml"), toml).unwrap();
}

/// Test (a): Tasks added via --project <main> from a worktree must write to
/// worktree's .backlog, NOT the main tree's .backlog. This is a critical
/// §8 violation if it fails.
#[test]
fn add_with_main_project_writes_to_worktree_not_main() {
    let home = temp_home("add-writes-to-wt");
    let (main_tree, wt_tree) = setup_repo_with_worktree("add-writes-to-wt");

    let main_canonical = main_tree.canonicalize().unwrap();
    let main_path_str = main_canonical.to_string_lossy();

    let (add_code, add_out, add_err) = run_in(
        &["add", "--title", "Test Task", "--project", &main_path_str],
        "",
        &home,
        &wt_tree,
    );
    assert_eq!(
        add_code, 0,
        "add must succeed; stdout={}, stderr={}",
        add_out, add_err
    );

    let wt_tasks = wt_tree.join(".backlog").join("tasks.toml");
    let main_tasks = main_tree.join(".backlog").join("tasks.toml");

    assert!(
        wt_tasks.exists(),
        "worktree .backlog/tasks.toml must exist after add, got: {:?}",
        wt_tasks
    );
    assert!(
        !main_tasks.exists(),
        "main tree .backlog/tasks.toml must NOT be created; found at {:?}",
        main_tasks
    );

    let (list_code, list_out, _) = run_in(&["list", "--json"], "", &home, &wt_tree);
    assert_eq!(list_code, 0, "list must succeed");
    let json: serde_json::Value = serde_json::from_str(list_out.trim())
        .unwrap_or_else(|e| panic!("list --json must emit valid JSON ({e}): {list_out}"));
    let arr = json.as_array().expect("top-level must be array");
    assert!(
        !arr.is_empty() && arr[0]["title"] == "Test Task",
        "task must appear in worktree's list, got: {}",
        list_out
    );
}

/// Test (b): A task stored with the main tree's project path (via --project
/// <main>) must be visible from the worktree's cwd-scoped `backlog list`
/// (no --project, no --all).
#[test]
fn list_sees_task_stored_with_main_project_path() {
    let home = temp_home("list-sees-main-project");
    let (main_tree, wt_tree) = setup_repo_with_worktree("list-sees-main-project");

    let main_canonical = main_tree.canonicalize().unwrap();
    let main_path_str = main_canonical.to_string_lossy();

    let (add_code, _, add_err) = run_in(
        &[
            "add",
            "--title",
            "Task for main project",
            "--project",
            &main_path_str,
        ],
        "",
        &home,
        &wt_tree,
    );
    assert_eq!(
        add_code, 0,
        "add must succeed to setup test; stderr: {}",
        add_err
    );

    let (list_code, list_out, list_err) = run_in(&["list"], "", &home, &wt_tree);
    assert_eq!(list_code, 0, "list must succeed; stderr: {}", list_err);

    assert!(
        list_out.contains("Task for main project"),
        "cwd-scoped list from worktree must see task stored with main project path, got:\n{}",
        list_out
    );
}

/// Test (c): Backward compatibility — an EXISTING task recorded under a
/// project's absolute path (main tree's canonical path) must remain visible
/// from `backlog list`'s default cwd-scoped resolution from BOTH the main
/// tree's cwd and the worktree's cwd.
///
/// Deliberately does NOT go through `backlog add`: `add`'s current write
/// path already determines where the byte gets written (that's what (a)
/// tests), and using it here would just re-prove (a)/(b) under a different
/// name instead of proving the READ-side backward-compat invariant. Instead
/// this seeds `tasks.toml` directly in EACH checkout's own store (axis A:
/// the store is resolved from the cwd of the checkout doing the reading, not
/// from the `project` value recorded on the task) with the identical task —
/// same id, same title, same `project` field (main's canonical absolute
/// path) — so the only variable under test is whether `list`'s per-cwd
/// PROJECT FILTER still matches that recorded value from each cwd.
#[test]
fn list_sees_same_task_from_main_tree_cwd() {
    let home = temp_home("list-sees-from-main");
    let (main_tree, wt_tree) = setup_repo_with_worktree("list-sees-from-main");

    let main_canonical = main_tree.canonicalize().unwrap();
    let main_path_str = main_canonical.to_string_lossy();

    // Seed BOTH checkouts' own local stores with the SAME task, recorded
    // under main's canonical absolute path — exactly what a pre-existing
    // tasks.toml (written before any resolution change) would contain.
    seed_tasks_toml(
        &main_tree.join(".backlog"),
        "aaaaaaaa",
        "Shared task",
        &main_path_str,
    );
    seed_tasks_toml(
        &wt_tree.join(".backlog"),
        "aaaaaaaa",
        "Shared task",
        &main_path_str,
    );

    // Sanity check (must hold regardless of the defect under test): from
    // main's OWN cwd, a task recorded under main's own canonical path is
    // found by the default cwd-scoped list.
    let (main_list_code, main_list_out, main_list_err) = run_in(&["list"], "", &home, &main_tree);
    assert_eq!(
        main_list_code, 0,
        "list from main must succeed; stderr: {}",
        main_list_err
    );
    assert!(
        main_list_out.contains("Shared task"),
        "list from main's own cwd must see a task recorded under main's own \
         canonical project path (sanity check independent of the defect under \
         test), got:\n{}",
        main_list_out
    );

    // The actual invariant under test: the IDENTICAL task, recorded under
    // the SAME `project` value (main's canonical path), and physically
    // present in the worktree's own store, must ALSO be visible from the
    // worktree's cwd-scoped default list. Today it is not: `list`'s
    // effective project filter resolves the worktree's cwd to the
    // WORKTREE's own canonical root (`git rev-parse --show-toplevel` from
    // inside a linked worktree returns the worktree itself, not main), which
    // does not equal the task's recorded `project` (main's canonical path),
    // so `project_matches` rejects it and the pre-existing task silently
    // disappears from the default view.
    let (wt_list_code, wt_list_out, wt_list_err) = run_in(&["list"], "", &home, &wt_tree);
    assert_eq!(
        wt_list_code, 0,
        "list from worktree must succeed; stderr: {}",
        wt_list_err
    );
    assert!(
        wt_list_out.contains("Shared task"),
        "an existing task recorded under main's canonical project path, and \
         physically present in the worktree's own store, must remain visible \
         from the worktree's cwd-scoped default list (backward compat); \
         got:\n{}",
        wt_list_out
    );
}

/// Test (d): Fail-closed defect — when a checkout's `.git` link is broken (a
/// dangling worktree pointer: `gitdir:` names a path that no longer exists),
/// `backlog list`'s default cwd-scoped project resolution cannot actually
/// determine the checkout's canonical project identity. A REAL,
/// already-persisted task recorded in that checkout's own `tasks.toml` must
/// not silently disappear into an empty ("no tasks" / `[]`) result
/// indistinguishable from a genuinely empty queue.
///
/// This is deliberately NOT the "no `.git` anywhere above cwd" case (that
/// path is a DOCUMENTED, intentional non-error default — `Config::
/// tasks_path_for`'s doc comment: a path with no repo root cannot be
/// resolved into a project store, so it falls back to the legacy
/// `~/.backlog`, which is by design, not a "cannot determine" defect). Here
/// `.git` DOES exist and DOES resolve the on-disk store correctly (the task
/// is physically sitting in `<cwd>/.backlog/tasks.toml`, proven below via
/// `--all`) — the defect is specifically that `list`'s effective-project
/// filter (`harness_core::discovery::resolve_repo_root`, which shells out to
/// `git rev-parse --show-toplevel`) silently swallows that command's failure
/// and falls back to the raw cwd, filtering out a task recorded under a
/// different (now-unreachable) canonical path with no error, no warning, and
/// no distinguishing marker versus a genuinely empty queue.
///
/// # Why this test carries a CONTROL
///
/// The claim under test is a claim of DISTINGUISHABILITY: observation A
/// ("scope could not be determined") must differ from observation B
/// ("genuinely zero tasks"). `A != B` cannot be asserted by observing A
/// alone — so this test also runs B, under the SAME isolated HOME: a healthy
/// `git init` checkout that owns no store at all. That control is first
/// pinned to the genuinely-empty baseline (`exit 0` + `[]`) so it cannot
/// silently drift, and only then is the broken checkout's observable pair
/// `(exit code, trimmed stdout)` asserted to differ from it.
///
/// The control is what makes this oracle reject the DEGENERATE class of
/// fix. Without it, "make an empty result always exit non-zero" or "always
/// print a marker" would turn the test green while leaving the two cases
/// every bit as indistinguishable as before — because such a change moves B
/// by exactly as much as it moves A, and an assertion that only ever looks
/// at A cannot notice that. Any change that renders emptiness uniformly
/// therefore still fails here.
///
/// What this control does NOT pin down (stated so the prose does not claim
/// more than the code checks): the control checkout owns no store at all,
/// whereas the broken checkout owns a store whose single task is filtered
/// out. So a fix that keys on "the store was non-empty but the filter
/// removed everything" would also pass, without ever consulting whether
/// scope resolution succeeded. That is a strictly better outcome than
/// today's silent `[]` and still satisfies (d), but it is a different
/// discriminator than "undetermined"; asserting the latter specifically
/// would need a third arm (a healthy checkout with a filtered-out task),
/// which is out of scope for this test.
#[test]
fn broken_git_link_does_not_silently_hide_an_existing_task() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("broken-git-link");
    let broken_dir = temp_dir("broken-git-link-dir");

    // A `.git` FILE (not directory) whose `gitdir:` target does not exist —
    // a dangling worktree link (e.g. its main tree was deleted or moved).
    // `.git` still `exists()` (config.rs's store-resolution only checks
    // existence), so the LOCAL `.backlog` is still where the store
    // physically resolves to — but `git rev-parse --show-toplevel` run
    // against this cwd fails (not a valid repository), which is the actual
    // "cannot determine" condition this test targets.
    std::fs::write(
        broken_dir.join(".git"),
        "gitdir: /definitely/nonexistent-xyz-q7f3k9z2/.git/worktrees/broken\n",
    )
    .unwrap();

    // Seed a REAL task directly into this checkout's own store, recorded
    // under some OTHER canonical project path (as it would have been before
    // whatever broke the `.git` link) — deliberately different from
    // `broken_dir` itself, so a naive "fall back to raw cwd" resolution does
    // NOT coincidentally match it.
    let stale_project = broken_dir.parent().unwrap().join("some-old-canonical-root");
    seed_tasks_toml(
        &broken_dir.join(".backlog"),
        "deadbeef",
        "Stranded task",
        &stale_project.to_string_lossy(),
    );

    // Anti-vacuity: prove the task genuinely, physically exists in this
    // checkout's own store (not that we mis-seeded an empty file) — `--all`
    // drops the project filter entirely and must see it.
    let (all_code, all_out, all_err) = run_in(&["list", "--all"], "", &home, &broken_dir);
    assert_eq!(all_code, 0, "list --all must succeed; stderr: {}", all_err);
    assert!(
        all_out.contains("Stranded task"),
        "anti-vacuity check failed: the seeded task must be physically present \
         in this checkout's store (via --all, which bypasses the project \
         filter under test), got:\n{}",
        all_out
    );

    // ---- The CONTROL: observation B, "genuinely zero tasks" ----
    //
    // A healthy checkout (real `git init`, so `git rev-parse
    // --show-toplevel` succeeds and the project scope IS determinable) that
    // simply owns no store. Run under the SAME isolated HOME as the broken
    // checkout, so any fix that changes how emptiness is rendered globally
    // moves this observation too. It differs from the broken checkout in two
    // respects — resolvability, and owning no store rather than a store
    // whose one task is filtered out — see this test's doc comment for which
    // of the two the assertion below can and cannot attribute the difference
    // to. This is the thing the broken checkout must be distinguishable FROM.
    let empty_repo = temp_dir("genuinely-empty-repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&empty_repo)
            .status()
            .unwrap()
            .success(),
        "git init failed for the genuinely-empty control repo"
    );
    assert!(
        !empty_repo.join(".backlog").exists(),
        "the control must own no store at all, but {:?} exists",
        empty_repo.join(".backlog")
    );

    let (empty_code, empty_out, empty_err) = run_in(&["list", "--json"], "", &home, &empty_repo);

    // Pin the control to the genuinely-empty baseline so it cannot silently
    // drift into something else and make the comparison below vacuously
    // true. If this ever fires, the control stopped being a control.
    assert!(
        empty_code == 0 && empty_out.trim() == "[]",
        "control setup is broken: a healthy, resolvable checkout with zero \
         tasks must be observed as exit 0 + `[]` (that IS the \"genuinely \
         empty queue\" baseline this test compares against). Got:\n  \
         exit_code: {}\n  stdout: {:?}\n  stderr: {}",
        empty_code,
        empty_out,
        empty_err
    );

    // ---- Observation A: scope cannot be determined ----
    //
    // The DEFAULT (no --project, no --all) scoped list, run where project
    // resolution demonstrably fails.
    let (list_code, list_out, list_err) = run_in(&["list", "--json"], "", &home, &broken_dir);

    // The actual assertion, and it is exactly the comparison the prose above
    // claims: the observable pair a downstream reader gets from the
    // undetermined case must DIFFER from the pair it gets from a genuinely
    // empty queue. Nothing weaker is asserted here, and nothing stronger is
    // claimed: stderr is deliberately excluded from the compared pair, since
    // a caller that parses `--json` reads exit code and stdout.
    let undetermined_observation = (list_code, list_out.trim().to_string());
    let genuinely_empty_observation = (empty_code, empty_out.trim().to_string());

    assert_ne!(
        undetermined_observation, genuinely_empty_observation,
        "an unresolvable (dangling .git link) project scope produces the SAME \
         observable (exit code, stdout) pair as a genuinely empty queue, so a \
         downstream reader (e.g. autoflow) cannot tell the two apart — yet the \
         task demonstrably exists on disk (proven via --all above), while the \
         control checkout genuinely has zero tasks.\n  \
         undetermined scope (cwd={:?}): exit_code={} stdout={:?} stderr={}\n  \
         genuinely empty  (cwd={:?}): exit_code={} stdout={:?} stderr={}",
        broken_dir, list_code, list_out, list_err, empty_repo, empty_code, empty_out, empty_err
    );
}
