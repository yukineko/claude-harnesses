#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog finding 2026-11-17 (run-20261117-100335-22251 t3): a task with two
//! repro commands. `tdd green --task t3 --cmd A` followed by
//! `tdd green --task t3 --cmd B` overwrites `.tdd/t3.green.json` with B, so the
//! A fail-to-pass evidence is lost (the oracle then reports command_mismatch).
//! Expected: the first F->P evidence survives (green.json still records A), or
//! the second green is refused (non-zero exit).
//!
//! Written by an independent test author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    proj: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn fixture(tag: &str) -> Fx {
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("tdd-2slot-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["config", "user.email", "t@t.com"]);
    git(&proj, &["config", "user.name", "t"]);
    git(&proj, &["config", "commit.gpgsign", "false"]);
    std::fs::write(proj.join("seed.txt"), "seed\n").unwrap();
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-qm", "seed"]);
    Fx { base, proj, home }
}

fn run(fx: &Fx, args: &[&str]) -> (i32, String, String) {
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(&fx.proj)
        .env("HOME", &fx.home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env("CLAUDE_CODE_SESSION_ID", "s-2slot")
        .env_remove("TDD_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tdd spawns");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn green_cmd(fx: &Fx) -> String {
    let t = std::fs::read_to_string(fx.proj.join(".tdd/t3.green.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    v["cmd"].as_str().unwrap_or("").to_string()
}

const A: &str = "test -f a.txt";
const B: &str = "test -f b.txt";

/// Control: red A, green A records A, and the oracle accepts it.
#[test]
fn control_single_repro_oracle_valid() {
    let fx = fixture("ctl");
    let (c, _, e) = run(&fx, &["red", "--task", "t3", "--cmd", A]);
    assert_eq!(c, 0, "apparatus red: {e}");
    std::fs::write(fx.proj.join("a.txt"), "x").unwrap();
    let (c, _, e) = run(&fx, &["green", "--task", "t3", "--cmd", A]);
    assert_eq!(c, 0, "apparatus green: {e}");
    assert_eq!(green_cmd(&fx), A);
    let (c, o, e) = run(&fx, &["oracle", "--task", "t3"]);
    assert_eq!(c, 0, "oracle must be valid: {o} {e}");
}

#[test]
#[ignore = "RED: a second `tdd green --cmd B` on the same task silently overwrites the first green proof (A lost, oracle command_mismatch)"]
fn second_green_with_other_cmd_must_not_destroy_first_proof() {
    let fx = fixture("two");
    let (c, _, e) = run(&fx, &["red", "--task", "t3", "--cmd", A]);
    assert_eq!(c, 0, "apparatus red A: {e}");
    std::fs::write(fx.proj.join("a.txt"), "x").unwrap();
    let (c, _, e) = run(&fx, &["green", "--task", "t3", "--cmd", A]);
    assert_eq!(c, 0, "apparatus green A: {e}");
    assert_eq!(green_cmd(&fx), A);
    // Second repro passes too.
    std::fs::write(fx.proj.join("b.txt"), "x").unwrap();
    let (c2, o2, e2) = run(&fx, &["green", "--task", "t3", "--cmd", B]);
    let kept = green_cmd(&fx);
    let (oc, oo, _) = run(&fx, &["oracle", "--task", "t3"]);
    assert!(
        c2 != 0 || kept == A,
        "second green (exit {c2}) overwrote the first proof: green.json cmd is now {kept:?}, \
         expected {A:?} or a refusal. oracle exit={oc} {oo}\nstdout={o2}\nstderr={e2}"
    );
}
