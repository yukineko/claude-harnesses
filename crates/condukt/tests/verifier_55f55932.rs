// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier test for backlog 55f55932 (written by the verifier,
//! not the implementer). A blank `--hashkey` (tab/newline-only, mixed
//! whitespace) must be refused by `claim-task`, `release-task` and
//! `is-claimed` as invalid input (exit 2), must never touch the claims
//! registry, and must never release or shadow a real claim. HOME is pinned
//! to a temp dir so the real `~/.condukt` is never read or written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

/// Every `claims.json` under `dir`, with its bytes.
fn claims_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(claims_files(&p));
        } else if p.file_name().is_some_and(|n| n == "claims.json") {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "condukt-verifier-55f55932-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn c(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_WORKTREE_BASE", self.base.join("wtb"))
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
    }

    fn registry(&self) -> Vec<(PathBuf, Vec<u8>)> {
        claims_files(&self.base)
    }
}

fn text(o: &Output) -> String {
    format!(
        "rc={:?} stdout={} stderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const BLANKS: &[&str] = &["\t", "\n", " \t\n ", "\r\n"];

#[test]
fn claim_task_blank_keys_exit_2_and_write_nothing() {
    for (i, k) in BLANKS.iter().enumerate() {
        let f = Fx::new(&format!("claim{i}"));
        let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", k]);
        assert_eq!(o.status.code(), Some(2), "claim-task {k:?}: {}", text(&o));
        assert!(
            o.stdout.is_empty(),
            "claim-task {k:?} printed: {}",
            text(&o)
        );
        assert!(
            f.registry().is_empty(),
            "claim-task {k:?} created a claims registry: {:?}",
            f.registry()
        );
    }
}

#[test]
fn claim_task_blank_key_mixed_with_real_key_claims_neither() {
    let f = Fx::new("mixed");
    let o = f.c(&[
        "state",
        "claim-task",
        "--run",
        "r1",
        "--hashkey",
        "real-key",
        "--hashkey",
        "\t",
    ]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(
        f.registry().is_empty(),
        "registry written: {:?}",
        f.registry()
    );
    let q = f.c(&["state", "is-claimed", "--hashkey", "real-key"]);
    assert_eq!(
        q.status.code(),
        Some(1),
        "real-key must be unclaimed: {}",
        text(&q)
    );
}

#[test]
fn is_claimed_blank_key_exits_2_no_stdout_no_registry() {
    for (i, k) in BLANKS.iter().chain(&[""]).enumerate() {
        let f = Fx::new(&format!("isc{i}"));
        let o = f.c(&["state", "is-claimed", "--hashkey", k]);
        assert_eq!(o.status.code(), Some(2), "is-claimed {k:?}: {}", text(&o));
        assert!(
            o.stdout.is_empty(),
            "is-claimed {k:?} printed: {}",
            text(&o)
        );
        assert!(
            f.registry().is_empty(),
            "registry touched: {:?}",
            f.registry()
        );
    }
}

#[test]
fn release_task_blank_key_does_not_release_or_touch_a_real_claim() {
    let f = Fx::new("release");
    let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", "hk-real"]);
    assert!(o.status.success(), "setup claim failed: {}", text(&o));
    let before = f.registry();
    assert!(!before.is_empty(), "setup claim wrote no registry");
    for k in BLANKS.iter().chain(&[""]) {
        let r = f.c(&["state", "release-task", "--run", "r1", "--hashkey", k]);
        assert_eq!(r.status.code(), Some(2), "release-task {k:?}: {}", text(&r));
        assert_eq!(f.registry(), before, "release-task {k:?} modified registry");
    }
    let q = f.c(&["state", "is-claimed", "--hashkey", "hk-real"]);
    assert_eq!(q.status.code(), Some(0), "real claim lost: {}", text(&q));
}

#[test]
fn release_task_with_no_hashkey_is_a_usage_error() {
    let f = Fx::new("nokey");
    let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", "hk"]);
    assert!(o.status.success(), "{}", text(&o));
    let before = f.registry();
    let r = f.c(&["state", "release-task", "--run", "r1"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r));
    assert_eq!(f.registry(), before);
    let c = f.c(&["state", "claim-task", "--run", "r2"]);
    assert_eq!(c.status.code(), Some(2), "{}", text(&c));
}

/// Control: a non-blank key with surrounding whitespace keeps its identity
/// (passed through verbatim, not trimmed) and round-trips claim/is-claimed/release.
#[test]
fn nonblank_keys_still_round_trip_verbatim() {
    let f = Fx::new("ctl");
    let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", " k "]);
    assert!(o.status.success(), "{}", text(&o));
    let trimmed = f.c(&["state", "is-claimed", "--hashkey", "k"]);
    assert_eq!(trimmed.status.code(), Some(1), "{}", text(&trimmed));
    let exact = f.c(&["state", "is-claimed", "--hashkey", " k "]);
    assert_eq!(exact.status.code(), Some(0), "{}", text(&exact));
    let r = f.c(&["state", "release-task", "--run", "r1", "--hashkey", " k "]);
    assert!(r.status.success(), "{}", text(&r));
    let after = f.c(&["state", "is-claimed", "--hashkey", " k "]);
    assert_eq!(after.status.code(), Some(1), "{}", text(&after));
}
