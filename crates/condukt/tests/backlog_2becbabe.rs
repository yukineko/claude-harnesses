// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 2becbabe: `condukt state check-oracle` maps a `tdd
//! oracle` run that EXITS 0 but whose stdout is empty, non-JSON, or reports the
//! `"unknown"` transition to `fallback: true` (crates/condukt/src/oracle.rs
//! `verdict_from_oracle_output` / `verdict_from_oracle`), and
//! `state::enforce_fp_gate` turns `fallback: true` into Allow. Nothing was
//! established in any of those runs, so CLAUDE.md §3 requires the restrictive
//! answer (`fallback: false`, i.e. Reject), not a degrade.
//!
//! Latent today (the real `tdd` exits 1 for "unknown"), so the repro uses a
//! fake `tdd` on PATH that breaks the exit protocol, as the ticket asks.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    shim: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

impl Fixture {
    fn new(tag: &str, fake_tdd_body: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-2becbabe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let shim = base.join("shim");
        for d in [&repo, &home, &shim] {
            std::fs::create_dir_all(d).unwrap();
        }
        let tdd = shim.join("tdd");
        std::fs::write(&tdd, format!("#!/bin/sh\n{fake_tdd_body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tdd, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            base,
            repo,
            home,
            shim,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_condukt"))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.shim.display()))
            .env("CLAUDE_CODE_SESSION_ID", "sess-2becbabe")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// `state init` a single fix task and return `check-oracle`'s verdict.
    fn check_oracle(&self) -> serde_json::Value {
        let dec = self.base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"fix x","kind":"fix","reproduction_tests":"cargo test x","touched_files":["x.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            "r1",
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "init: {out:?}");
        let out = self.condukt(&["state", "check-oracle", "--run", "r1", "--task", "t1"]);
        assert!(out.status.success(), "check-oracle: {out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

/// CONTROL: the fake `tdd` on PATH is the one consulted — a valid F→P report
/// on exit 0 comes back as a real valid verdict.
#[test]
fn control_fake_tdd_is_consulted() {
    let fx = Fixture::new(
        "ctl",
        r#"echo '{"valid_fp_oracle":true,"transition":"fail_to_pass"}'; exit 0"#,
    );
    let v = fx.check_oracle();
    assert_eq!(v["valid_fp_oracle"], true, "control: {v}");
    assert_eq!(v["fallback"], false, "control: {v}");
}

fn assert_not_fallback(tag: &str, body: &str) {
    let fx = Fixture::new(tag, body);
    let v = fx.check_oracle();
    assert_eq!(
        v["fallback"], false,
        "a tdd oracle run that exited 0 without establishing a valid F→P oracle \
         ({tag}) must be Reject-shaped (fallback:false), not degrade to \
         fallback:true (= enforce_fp_gate Allow): {v}"
    );
}

#[test]
#[ignore = "backlog 2becbabe: RED repro"]
fn exit0_empty_stdout_is_not_fallback() {
    assert_not_fallback("empty", "exit 0");
}

#[test]
#[ignore = "backlog 2becbabe: RED repro"]
fn exit0_garbled_stdout_is_not_fallback() {
    assert_not_fallback("garbled", "echo 'not json {'; exit 0");
}

#[test]
#[ignore = "backlog 2becbabe: RED repro"]
fn exit0_unknown_transition_is_not_fallback() {
    assert_not_fallback(
        "unknown",
        r#"echo '{"valid_fp_oracle":false,"transition":"unknown"}'; exit 0"#,
    );
}
