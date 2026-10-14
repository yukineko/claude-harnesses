#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog d87b4305 — `state init --run <id>` from the MAIN tree, for an id that
//! already exists as a LEGACY (per-checkout-keyed) run of a linked worktree,
//! creates a canonical run that `run_dir` then prefers, silently hiding the
//! legacy run from its own worktree.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-d87b4305-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("PWD", dir)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .expect("condukt spawns")
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.t")
        .output()
        .expect("git spawns");
    assert!(out.status.success(), "git {args:?}: {}", s(&out.stderr));
}

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

/// Create run `run` with goal `goal` from the main tree, then move it under the
/// legacy per-checkout key of `wt` (a pre-0.7.217 run created from `wt`).
fn legacy_run(repo: &Path, wt: &Path, home: &Path, run: &str, goal: &str) {
    let dec = repo.parent().unwrap().join(format!("{run}-d.json"));
    std::fs::write(
        &dec,
        format!(r#"{{"goal":"{goal}","tasks":[{{"id":"t1","title":"x"}}]}}"#),
    )
    .unwrap();
    let out = condukt(
        repo,
        home,
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
    let canonical = key_dir(home, repo);
    let legacy = key_dir(home, wt);
    assert_ne!(canonical, legacy);
    std::fs::create_dir_all(&legacy).unwrap();
    for f in run_files(&canonical, run) {
        std::fs::rename(canonical.join(&f), legacy.join(&f)).unwrap();
    }
    assert!(run_files(&canonical, run).is_empty());
}

#[test]
#[ignore = "backlog d87b4305: RED repro"]
fn d87b4305_canonical_init_does_not_shadow_legacy_run() {
    let (repo, wt, home) = repo_with_worktree("d87b");
    legacy_run(&repo, &wt, &home, "dup-run", "legacy-goal");
    let dec = repo.parent().unwrap().join("main-d.json");
    std::fs::write(
        &dec,
        r#"{"goal":"main-goal","tasks":[{"id":"t1","title":"x"}]}"#,
    )
    .unwrap();
    let init = condukt(
        &repo,
        &home,
        &[
            "state",
            "init",
            "--run",
            "dup-run",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    let show = condukt(&wt, &home, &["state", "show", "--run", "dup-run"]);
    let out = s(&show.stdout);
    let shadowed = init.status.success()
        && show.status.success()
        && out.contains("main-goal")
        && !out.contains("legacy-goal");
    assert!(
        !shadowed,
        "the legacy run 'dup-run' is silently hidden from its own worktree by a \
         canonical run created with the same id from the main tree"
    );
}
