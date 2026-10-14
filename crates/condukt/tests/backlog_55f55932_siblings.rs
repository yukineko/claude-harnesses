// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 55f55932 siblings: an empty / whitespace-only `--hashkey` must be
//! rejected as INVALID INPUT by every task-claim subcommand, never read as a
//! real key. `claim-task ""` is pinned in `backlog_audit_s01.rs`; these cover
//! the whitespace-only claim, `release-task`, and `is-claimed`.
//!
//! `is-claimed` exit codes are a three-valued contract consumed by `backlog
//! add` and the flow SKILL: 0 = claimed, 1 = not claimed (with
//! `"claimed": false` on stdout), 3 = registry unreadable. An invalid key must
//! use none of 0/1 — it exits 2 (clap usage error) with nothing on stdout, so
//! it can never be read as "not claimed".

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

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-55f55932-{}-{tag}", std::process::id()));
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
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The live registry as JSON, via `state claims`.
fn claims(f: &Fx) -> serde_json::Value {
    let o = f.c(&["state", "claims"]);
    assert!(o.status.success(), "state claims failed: {}", text(&o));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn task_claim_keys(f: &Fx) -> Vec<String> {
    let v = claims(f);
    // Strict: a missing/renamed field must fail loudly, not read as "no claims".
    v.get("task_claims")
        .and_then(|t| t.as_object())
        .unwrap_or_else(|| panic!("`state claims` has no task_claims object: {v}"))
        .keys()
        .cloned()
        .collect()
}

#[test]
fn claim_task_rejects_whitespace_only_hashkey_and_writes_nothing() {
    let f = Fx::new("ws-claim");
    let o = f.c(&["state", "claim-task", "--run", "r1", "--hashkey", "   "]);
    assert!(
        !o.status.success(),
        "claim-task with a whitespace-only hashkey must fail: {}",
        text(&o)
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("hashkey"),
        "diagnostic must name the hashkey: {}",
        text(&o)
    );
    assert!(
        task_claim_keys(&f).is_empty(),
        "no claim entry may be written: {:?}",
        task_claim_keys(&f)
    );
}

#[test]
fn claim_task_with_a_blank_among_valid_keys_claims_nothing() {
    let f = Fx::new("mixed-claim");
    let o = f.c(&[
        "state",
        "claim-task",
        "--run",
        "r1",
        "--hashkey",
        "hk-real",
        "--hashkey",
        "",
    ]);
    assert!(!o.status.success(), "must fail: {}", text(&o));
    assert!(
        task_claim_keys(&f).is_empty(),
        "an invalid batch must not partially claim: {:?}",
        task_claim_keys(&f)
    );
}

#[test]
fn release_task_rejects_empty_hashkey() {
    let f = Fx::new("rel");
    let o = f.c(&["state", "release-task", "--run", "r1", "--hashkey", ""]);
    assert!(
        !o.status.success(),
        "release-task with an empty hashkey must fail: {}",
        text(&o)
    );
    let o = f.c(&["state", "release-task", "--run", "r1", "--hashkey", " \t"]);
    assert!(
        !o.status.success(),
        "release-task with a whitespace-only hashkey must fail: {}",
        text(&o)
    );
}

#[test]
fn is_claimed_rejects_empty_hashkey_with_a_code_distinct_from_not_claimed() {
    let f = Fx::new("isc");
    for key in ["", "  "] {
        let o = f.c(&["state", "is-claimed", "--hashkey", key]);
        let code = o.status.code();
        assert_eq!(
            code,
            Some(2),
            "is-claimed {key:?} must exit 2 (invalid input), distinct from 0/1/3: {}",
            text(&o)
        );
        assert!(
            o.stdout.is_empty(),
            "invalid input must print no claimed:false JSON on stdout: {}",
            text(&o)
        );
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("hashkey"),
            "diagnostic must name the hashkey: {}",
            text(&o)
        );
    }
}

/// Control: a real key still answers "not claimed" with exit 1.
#[test]
fn is_claimed_real_key_still_answers_not_claimed() {
    let f = Fx::new("isc-ctl");
    let o = f.c(&["state", "is-claimed", "--hashkey", "hk-free"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"claimed\": false"));
}

/// The empty SET of hashkeys is the same defect one level up: `claim-task`
/// with no `--hashkey` at all must not exit 0 with `"claimed": []`.
#[test]
fn claim_and_release_require_at_least_one_hashkey() {
    let f = Fx::new("nokey");
    let o = f.c(&["state", "claim-task", "--run", "r1"]);
    assert!(
        !o.status.success(),
        "claim-task with no --hashkey must fail: {}",
        text(&o)
    );
    let o = f.c(&["state", "release-task", "--run", "r1"]);
    assert!(
        !o.status.success(),
        "release-task with no --hashkey must fail: {}",
        text(&o)
    );
}
