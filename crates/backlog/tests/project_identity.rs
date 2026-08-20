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

/// Seed a store holding SEVERAL tasks, each with its own project key and
/// weight. Ordering within `backlog next` is weight-descending, so a caller
/// can make a specific project's task sort first and thereby tell a
/// cwd-scoped pick apart from a store-wide one.
fn seed_tasks_toml_multi(store_dir: &std::path::Path, tasks: &[(&str, &str, &str, f64)]) {
    std::fs::create_dir_all(store_dir).unwrap();
    let mut toml = String::new();
    for (id, title, project, weight) in tasks {
        toml.push_str(&format!(
            r#"[[task]]
id = "{id}"
title = "{title}"
project = "{project}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = {weight:?}
"#
        ));
    }
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

/// Test (d), INVERTED 2026-08-20: a checkout whose `.git` link is dangling
/// (`gitdir:` names a path that no longer exists) can no longer hide a task
/// that is physically sitting in its own store.
///
/// The original defect: `list`'s default scope resolved the cwd to a canonical
/// project identity and filtered rows by it, so when that resolution failed,
/// a task recorded under a different (now-unreachable) label vanished into an
/// ordinary empty result. The fix at the time made the failure AUDIBLE — a
/// non-zero exit instead of `[]` — because the filter was taken as given.
///
/// The filter is what changed. A repo store is now the scope itself (see
/// `tests/project_scope.rs`), so listing this checkout's own file resolves no
/// identity at all and a dangling link cannot hide anything: the defect is
/// removed at the root rather than made audible. What this test pins is
/// therefore inverted — the task is LISTED — plus the one place an identity is
/// still load-bearing and still fails closed: `next --claim`, whose ledger key
/// IS the project identity (`main::claim_identity`), and which is the call a
/// real driver makes.
///
/// # Why this test carries a CONTROL
///
/// The surviving claim is still a claim of DISTINGUISHABILITY — "could not
/// determine" must not render as "nothing to do" — and `A != B` cannot be
/// asserted by observing A alone. So the healthy, resolvable, genuinely-empty
/// checkout is kept as B, and kept pinned to its own baseline first so it
/// cannot silently drift and make the comparison vacuous. It also still
/// rejects the degenerate fix: making emptiness uniformly noisy would move B
/// by as much as A and fail here.
#[test]
fn a_dangling_git_link_no_longer_hides_an_existing_task() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("broken-git-link");
    let broken_dir = temp_dir("broken-git-link-dir");

    // A `.git` FILE (not directory) whose `gitdir:` target does not exist —
    // a dangling worktree link (e.g. its main tree was deleted or moved).
    // `.git` still `exists()`, so this IS a repo store as far as
    // `config::locate` is concerned, while `git rev-parse --show-toplevel`
    // run against this cwd fails — the actual "cannot determine" condition.
    std::fs::write(
        broken_dir.join(".git"),
        "gitdir: /definitely/nonexistent-xyz-q7f3k9z2/.git/worktrees/broken\n",
    )
    .unwrap();

    // A REAL task in this checkout's own store, recorded under some OTHER
    // canonical project path (as it would have been before whatever broke the
    // `.git` link) — deliberately different from `broken_dir` itself, so a
    // naive "fall back to raw cwd" resolution does NOT coincidentally match.
    let stale_project = broken_dir.parent().unwrap().join("some-old-canonical-root");
    seed_tasks_toml(
        &broken_dir.join(".backlog"),
        "deadbeef",
        "Stranded task",
        &stale_project.to_string_lossy(),
    );

    // Anti-vacuity: the task genuinely, physically exists in this checkout's
    // own store (not a mis-seeded empty file).
    let (all_code, all_out, all_err) = run_in(&["list", "--all"], "", &home, &broken_dir);
    assert_eq!(all_code, 0, "list --all must succeed; stderr: {}", all_err);
    assert!(
        all_out.contains("Stranded task"),
        "anti-vacuity check failed: the seeded task must be physically present \
         in this checkout's store, got:\n{}",
        all_out
    );

    // ---- The inverted assertion: the DEFAULT list shows it ----
    let (list_code, list_out, list_err) = run_in(&["list", "--json"], "", &home, &broken_dir);
    assert_eq!(
        list_code, 0,
        "the default list must succeed: no project identity is needed to read \
         this checkout's own store; stderr: {}",
        list_err
    );
    assert!(
        list_out.contains("Stranded task"),
        "a task physically present in this checkout's own store must appear in \
         the DEFAULT listing. A repo store is the scope, so there is no label \
         to filter it out by — and a dangling `.git` link therefore cannot \
         hide it, audibly or otherwise.\nGot:\n{}",
        list_out
    );

    // ---- The CONTROL: observation B, "genuinely zero tasks" ----
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
    assert!(
        empty_code == 0 && empty_out.trim() == "[]",
        "control setup is broken: a healthy, resolvable checkout with zero \
         tasks must be observed as exit 0 + `[]` (that IS the \"genuinely \
         empty queue\" baseline). Got:\n  exit_code: {}\n  stdout: {:?}\n  \
         stderr: {}",
        empty_code,
        empty_out,
        empty_err
    );

    let genuinely_empty_observation = (empty_code, empty_out.trim().to_string());
    assert_ne!(
        (list_code, list_out.trim().to_string()),
        genuinely_empty_observation,
        "the broken checkout's listing is observationally identical to a \
         genuinely empty queue, so the task on disk is unreachable either way"
    );

    // ---- Where identity IS still load-bearing: the claim ledger ----
    //
    // `next --claim` keys the cross-checkout ledger by project identity, so an
    // undetermined identity there is a genuine "cannot determine" and must not
    // be reported as "nothing to do" (the control shows what that looks like).
    let (claim_code, claim_out, claim_err) = run_in(&["next", "--claim"], "", &home, &broken_dir);
    let (ctl_code, ctl_out, ctl_err) = run_in(&["next", "--claim"], "", &home, &empty_repo);
    assert_eq!(
        ctl_code, 0,
        "control setup is broken: a healthy checkout with nothing queued must \
         stay a clean exit-0 \"nothing to do\"; stderr: {}",
        ctl_err
    );
    assert_ne!(
        (claim_code, claim_out.trim().to_string()),
        (ctl_code, ctl_out.trim().to_string()),
        "`next --claim` under an UNDETERMINED project identity is \
         observationally identical to a genuinely empty queue, so a driver \
         cannot tell \"I could not determine which project this is\" from \
         \"there is nothing to do\" — and the claim it would record is keyed \
         by a guess, i.e. invisible to every other checkout.\n  \
         undetermined (cwd={:?}): exit_code={} stdout={:?} stderr={}\n  \
         control      (cwd={:?}): exit_code={} stdout={:?}",
        broken_dir,
        claim_code,
        claim_out,
        claim_err,
        empty_repo,
        ctl_code,
        ctl_out
    );

    // ---- Observation C, also inverted: another checkout's label ----
    //
    // A HEALTHY checkout whose store holds one row labelled with a DIFFERENT
    // path. That row used to be "legitimately filtered out" and asserted
    // indistinguishable from the empty control. It is not foreign at all: a
    // repo store is tracked and shared through git, so such a row is this same
    // repo's work written from another checkout — the 258 pending tasks
    // measured invisible on 2026-08-20. It must be LISTED.
    let other_checkout_repo = temp_dir("other-checkout-label-repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&other_checkout_repo)
            .status()
            .unwrap()
            .success(),
        "git init failed for the other-checkout-label repo"
    );
    let other_project = other_checkout_repo
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .join("some-other-canonical-root-for-observation-c");
    seed_tasks_toml(
        &other_checkout_repo.join(".backlog"),
        "c0ffee00",
        "Other-checkout task",
        &other_project.to_string_lossy(),
    );

    let (c_code, c_out, c_err) = run_in(&["list", "--json"], "", &home, &other_checkout_repo);
    assert_eq!(
        c_code, 0,
        "list must succeed for the other-checkout-label repo; stderr: {}",
        c_err
    );
    assert!(
        c_out.contains("Other-checkout task"),
        "a row in THIS repo's own tracked store, labelled with another \
         checkout's path, must be listed: the store is the scope, and \
         filtering by checkout path is what split one repo's queue into one \
         queue per machine.\nGot:\n{}",
        c_out
    );
    assert_ne!(
        (c_code, c_out.trim().to_string()),
        genuinely_empty_observation,
        "the other-checkout row is still being filtered out — this listing is \
         indistinguishable from a genuinely empty queue"
    );
}

/// Test (e): `backlog next` and `backlog list` must AGREE on what "this
/// project" means.
///
/// That is the durable claim, and it survived the 2026-08-20 scope change
/// intact — only the answer moved. `next` used to pass its `Option` straight
/// to `store::next` while `list` applied a cwd-derived project filter, so the
/// two disagreed whenever one store held more than one project key: a driver
/// picking with `next` could act on another checkout's work. Now a repo store
/// IS the scope for both, so both range over the whole file, and the row
/// labelled with another checkout's path is this repo's own work rather than
/// something to skip (`tests/project_scope.rs`).
///
/// The discriminator is deliberately NOT insertion order: the other-checkout
/// task carries a HIGHER weight, so a correctly store-wide pick returns it and
/// a still-filtered pick returns the local one. Ordering alone could otherwise
/// make either behaviour look right by coincidence.
#[test]
fn next_and_list_agree_on_scope_for_a_repo_store() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("next-default-scope");
    let local_repo = temp_dir("next-default-scope-local");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&local_repo)
            .status()
            .unwrap()
            .success(),
        "git init failed for the local repo"
    );

    let local_canonical = local_repo.canonicalize().unwrap();
    let local_project = local_canonical.to_string_lossy().into_owned();
    let other_checkout = local_canonical
        .parent()
        .unwrap()
        .join("some-other-checkout-entirely")
        .to_string_lossy()
        .into_owned();

    // One store, two labels. The other-checkout task outranks the local one by
    // weight, so a store-wide pick necessarily returns it.
    seed_tasks_toml_multi(
        &local_repo.join(".backlog"),
        &[
            ("10cal000", "Local task", &local_project, 0.0),
            ("f0re1gn0", "Other-checkout task", &other_checkout, 99.0),
        ],
    );

    // Anti-vacuity: both rows really are in THIS checkout's store.
    let (all_code, all_out, all_err) = run_in(&["list", "--all"], "", &home, &local_repo);
    assert_eq!(all_code, 0, "list --all must succeed; stderr: {}", all_err);
    assert!(
        all_out.contains("Local task") && all_out.contains("Other-checkout task"),
        "anti-vacuity check failed: both tasks must be physically present in \
         this checkout's store, got:\n{}",
        all_out
    );

    // Both commands see the whole store...
    let (list_code, list_out, list_err) = run_in(&["list", "--json"], "", &home, &local_repo);
    assert_eq!(list_code, 0, "list must succeed; stderr: {}", list_err);
    assert!(
        list_out.contains("Local task") && list_out.contains("Other-checkout task"),
        "a repo store is the scope, so the default list must show every row in \
         it — including one written from another checkout of the same repo, \
         got:\n{}",
        list_out
    );

    // ...and `next` ranks over that same set, which is the agreement claim.
    let (next_code, next_out, next_err) = run_in(&["next"], "", &home, &local_repo);
    assert_eq!(
        next_code, 0,
        "a bare `next` in a resolvable checkout must succeed; stderr: {}",
        next_err
    );
    assert!(
        next_out.contains("Other-checkout task") && !next_out.contains("Local task"),
        "`next` and `list` disagree about what \"this project\" means: the \
         listing above ranges over the whole store, so `next` must rank over \
         the same set and return the highest-weighted row in it. A driver that \
         picks with `next` must not be handed a different set than the one a \
         reader sees.\n  \
         bare next  (cwd={:?}): exit_code={} stdout={}\n  \
         bare list  (cwd={:?}): exit_code={} stdout={}",
        local_repo,
        next_code,
        next_out,
        local_repo,
        list_code,
        list_out
    );

    // And the agreement extends to the refusal: naming a project this store
    // cannot be scoped to is an error on `next` too, not a filtered answer.
    let (foreign_code, foreign_out, foreign_err) = run_in(
        &["next", "--project", &other_checkout],
        "",
        &home,
        &local_repo,
    );
    assert_ne!(
        foreign_code, 0,
        "`next --project <another repo>` must refuse, like `list` does: this \
         store cannot be scoped to another project, and answering from it \
         anyway would answer a different question than the one asked. \
         stdout={:?} stderr={}",
        foreign_out, foreign_err
    );
    assert!(
        foreign_out.trim().is_empty(),
        "a refused `next` must print nothing on stdout, got: {}",
        foreign_out
    );
}

/// Test (f): the `next` counterpart of test (d), re-pinned 2026-08-20.
///
/// The undetermined condition (a dangling worktree `.git` link) no longer
/// affects the READ: a repo store is the scope, so listing or ranking this
/// checkout's own file needs no identity and the task is handed out rather
/// than hidden. It is still load-bearing for `next --claim`, whose
/// cross-checkout ledger is KEYED by the project identity: a claim recorded
/// under a guessed key is invisible to every other checkout, i.e. no exclusion
/// at all. So the claim path must still refuse rather than print "no pending
/// tasks" on exit 0 — and the control shows what that legitimate exit 0 looks
/// like, so the two cannot be conflated.
#[test]
fn next_claim_fails_closed_when_the_project_identity_cannot_be_determined() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("next-undetermined");

    // The CONTROL: a healthy, resolvable checkout that genuinely owns no
    // store. This is what "nothing to do" legitimately looks like, and it
    // must stay exit 0 — the assertion below is about telling the
    // undetermined case apart from THIS, not about making `next` noisy.
    let empty_repo = temp_dir("next-undetermined-control");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&empty_repo)
            .status()
            .unwrap()
            .success(),
        "git init failed for the control repo"
    );
    let (ctl_code, ctl_out, ctl_err) = run_in(&["next"], "", &home, &empty_repo);
    assert_eq!(
        ctl_code, 0,
        "control setup is broken: a healthy, resolvable checkout with no \
         store must still be a clean exit-0 \"nothing to do\"; stderr: {}",
        ctl_err
    );

    // The undetermined case: a dangling worktree `.git` link, so
    // `git rev-parse --show-toplevel` fails, while a real task sits in this
    // checkout's own store recorded under some other canonical root.
    let broken_dir = temp_dir("next-undetermined-broken");
    std::fs::write(
        broken_dir.join(".git"),
        "gitdir: /definitely/nonexistent-xyz-q7f3k9z2/.git/worktrees/broken\n",
    )
    .unwrap();
    let stale_project = broken_dir.parent().unwrap().join("some-old-canonical-root");
    seed_tasks_toml(
        &broken_dir.join(".backlog"),
        "beefcafe",
        "Stranded next task",
        &stale_project.to_string_lossy(),
    );

    // Anti-vacuity: the task genuinely exists in this checkout's store.
    let (all_code, all_out, all_err) = run_in(&["list", "--all"], "", &home, &broken_dir);
    assert_eq!(all_code, 0, "list --all must succeed; stderr: {}", all_err);
    assert!(
        all_out.contains("Stranded next task"),
        "anti-vacuity check failed: the seeded task must be physically \
         present in this checkout's store, got:\n{}",
        all_out
    );

    // The READ side no longer depends on the identity, so it hands the task
    // over instead of failing closed. Pinned here (rather than left
    // unmentioned) because the old version of this test asserted the
    // opposite, and a reader needs to see which way it went.
    let (list_code, list_out, list_err) = run_in(&["list", "--json"], "", &home, &broken_dir);
    assert_eq!(
        list_code, 0,
        "the default list needs no identity for a repo store; stderr: {}",
        list_err
    );
    assert!(
        list_out.contains("Stranded next task"),
        "the task must be listed, not hidden, got:\n{}",
        list_out
    );
    let (bare_code, bare_out, bare_err) = run_in(&["next"], "", &home, &broken_dir);
    assert_eq!(
        bare_code, 0,
        "a bare `next` ranks over this store without needing an identity; \
         stderr: {}",
        bare_err
    );
    assert!(
        bare_out.contains("Stranded next task"),
        "a bare `next` must hand out the task in this checkout's own store, \
         got:\n{}",
        bare_out
    );

    // The CLAIM side still needs the identity as its ledger key, and still
    // refuses. This is the path real drivers (e.g. /flow) call.
    let (claim_code, claim_out, claim_err) = run_in(&["next", "--claim"], "", &home, &broken_dir);
    assert_ne!(
        claim_code, 0,
        "`backlog next --claim` resolved an UNDETERMINED project identity to \
         exit 0, so the claim it recorded is keyed by a guess and excludes no \
         other checkout, while a driver reads the result as ordinary work.\n  \
         exit_code={} stdout={:?} stderr={}\n  \
         genuinely-empty control: exit_code={} stdout={:?}",
        claim_code, claim_out, claim_err, ctl_code, ctl_out
    );
    assert!(
        claim_out.trim().is_empty(),
        "a refused claim must print nothing on stdout, got: {}",
        claim_out
    );
}

/// Seed a store from a caller-supplied `tasks.toml` body verbatim. Lets a
/// test pin the exact on-disk shape (including fields a newer binary adds,
/// or the absence of them in a legacy file).
fn seed_tasks_toml_raw(store_dir: &std::path::Path, body: &str) {
    std::fs::create_dir_all(store_dir).unwrap();
    std::fs::write(store_dir.join("tasks.toml"), body).unwrap();
}

/// Test (g), write side: when `canonicalize_project` degrades because the
/// project identity could not be determined, the task that gets written must
/// CARRY that fact.
///
/// `add` deliberately does not block on an undetermined scope — blocking
/// would lose the finding being filed, which is worse. But the degrade
/// currently leaves no trace on the stored task: the fallback label is
/// written into `project` and is indistinguishable from a label that was
/// genuinely resolved. A later reader (this checkout once its `.git` link is
/// repaired, or the main tree this worktree belongs to) filters on `project`
/// and the task simply is not there — the silent disappearance this test
/// exists to prevent. The marker is what makes "this label is a guess"
/// recoverable downstream.
#[test]
fn add_under_an_undetermined_scope_marks_the_stored_task() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("undetermined-write-marker");
    let broken_dir = temp_dir("undetermined-write-marker-dir");
    std::fs::write(
        broken_dir.join(".git"),
        "gitdir: /definitely/nonexistent-xyz-q7f3k9z2/.git/worktrees/broken\n",
    )
    .unwrap();

    let broken_str = broken_dir.to_string_lossy().into_owned();
    let (add_code, _add_out, add_err) = run_in(
        &[
            "add",
            "--title",
            "Filed under a guess",
            "--project",
            &broken_str,
        ],
        "",
        &home,
        &broken_dir,
    );

    // The degrade itself stays: filing must not be blocked.
    assert_eq!(
        add_code, 0,
        "add must still succeed under an undetermined scope (blocking would \
         lose the finding being filed); stderr: {}",
        add_err
    );
    assert!(
        add_err.contains("could not resolve project scope"),
        "precondition: this cwd must actually exercise the undetermined \
         degrade path (that is what makes this test about the marker rather \
         than about a resolvable add), got stderr:\n{}",
        add_err
    );

    let stored = std::fs::read_to_string(broken_dir.join(".backlog").join("tasks.toml"))
        .expect("add must have written a store");

    // Anti-vacuity: the task really was written.
    assert!(
        stored.contains("Filed under a guess"),
        "anti-vacuity check failed: the task must be in the store, got:\n{}",
        stored
    );

    assert!(
        stored.contains("project_unresolved = true"),
        "a task written while project identity was UNDETERMINED carries no \
         trace of that in the store, so nothing downstream can tell a guessed \
         `project` label apart from a resolved one — the task can silently \
         vanish from another checkout's default filtered list with no way to \
         recover it. Expected a `project_unresolved = true` marker in:\n{}",
        stored
    );
}

/// Test (h), read side: a task marked as written under an undetermined scope
/// must NOT be silently dropped by the default (project-filtered) list, and
/// must be rendered distinguishably rather than blended in.
///
/// The control in the same store is a task whose `project` also does not
/// match this checkout but which WAS resolved: that one must still be
/// filtered out, otherwise "do not drop the marked one" would have been
/// implemented as "stop filtering", which is a different (and much worse)
/// change.
#[test]
fn default_list_does_not_silently_drop_a_task_written_under_an_undetermined_scope() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("undetermined-read-visible");
    let repo = temp_dir("undetermined-read-visible-repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );

    let canonical = repo.canonicalize().unwrap();
    let elsewhere = canonical
        .parent()
        .unwrap()
        .join("a-totally-different-checkout");
    let elsewhere = elsewhere.to_string_lossy();

    // Three tasks in THIS checkout's store:
    //   - one that belongs here (must be listed, as always)
    //   - one that belongs elsewhere and was RESOLVED (must stay filtered out)
    //   - one that belongs "elsewhere" only because the label was a guess
    //     (must NOT be silently dropped)
    seed_tasks_toml_raw(
        &repo.join(".backlog"),
        &format!(
            r#"[[task]]
id = "aaaa0001"
title = "Local resolved task"
project = "{local}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = 0.0

[[task]]
id = "aaaa0002"
title = "Foreign resolved task"
project = "{elsewhere}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = 0.0

[[task]]
id = "aaaa0003"
title = "Foreign guessed task"
project = "{elsewhere}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = 0.0
project_unresolved = true
"#,
            local = canonical.to_string_lossy(),
            elsewhere = elsewhere,
        ),
    );

    // Anti-vacuity: all three are physically present and parse.
    let (all_code, all_out, all_err) = run_in(&["list", "--all"], "", &home, &repo);
    assert_eq!(all_code, 0, "list --all must succeed; stderr: {}", all_err);
    for t in [
        "Local resolved task",
        "Foreign resolved task",
        "Foreign guessed task",
    ] {
        assert!(
            all_out.contains(t),
            "anti-vacuity check failed: {:?} must be physically present in the \
             store (via --all), got:\n{}",
            t,
            all_out
        );
    }

    let (code, out, err) = run_in(&["list"], "", &home, &repo);
    assert_eq!(code, 0, "default list must succeed; stderr: {}", err);

    assert!(
        out.contains("Local resolved task"),
        "regression: the default list must still show this project's own \
         tasks, got:\n{}",
        out
    );
    // INVERTED 2026-08-20. This asserted that the resolved-but-different
    // label stayed filtered out, as the control proving "do not drop the
    // marked one" had not been implemented as "stop filtering". Stopping the
    // filtering is now exactly the change: a repo store is tracked and shared
    // through git, so a row labelled with another checkout's path is this same
    // repo's work (`tests/project_scope.rs`). The marker's value is unaffected
    // — it was never the filter, it is the RENDERING, asserted below.
    assert!(
        out.contains("Foreign resolved task"),
        "a row labelled with another checkout of this same repo must be listed \
         — a repo store is the scope, and filtering by checkout path is what \
         split one repo's queue into one queue per machine.\nGot:\n{}",
        out
    );
    assert!(
        out.contains("Foreign guessed task"),
        "a task written while project identity was UNDETERMINED was silently \
         dropped by the default (filtered) list: its `project` label is a \
         guess, so filtering on it hides the task with no diagnostic, which is \
         exactly the \"cannot determine\" -> \"nothing here\" collapse this \
         repo's gate invariant forbids.\nGot:\n{}",
        out
    );
    assert!(
        out.to_lowercase().contains("unresolved"),
        "the undetermined-scope task is listed but rendered exactly like a \
         normally-scoped one, so a reader cannot tell that its project label \
         is a guess. It must be distinguishable (the string \"unresolved\" is \
         expected somewhere in the rendering).\nGot:\n{}",
        out
    );
}

/// Test (i), backward compatibility: a `tasks.toml` written by an older
/// binary has no marker field at all, and must keep loading unchanged.
///
/// This is the constraint that makes the marker safe to add: a new field
/// that is not `#[serde(default)]` would make every pre-existing store fail
/// to parse, turning a visibility fix into total data loss.
#[test]
fn a_legacy_tasks_toml_without_the_marker_still_loads() {
    assert!(
        git_available(),
        "git is required for project-identity tests but is not available in PATH"
    );

    let home = temp_home("legacy-no-marker");
    let repo = temp_dir("legacy-no-marker-repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "git init failed"
    );
    let canonical = repo.canonicalize().unwrap();

    // Exactly the shape an older binary wrote: no `project_unresolved` key.
    seed_tasks_toml_raw(
        &repo.join(".backlog"),
        &format!(
            r#"[[task]]
id = "1e9acy01"
title = "Task from an older binary"
project = "{local}"
tags = []
status = "pending"
notes = ""
created_at = 1000
updated_at = 1000
weight = 0.0
"#,
            local = canonical.to_string_lossy(),
        ),
    );

    let (code, out, err) = run_in(&["list"], "", &home, &repo);
    assert_eq!(
        code, 0,
        "a legacy tasks.toml (no marker field) must still load; stderr: {}",
        err
    );
    assert!(
        out.contains("Task from an older binary"),
        "the legacy task must be listed unchanged, got:\n{}",
        out
    );
    // A legacy task must default to "resolved", not to "unresolved" — the
    // absent field means "this binary never recorded either way", and
    // rendering every legacy task as a guess would make the marker useless.
    assert!(
        !out.to_lowercase().contains("unresolved"),
        "a legacy task (marker field absent) was rendered as unresolved; the \
         default for a missing field must be `false`, otherwise every \
         pre-existing task is flagged and the marker carries no signal.\nGot:\n{}",
        out
    );

    let (jcode, jout, jerr) = run_in(&["list", "--json"], "", &home, &repo);
    assert_eq!(
        jcode, 0,
        "a legacy tasks.toml must also serialize to --json; stderr: {}",
        jerr
    );
    assert!(
        jout.contains("Task from an older binary"),
        "the legacy task must appear in --json, got:\n{}",
        jout
    );
}
