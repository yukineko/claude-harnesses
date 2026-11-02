#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Production `git` spawns in harness-core must answer about the directory they
//! are given, not about the repository an inherited `GIT_DIR` names (repo B).
//!
//! Each site has a `<site>_ignores_inherited_git_dir` parent test that re-execs
//! this test binary as a child carrying `GIT_DIR=<B>/.git` (no `set_var` in the
//! parent) and an `inner_*` test that only acts when its flag env var is set.
//! The inner test asserts the CORRECT behaviour, so a failing child is the RED.

use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    let out = c
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn fixture_base(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let base = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("hc-prod-gitdir-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// A committed repo with one tracked file `rel`.
fn repo_with(dir: &Path, rel: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "fn x() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "i"]);
}

/// Re-exec this binary running only `inner`, with `GIT_DIR` set and `flag`
/// pointing at `arg`. Returns the child's combined output; asserts the child
/// really ran the inner test and that it passed.
fn run_child(inner: &str, flag: &str, arg: &Path, git_dir: &Path, extra: &[(&str, &Path)]) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(["--exact", inner, "--test-threads=1", "--nocapture"])
        .env(flag, arg)
        .env("GIT_DIR", git_dir);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("1 passed") || !out.status.success(),
        "child did not actually run the inner test:\n{text}"
    );
    assert!(out.status.success(), "child failed:\n{text}");
}

// ---- site 1: index_store::rs_files ---------------------------------------

const RS_FLAG: &str = "HC_GITDIR_RS_FILES";

#[test]
fn inner_rs_files() {
    let Some(a) = std::env::var_os(RS_FLAG) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "child must carry GIT_DIR"
    );
    let got = harness_core::index_store::rs_files(Path::new(&a));
    assert_eq!(
        got,
        Determination::known(vec!["a.rs".to_string()]),
        "rs_files listed the tracked files of the repo named by the inherited GIT_DIR, \
         not the repo that contains the given root"
    );
}

#[test]
fn rs_files_ignores_inherited_git_dir() {
    if std::env::var_os(RS_FLAG).is_some() {
        return;
    }
    let base = fixture_base("rs");
    let (a, b) = (base.join("a"), base.join("b"));
    repo_with(&a, "a.rs");
    repo_with(&b, "b.rs");
    run_child("inner_rs_files", RS_FLAG, &a, &b.join(".git"), &[]);
    let _ = std::fs::remove_dir_all(&base);
}

// ---- site 3: progress::git_head_signal -----------------------------------

const HEAD_FLAG: &str = "HC_GITDIR_HEAD_SIGNAL";

fn head_of(dir: &Path) -> String {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    let o = c
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn inner_git_head_signal() {
    let Some(a) = std::env::var_os(HEAD_FLAG) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "child must carry GIT_DIR"
    );
    let a = PathBuf::from(a);
    let got = harness_core::progress::git_head_signal(&a);
    assert_eq!(
        got,
        Determination::known(head_of(&a).into_bytes()),
        "git_head_signal returned the HEAD of the repo named by the inherited GIT_DIR, \
         not the HEAD of the repo at the given path"
    );
}

#[test]
fn git_head_signal_ignores_inherited_git_dir() {
    if std::env::var_os(HEAD_FLAG).is_some() {
        return;
    }
    let base = fixture_base("head");
    let (a, b) = (base.join("a"), base.join("b"));
    repo_with(&a, "a.rs");
    repo_with(&b, "b.rs");
    // Make B's HEAD differ from A's.
    std::fs::write(b.join("more.txt"), "m").unwrap();
    git(&b, &["add", "-A"]);
    git(&b, &["commit", "-q", "-m", "second"]);
    assert_ne!(head_of(&a), head_of(&b));
    run_child("inner_git_head_signal", HEAD_FLAG, &a, &b.join(".git"), &[]);
    let _ = std::fs::remove_dir_all(&base);
}

// ---- site 4a: discovery.rs:144 (git_toplevel_detailed via resolve_repo_root)

const ROOT_FLAG: &str = "HC_GITDIR_RESOLVE_ROOT";

#[test]
fn inner_resolve_repo_root() {
    let Some(plain) = std::env::var_os(ROOT_FLAG) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "child must carry GIT_DIR"
    );
    let plain = PathBuf::from(plain);
    let got = harness_core::discovery::resolve_repo_root(&plain);
    assert_eq!(
        got,
        plain.canonicalize().unwrap(),
        "resolve_repo_root answered with the toplevel of the repo named by the inherited \
         GIT_DIR for a directory that is in no repository"
    );
}

#[test]
fn discovery_toplevel_ignores_inherited_git_dir() {
    if std::env::var_os(ROOT_FLAG).is_some() {
        return;
    }
    let base = fixture_base("root");
    let (plain, b) = (base.join("plain"), base.join("b"));
    std::fs::create_dir_all(&plain).unwrap();
    repo_with(&b, "b.rs");
    // GIT_DIR alone makes git call the cwd the work tree (toplevel == cwd), so
    // the wrong-repo answer needs the paired GIT_WORK_TREE a hook exports.
    run_child(
        "inner_resolve_repo_root",
        ROOT_FLAG,
        &plain,
        &b.join(".git"),
        &[("GIT_WORK_TREE", b.as_path())],
    );
    let _ = std::fs::remove_dir_all(&base);
}

// ---- site 4b: discovery.rs:407 (migrate_legacy `git worktree list`) --------

const MIG_FLAG: &str = "HC_GITDIR_MIGRATE";

#[test]
fn inner_migrate_legacy() {
    let Some(a) = std::env::var_os(MIG_FLAG) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "child must carry GIT_DIR"
    );
    let got = harness_core::discovery::migrate_legacy(Path::new(&a));
    match got {
        Determination::Known(s) => assert!(
            s.migrated == 0 && s.sources.is_empty(),
            "migrate_legacy for repo A (no linked worktrees) migrated {} row(s) from {:?}: \
             it listed the worktrees of the repo named by the inherited GIT_DIR",
            s.migrated,
            s.sources
        ),
        Determination::Undetermined(why) => {
            panic!(
                "migrate_legacy for a plain repo A was Undetermined: {}",
                why.as_str()
            )
        }
    }
}

#[test]
fn migrate_legacy_ignores_inherited_git_dir() {
    if std::env::var_os(MIG_FLAG).is_some() {
        return;
    }
    let base = fixture_base("mig");
    let (a, b, bwt, home) = (
        base.join("a"),
        base.join("b"),
        base.join("b-wt"),
        base.join("home"),
    );
    repo_with(&a, "a.rs");
    repo_with(&b, "b.rs");
    git(
        &b,
        &["worktree", "add", "-q", "-b", "w", bwt.to_str().unwrap()],
    );
    std::fs::create_dir_all(&home).unwrap();
    // A legacy, worktree-keyed discovery file for B's linked worktree, written
    // where an older binary would have put it (under the child's HOME).
    let bwt_c = bwt.canonicalize().unwrap();
    let legacy = home
        .join(".compass")
        .join(harness_core::store::project_key(&bwt_c))
        .join("discovery.jsonl");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let rec = harness_core::discovery::DiscoveryRecord {
        fingerprint: harness_core::discovery::fingerprint("legacy row from B"),
        session_id: "s".into(),
        status: harness_core::discovery::Status::Discovered,
        created_at: 1,
        title: "legacy row from B".into(),
    };
    std::fs::write(
        &legacy,
        format!("{}\n", serde_json::to_string(&rec).unwrap()),
    )
    .unwrap();
    run_child(
        "inner_migrate_legacy",
        MIG_FLAG,
        &a,
        &b.join(".git"),
        &[("HOME", home.as_path())],
    );
    let _ = std::fs::remove_dir_all(&base);
}
