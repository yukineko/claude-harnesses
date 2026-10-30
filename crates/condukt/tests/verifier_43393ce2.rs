// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier pins for backlog 43393ce2 / 4708069b (run state keyed
//! by the MAIN worktree root). Written by the verifier, not the implementer.
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
        "condukt-v43393ce2-{tag}-{}-{}",
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
        .expect("git spawns");
    assert!(
        out.status.success(),
        "git {args:?}: {}{}",
        s(&out.stdout),
        s(&out.stderr)
    );
}

/// `(repo, linked worktree, home)`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = unique_dir(tag);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "x\n").unwrap();
    git_ok(&repo, &["add", "f.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "seed"]);
    let wt = base.join("wt");
    git_ok(
        &repo,
        &["worktree", "add", "-q", "-b", "vside", wt.to_str().unwrap()],
    );
    (repo, wt, home)
}

fn state_root(home: &Path) -> PathBuf {
    home.join(".condukt").join("state")
}

fn key_dir(home: &Path, root: &Path) -> PathBuf {
    state_root(home).join(harness_core::projkey::project_key(root))
}

/// Every `<run>.json` anywhere under the state root (recursive).
fn copies_of(home: &Path, run: &str) -> Vec<PathBuf> {
    fn walk(d: &Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, name, out);
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&state_root(home), &format!("{run}.json"), &mut out);
    out
}

fn init_run(cwd: &Path, home: &Path, run: &str) {
    let dec = cwd.parent().unwrap().join(format!("{run}-dec.json"));
    std::fs::write(&dec, r#"{"goal":"g","tasks":[{"id":"t1","title":"x"}]}"#).unwrap();
    let out = condukt(
        cwd,
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
}

/// A run created in the MAIN tree is shown and listed from a linked worktree.
#[test]
fn verifier_main_tree_run_is_shown_and_listed_from_worktree() {
    let (repo, wt, home) = fixture("show");
    init_run(&repo, &home, "vrun-main");
    let shown = condukt(&wt, &home, &["state", "show", "--run", "vrun-main"]);
    assert!(
        shown.status.success() && s(&shown.stdout).contains("vrun-main"),
        "show from worktree: rc={:?} out={} err={}",
        shown.status.code(),
        s(&shown.stdout),
        s(&shown.stderr)
    );
    let listed = condukt(&wt, &home, &["state", "list"]);
    assert!(
        listed.status.success() && s(&listed.stdout).contains("vrun-main"),
        "list from worktree: {}",
        s(&listed.stdout)
    );
}

/// A run created in a worktree, then `state set` from the main tree, lands in
/// the canonical file and there is exactly ONE copy anywhere in the state dir.
#[test]
fn verifier_worktree_run_set_from_main_tree_has_one_canonical_copy() {
    let (repo, wt, home) = fixture("set");
    init_run(&wt, &home, "vrun-wt");
    let set = condukt(
        &repo,
        &home,
        &[
            "state", "set", "--run", "vrun-wt", "--task", "t1", "--status", "running",
        ],
    );
    assert!(
        set.status.success(),
        "set from main tree: rc={:?} err={}",
        set.status.code(),
        s(&set.stderr)
    );
    let copies = copies_of(&home, "vrun-wt");
    let canonical = key_dir(&home, &repo).join("vrun-wt.json");
    assert_eq!(copies.len(), 1, "exactly one run file: {copies:?}");
    assert_eq!(
        copies[0].canonicalize().unwrap(),
        canonical.canonicalize().unwrap()
    );
    let stored = std::fs::read_to_string(&canonical).unwrap();
    assert!(stored.contains("\"running\""), "{stored}");
    // And the worktree reads the same updated state back.
    let shown = s(&condukt(&wt, &home, &["state", "show", "--run", "vrun-wt"]).stdout);
    assert!(shown.contains("\"running\""), "{shown}");
}

/// A garbage `.git` gitfile makes the main worktree root undeterminable:
/// `state list` and `state show` are non-zero, not a per-checkout fallback.
#[test]
fn verifier_garbage_gitfile_is_nonzero_for_list_and_show() {
    let (_repo, wt, home) = fixture("garbage");
    std::fs::write(wt.join(".git"), "garbage\n").unwrap();
    let listed = condukt(&wt, &home, &["state", "list"]);
    assert!(
        !listed.status.success(),
        "list must be non-zero: out={} err={}",
        s(&listed.stdout),
        s(&listed.stderr)
    );
    let st = condukt(&wt, &home, &["status"]);
    assert!(
        !st.status.success(),
        "status must be non-zero (not 'no open runs'): out={}",
        s(&st.stdout)
    );
}

/// A legacy run (only under the worktree's per-checkout key) is written back
/// in place from its worktree and never forked into a canonical copy; and from
/// the main tree a `set` on it fails without creating a canonical copy.
#[test]
fn verifier_legacy_run_write_back_never_forks() {
    let (repo, wt, home) = fixture("legacy");
    init_run(&repo, &home, "vrun-leg");
    let canonical = key_dir(&home, &repo);
    let legacy = key_dir(&home, &wt);
    std::fs::create_dir_all(&legacy).unwrap();
    for e in std::fs::read_dir(&canonical).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("vrun-leg.") {
            std::fs::rename(e.path(), legacy.join(&n)).unwrap();
        }
    }
    // From the main tree: no legacy view, so set must fail and fork nothing.
    let main_set = condukt(
        &repo,
        &home,
        &[
            "state", "set", "--run", "vrun-leg", "--task", "t1", "--status", "running",
        ],
    );
    assert!(!main_set.status.success());
    assert_eq!(copies_of(&home, "vrun-leg").len(), 1);
    // From the worktree: two writes in a row, still one copy, in legacy.
    for st in ["running", "failed"] {
        let out = condukt(
            &wt,
            &home,
            &[
                "state", "set", "--run", "vrun-leg", "--task", "t1", "--status", st,
            ],
        );
        assert!(out.status.success(), "set {st}: {}", s(&out.stderr));
    }
    let copies = copies_of(&home, "vrun-leg");
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert!(copies[0].starts_with(&legacy), "{copies:?}");
    assert!(
        !canonical.join("vrun-leg.decomposition.json").exists(),
        "decomposition forked"
    );
}

/// An unreadable legacy dir makes the existence probe an error: the run must
/// not be silently resolved to a fresh canonical copy.
#[cfg(unix)]
#[test]
fn verifier_unreadable_legacy_dir_is_an_error_not_a_canonical_fork() {
    use std::os::unix::fs::PermissionsExt;
    let (repo, wt, home) = fixture("unread");
    init_run(&repo, &home, "vrun-ur");
    let canonical = key_dir(&home, &repo);
    let legacy = key_dir(&home, &wt);
    std::fs::create_dir_all(&legacy).unwrap();
    for e in std::fs::read_dir(&canonical).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("vrun-ur.") {
            std::fs::rename(e.path(), legacy.join(&n)).unwrap();
        }
    }
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = condukt(
        &wt,
        &home,
        &[
            "state", "set", "--run", "vrun-ur", "--task", "t1", "--status", "running",
        ],
    );
    // `state init` writes without loading first: with the probe swallowed to
    // "absent" it would create a fresh canonical copy beside the legacy run.
    let dec = wt.parent().unwrap().join("ur-dec.json");
    std::fs::write(&dec, r#"{"goal":"g","tasks":[{"id":"t1","title":"x"}]}"#).unwrap();
    let init = condukt(
        &wt,
        &home,
        &[
            "state",
            "init",
            "--run",
            "vrun-ur",
            "--file",
            dec.to_str().unwrap(),
        ],
    );
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success(), "set must fail: {}", s(&out.stdout));
    assert!(
        !init.status.success(),
        "init must fail while the legacy probe cannot be answered: {}",
        s(&init.stdout)
    );
    assert!(
        !canonical.join("vrun-ur.json").exists(),
        "a canonical copy was forked while the legacy probe failed"
    );
}
