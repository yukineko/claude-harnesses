#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt state is-claimed` exit-code contract (backlog 420f1eec, round 2):
//! 0 = claimed, 1 = not claimed, 3 = the claim registry cannot be read/parsed.
//! Exit 1 must never double as "could not check", because `backlog add` reads
//! exit 1 as "free to add".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    repo: PathBuf,
    home: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?} failed: {o:?}");
}

fn fx(tag: &str) -> Fx {
    let base = std::env::temp_dir().join(format!("condukt-isclaimed-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    Fx { repo, home }
}

impl Fx {
    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_condukt"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    }
    fn is_claimed(&self, hk: &str) -> Output {
        self.condukt(&["state", "is-claimed", "--hashkey", hk])
    }
    /// `<HOME>/.../<project-key>/claims.json` (claim.rs `claim_paths`), found by
    /// name under the sandboxed HOME rather than re-deriving the key.
    fn claims_json(&self) -> Option<PathBuf> {
        find(&self.home, "claims.json")
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

#[test]
fn clean_empty_registry_exits_1_with_claimed_false() {
    let f = fx("clean");
    let o = f.is_claimed("abc");
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {o:?}"));
    assert_eq!(v["claimed"], serde_json::Value::Bool(false));
}

#[test]
fn claimed_hashkey_exits_0_with_claimed_true() {
    let f = fx("claimed");
    let c = f.condukt(&["state", "claim-task", "--run", "r1", "--hashkey", "abc"]);
    assert!(c.status.success(), "precondition: claim-task: {c:?}");
    let o = f.is_claimed("abc");
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["claimed"], serde_json::Value::Bool(true));
}

#[test]
fn corrupt_registry_exits_3_not_1() {
    let f = fx("corrupt");
    let c = f.condukt(&["state", "claim-task", "--run", "r1", "--hashkey", "other"]);
    assert!(c.status.success(), "precondition: claim-task: {c:?}");
    let path = f
        .claims_json()
        .expect("claims.json must exist after a claim");
    std::fs::write(&path, "{garbage").unwrap();

    let o = f.is_claimed("abc");
    assert_eq!(
        o.status.code(),
        Some(3),
        "unreadable registry must be exit 3 (exit 1 reads as 'not claimed'): {o:?}"
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        !stdout.contains("\"claimed\": false") && !stdout.contains("\"claimed\":false"),
        "stdout must not assert claimed:false: {stdout}"
    );
    assert!(!o.stderr.is_empty(), "stderr must carry a reason");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{garbage",
        "the unreadable registry must not be overwritten"
    );
}
