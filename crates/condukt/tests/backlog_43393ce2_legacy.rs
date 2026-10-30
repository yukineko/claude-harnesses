// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 43393ce2 (== 4708069b): run state is addressed by the MAIN worktree
//! root, so a run created in a linked worktree is visible from the main tree.
//!
//! These tests pin the two upgrade-side properties of that re-keying:
//!
//! 1. A run that was stored under the LEGACY per-checkout key (the
//!    `repo_root(cwd)` key of the linked worktree it was created in, which is
//!    where every pre-fix condukt wrote) stays readable — `list`, `show`,
//!    `set`, `gate` — from that worktree, and a write for it lands back where
//!    it was found instead of forking a second copy under the canonical key.
//! 2. A main worktree root that cannot be determined (an unparseable `.git`
//!    gitfile) is an error, not a silent fallback to the per-checkout key.
//!
//! Every invocation pins `HOME` to a fixture dir so the real `~/.condukt` is
//! never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-43393ce2-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .expect("condukt spawns")
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.t")
        .output()
        .expect("git spawns (failing rather than skipping)");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}{}",
        dir.display(),
        s(&out.stdout),
        s(&out.stderr)
    );
}

/// `(repo, linked worktree, home)`.
fn repo_with_worktree(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = unique_dir(tag);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    git_ok(&repo, &["add", "seed.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "seed"]);
    let wt = base.join("wt");
    git_ok(
        &repo,
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    (repo, wt, home)
}

fn key_dir(home: &Path, root: &Path) -> PathBuf {
    home.join(".condukt")
        .join("state")
        .join(harness_core::projkey::project_key(root))
}

/// Every file in `dir` whose name starts with `<run>.` — the run's state file
/// and its per-run siblings.
fn run_files(dir: &Path, run: &str) -> Vec<String> {
    let prefix = format!("{run}.");
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn legacy_per_worktree_run_stays_readable_and_is_not_forked() {
    let (repo, wt, home) = repo_with_worktree("legacy");
    let run = "legacy-run";

    // Create the run from the MAIN tree: its key is the canonical key both
    // before and after the fix, so this is a well-formed run-state on disk.
    let dec = repo.parent().unwrap().join("d.json");
    std::fs::write(&dec, r#"{"goal":"g","tasks":[{"id":"t1","title":"x"}]}"#).unwrap();
    let out = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "init: {}", s(&out.stderr));

    // Simulate a pre-fix run created in the linked worktree: move every file of
    // the run under the LEGACY key — the per-checkout `repo_root(wt)` key.
    let canonical = key_dir(&home, &repo);
    let legacy = key_dir(&home, &wt);
    assert_ne!(canonical, legacy, "fixture: keys must differ");
    let files = run_files(&canonical, run);
    assert!(
        files.iter().any(|f| f == &format!("{run}.json")),
        "fixture: init wrote the run under the canonical key: {files:?}"
    );
    std::fs::create_dir_all(&legacy).unwrap();
    for f in &files {
        std::fs::rename(canonical.join(f), legacy.join(f)).unwrap();
    }
    assert!(run_files(&canonical, run).is_empty());

    // list / show / gate from the worktree the run was created in.
    let listed = condukt(&wt, &home, &["state", "list"]);
    assert!(
        listed.status.success() && s(&listed.stdout).contains(run),
        "legacy run must be listed from its worktree: rc={:?} out={} err={}",
        listed.status.code(),
        s(&listed.stdout),
        s(&listed.stderr)
    );
    let shown = condukt(&wt, &home, &["state", "show", "--run", run]);
    assert!(
        shown.status.success(),
        "legacy run must be shown from its worktree: {}",
        s(&shown.stderr)
    );
    let gate = condukt(&wt, &home, &["state", "gate", "--run", run]);
    let gate_err = s(&gate.stderr);
    assert!(
        gate_err.contains("gate FAIL") && !gate_err.contains("no run"),
        "legacy run must reach a gate verdict (incomplete => FAIL), not 'no run': rc={:?} {gate_err}",
        gate.status.code()
    );

    // set: the write must land back under the legacy key, not fork a copy.
    let set = condukt(
        &wt,
        &home,
        &[
            "state", "set", "--run", run, "--task", "t1", "--status", "running",
        ],
    );
    assert!(set.status.success(), "set: {}", s(&set.stderr));
    assert!(
        run_files(&canonical, run).is_empty(),
        "a write for a legacy run forked it under the canonical key: {:?}",
        run_files(&canonical, run)
    );
    let stored = std::fs::read_to_string(legacy.join(format!("{run}.json"))).unwrap();
    assert!(
        stored.contains("\"running\""),
        "the set did not land in the legacy copy: {stored}"
    );
    let reshown = s(&condukt(&wt, &home, &["state", "show", "--run", run]).stdout);
    assert!(reshown.contains("\"running\""), "show after set: {reshown}");
}

#[test]
fn undeterminable_main_worktree_root_is_an_error_not_a_fallback() {
    let (_repo, wt, home) = repo_with_worktree("undet");
    // A `.git` FILE with no `gitdir:` line: whether this is a linked worktree,
    // and so which main worktree root keys its state, cannot be determined.
    std::fs::write(wt.join(".git"), "not a gitfile\n").unwrap();
    let out = condukt(&wt, &home, &["state", "list"]);
    assert!(
        !out.status.success(),
        "an undeterminable main worktree root must fail, not list from a fallback \
         key: rc={:?} out={} err={}",
        out.status.code(),
        s(&out.stdout),
        s(&out.stderr)
    );
    assert!(
        s(&out.stderr).contains("main worktree root"),
        "the error must name the undetermined main worktree root: {}",
        s(&out.stderr)
    );
    // Nothing was published under the per-checkout fallback key.
    assert!(
        !key_dir(&home, &wt).exists(),
        "state was published under the per-checkout fallback key"
    );
}

#[test]
fn escalation_enqueued_in_a_worktree_is_listed_from_the_main_tree() {
    let (repo, wt, home) = repo_with_worktree("esc");
    let add = condukt(
        &wt,
        &home,
        &[
            "escalate",
            "add",
            "--run",
            "r1",
            "--task",
            "t1",
            "--question",
            "Q?",
            "--option",
            "a",
            "--option",
            "b",
        ],
    );
    assert!(add.status.success(), "add: {}", s(&add.stderr));
    let listed = condukt(&repo, &home, &["escalate", "list", "--run", "r1", "--json"]);
    assert!(
        listed.status.success() && s(&listed.stdout).contains("Q?"),
        "an escalation enqueued from a linked worktree must be listed from the main \
         tree: rc={:?} out={} err={}",
        listed.status.code(),
        s(&listed.stdout),
        s(&listed.stderr)
    );
}

#[test]
fn legacy_escalation_is_listed_and_resolved_in_place() {
    let (repo, wt, home) = repo_with_worktree("escleg");
    // Enqueue canonically (from the main tree), then move the registry under
    // the worktree's LEGACY per-checkout key, as a pre-fix condukt left it.
    let add = condukt(
        &repo,
        &home,
        &[
            "escalate",
            "add",
            "--run",
            "r1",
            "--task",
            "t1",
            "--question",
            "Q?",
            "--option",
            "a",
        ],
    );
    assert!(add.status.success(), "add: {}", s(&add.stderr));
    let id = s(&add.stdout).lines().next().unwrap().trim().to_string();
    let canonical = key_dir(&home, &repo).join("escalations.json");
    let legacy_dir = key_dir(&home, &wt);
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::rename(&canonical, legacy_dir.join("escalations.json")).unwrap();

    let listed = s(&condukt(&wt, &home, &["escalate", "list", "--run", "r1", "--json"]).stdout);
    assert!(
        listed.contains(&id),
        "legacy escalation listed from its worktree: {listed}"
    );
    let res = condukt(
        &wt,
        &home,
        &["escalate", "resolve", "--id", &id, "--choice", "a"],
    );
    assert!(res.status.success(), "resolve: {}", s(&res.stderr));
    assert!(
        !canonical.exists(),
        "resolving a legacy escalation forked a canonical registry"
    );
    let stored = std::fs::read_to_string(legacy_dir.join("escalations.json")).unwrap();
    assert!(
        stored.contains("\"resolved\": true"),
        "resolution must land in the legacy registry: {stored}"
    );
}
