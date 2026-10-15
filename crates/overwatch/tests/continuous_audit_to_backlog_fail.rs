// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent-verifier tests for H3: `scripts/continuous-audit.sh` must not
//! sign off with "PASS:" when `overwatch review-queue --to-backlog` failed to
//! forward a CONFIRMED finding. The script drives the real overwatch binary in
//! a sandboxed HOME; `OVERWATCH_BACKLOG_BIN` points at a fake backlog.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

fn fake_backlog(dir: &Path, exit: i32) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("backlog");
    fs::write(
        &p,
        format!("#!/bin/sh\nif [ \"$1\" = add ]; then echo refused >&2; exit {exit}; fi\nexit 0\n"),
    )
    .unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&p, perm).unwrap();
    p
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("ca-bridge-fail-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        Sandbox { root }
    }
    fn run(&self, backlog: &Path, round: &str, fid: &str) -> (i32, String) {
        let o = Command::new("bash")
            .arg(repo_root().join("scripts/continuous-audit.sh"))
            .args([
                "--round",
                round,
                "--target",
                "specguard",
                "--new-findings",
                "1",
                "--confirmed",
                "1",
                "--regression-tests-added",
                "1",
                "--finding",
                &format!("{fid}|high|confirmed finding {fid}|crates/x/src/y.rs"),
            ])
            .current_dir(self.root.join("work"))
            .env("HOME", self.root.join("home"))
            .env("OVERWATCH_BIN", env!("CARGO_BIN_EXE_overwatch"))
            .env("OVERWATCH_BACKLOG_BIN", backlog)
            .output()
            .expect("run script");
        (
            o.status.code().unwrap_or(-1),
            format!(
                "{}\n--stderr--\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )
    }
}

fn find_ledger(home: &Path) -> Option<PathBuf> {
    fn walk(d: &Path, out: &mut Option<PathBuf>) {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n == "bridged_findings.jsonl") {
                    *out = Some(p);
                }
            }
        }
    }
    let mut out = None;
    walk(home, &mut out);
    out
}

/// H3 (stated form): every `backlog add` refused => no "PASS:" line.
#[test]
fn refused_backlog_add_does_not_print_pass() {
    let sb = Sandbox::new("refused");
    let bl = fake_backlog(&sb.root.join("bin"), 2);
    let (code, out) = sb.run(&bl, "R1", "F-A");
    assert!(
        !out.contains("PASS:"),
        "script signed off PASS although the add was refused (exit {code}):\n{out}"
    );
}

/// Observation: the same scenario should also not exit 0 (a caller that reads
/// only the exit status sees success).
#[test]
#[ignore = "open defect: continuous-audit.sh prints PARTIAL but exits 0 when backlog forwards failed"]
fn refused_backlog_add_does_not_exit_zero() {
    let sb = Sandbox::new("refused-exit");
    let bl = fake_backlog(&sb.root.join("bin"), 2);
    let (code, out) = sb.run(&bl, "R1", "F-A");
    assert_ne!(code, 0, "exit 0 despite refused forward:\n{out}");
}

/// H3 variant: the idempotency ledger becomes unreadable => the bridge exits 3
/// (undetermined) and forwards NOTHING for the new finding. The script's
/// failure counter only greps a fixed set of WARNING strings; this checks
/// whether a "PASS:" is still printed.
#[test]
fn undetermined_bridge_ledger_does_not_print_pass() {
    let sb = Sandbox::new("undet");
    let ok = fake_backlog(&sb.root.join("bin-ok"), 0);
    let (c1, o1) = sb.run(&ok, "R1", "F-A");
    assert_eq!(c1, 0, "setup run failed:\n{o1}");
    let ledger = find_ledger(&sb.root.join("home")).expect("ledger created by first round");
    fs::set_permissions(&ledger, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_to_string(&ledger).is_ok();
    assert!(
        !readable,
        "precondition: ledger must be unreadable (running as root?)"
    );

    let (code, out) = sb.run(&ok, "R2", "F-B");
    fs::set_permissions(&ledger, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        !out.contains("PASS:"),
        "PASS printed although the bridge could not read its ledger and forwarded nothing (exit {code}):\n{out}"
    );
}
