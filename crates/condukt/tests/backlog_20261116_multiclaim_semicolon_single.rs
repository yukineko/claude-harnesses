// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (observed 2026-11-16, run run-20261116-143410-12142, ~main
//! fa58e3698): `condukt state check-criteria` reported
//! `"coverage":{"kind":"single"}` and `skip_verifier:true` for
//!
//!   "`cargo test -p blastguard --test X -- --include-ignored` exits 0 with
//!    the test un-ignored; no other blastguard test regresses; version bumped."
//!
//! Three `;`-separated claims; only the first is mechanical. The coverage
//! classifier (verify.rs `criteria_coverage_for`) counts enumeration markers
//! and commands; plain `;` separation is evidently not counted.
//!
//! Hermetic: a shim `cargo` (exit 0) is prepended to PATH so no real build
//! runs; the verdict path is then the green-mechanical one.
//!
//! Pinned (RED): coverage.kind != "single" and skip_verifier != true.
//! Control (not ignored): the same command alone as the whole criteria IS
//! single (so the RED is about the extra claims, not the fixture).
//!
//! Reading (a) defect: multi-claim criteria must veto the skip. Reading (b)
//! intended: `;`-clauses are treated as prose of one claim. Tell apart via
//! verify.rs doc on `CriteriaCoverage` ("does this text assert more than one
//! thing?") which favours (a).
//!
//! Does NOT prove: other separators (`、`, `and`), or which counter should
//! detect it (Partial vs Undecidable both satisfy).
//!
//! Written by an independent test author, not the implementer.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const MULTI: &str = "`cargo test -p blastguard --test X -- --include-ignored` exits 0 with the test un-ignored; no other blastguard test regresses; version bumped.";
const SINGLE: &str =
    "`cargo test -p blastguard --test X -- --include-ignored` exits 0 with the test un-ignored.";

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

struct Obs {
    json: serde_json::Value,
    out: Output,
}

fn observe(tag: &str, criteria: &str) -> Obs {
    use std::os::unix::fs::PermissionsExt;
    let base: PathBuf =
        std::env::temp_dir().join(format!("condukt-multiclaim-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    let shim = base.join("shim");
    let wt = base.join("wt");
    for d in [&repo, &home, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    let cargo = shim.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "b\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condukt/mc",
            wt.to_str().unwrap(),
        ],
    );

    let path = format!("{}:/usr/bin:/bin", shim.display());
    let run = |args: &[&str]| -> Output {
        common::prime_if_running(args, &home, "sess-mc");
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("PATH", &path)
            .env("CONDUKT_TESTSCOPE_LEDGER", common::testscope_ledger(&home))
            .env("CLAUDE_CODE_SESSION_ID", "sess-mc")
            .env("CONDUKT_AUTONOMOUS", "0")
            .env_remove("CONDUKT_DISABLE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("spawn condukt")
    };
    let dec = base.join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({
            "goal": "g",
            "tasks": [{
                "id": "t1", "title": "t", "touched_files": ["work.txt"],
                "deps": [], "class": "parallel",
                "done_criteria": criteria,
                "is_behavioral": false
            }]
        })
        .to_string(),
    )
    .unwrap();
    for args in [
        vec![
            "state",
            "init",
            "--run",
            "r1",
            "--file",
            dec.to_str().unwrap(),
        ],
        vec![
            "state",
            "set",
            "--run",
            "r1",
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            wt.to_str().unwrap(),
        ],
    ] {
        let o = run(&args);
        assert!(o.status.success(), "condukt {args:?} failed: {o:?}");
    }
    let out = run(&["state", "check-criteria", "--run", "r1", "--task", "t1"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .next_back()
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout} / {out:?}"));
    let _ = std::fs::remove_dir_all(&base);
    Obs { json, out }
}

#[test]
fn control_single_claim_command_is_single_coverage() {
    let o = observe("ctl", SINGLE);
    assert_eq!(
        o.json["coverage"]["kind"],
        serde_json::json!("single"),
        "control: a lone command criteria is single: {} / {:?}",
        o.json,
        o.out
    );
}

#[test]
#[ignore = "RED: three `;`-separated claims (only the first mechanical) classified coverage=single and skip_verifier=true"]
fn semicolon_separated_multi_claim_criteria_is_not_single_and_does_not_skip_verifier() {
    let o = observe("multi", MULTI);
    assert_ne!(
        o.json["coverage"]["kind"],
        serde_json::json!("single"),
        "3 claims classified single: {}",
        o.json
    );
    assert_ne!(
        o.json["skip_verifier"],
        serde_json::json!(true),
        "skip_verifier true although 2 of 3 claims are unchecked: {}",
        o.json
    );
}
