// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 0805b07a (the check-oracle half): `condukt state
//! check-oracle --help` says that "no `tdd` on PATH ... degrade[s] to
//! `fallback:true`", but with no `tdd` reachable the command prints
//! `fallback:false`. The behaviour (fail-closed) is the correct side; the
//! defect is that the help text promises the opposite (CLAUDE.md §4: a
//! docstring claiming behaviour the code lacks).
//!
//! The test drives the real binary with a sandboxed `$HOME` (no plugin cache)
//! and a `$PATH` that contains no `tdd`, observes the real output, and then
//! requires the help text not to claim the contrary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

impl Fixture {
    fn new() -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-0805b07a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            // No `tdd` anywhere on this PATH.
            .env("PATH", "/usr/bin:/bin")
            .env("CLAUDE_CODE_SESSION_ID", "sess-0805b07a")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }
}

#[test]
fn check_oracle_help_matches_behaviour_when_tdd_is_missing() {
    let fx = Fixture::new();
    assert!(
        !Path::new("/usr/bin/tdd").exists() && !Path::new("/bin/tdd").exists(),
        "fixture: tdd must be absent from the test PATH"
    );
    let dec = fx.base.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"fix x","kind":"fix","reproduction_tests":"cargo test x","touched_files":["x.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    let out = fx.condukt(&[
        "state",
        "init",
        "--run",
        "r1",
        "--file",
        dec.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "init: {out:?}");

    let out = fx.condukt(&["state", "check-oracle", "--run", "r1", "--task", "t1"]);
    assert!(out.status.success(), "check-oracle: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Observed behaviour: a missing tdd does NOT degrade to fallback:true.
    assert_eq!(v["fallback"], false, "observed: {v}");

    let help = fx.condukt(&["state", "check-oracle", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout).replace('\n', " ");
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !help.contains("no `tdd` on PATH"),
        "check-oracle --help claims a missing tdd degrades to fallback:true, but \
         the command printed {v} — help: {help}"
    );
}
