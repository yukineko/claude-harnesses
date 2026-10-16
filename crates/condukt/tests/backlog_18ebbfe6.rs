// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 18ebbfe6: `condukt state record-run` resolves a
//! task's real cost/tokens from `gauge subagents --json`. When gauge exits 0
//! but prints MALFORMED JSON, `parse_agent_cost` / `parse_agent_tokens` map
//! that to `None` ("no matching id" = no data), so the episode is recorded to
//! fugu-router with the MANUAL `--cost` instead of being left unrecorded as
//! undetermined. A present-but-broken source is not "no data".
//!
//! Fixture: real binary, sandboxed `$HOME` (no plugin cache), `$PATH` with a
//! fake `gauge` and a fake `fugu-router`. The fake fugu-router appends each
//! `record` invocation's argv to a log file.
//!
//! * RED: gauge prints `{not json` (exit 0) -> no episode may be recorded.
//! * control: gauge prints `[]` (valid, no matching id) -> the manual cost IS
//!   recorded (legitimate "no data" fallback), proving the fakes are wired.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    fakebin: PathBuf,
    log: PathBuf,
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

fn write_exec(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    fn new(tag: &str, gauge_stdout: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-18ebbfe6-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let fakebin = base.join("fakebin");
        let log = base.join("fugu-record.log");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&fakebin).unwrap();
        write_exec(
            &fakebin.join("gauge"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"subagents\" ]; then printf '%s' '{gauge_stdout}'; fi\nexit 0\n"
            ),
        );
        write_exec(
            &fakebin.join("fugu-router"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  fingerprint) echo fp-test ;;\n  record) echo \"$*\" >> '{}' ;;\nesac\nexit 0\n",
                log.display()
            ),
        );
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
            fakebin,
            log,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.fakebin.display()))
            .env("CLAUDE_CODE_SESSION_ID", "sess-18ebbfe6")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .expect("spawn condukt")
    }

    /// A settled (failed) single-task run carrying a Task-tool agent id and a
    /// manually recorded cost of 1.5.
    fn settled_run(&self, run: &str) {
        let dec = self.base.join("dec.json");
        std::fs::write(
            &dec,
            r#"{"goal":"g","tasks":[{"id":"t1","title":"edit x","touched_files":["x.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
        )
        .unwrap();
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "init: {out:?}");
        let out = self.condukt(&[
            "state",
            "set",
            "--run",
            run,
            "--task",
            "t1",
            "--status",
            "failed",
            "--model",
            "sonnet",
            "--agent-id",
            "agent-xyz",
            "--cost",
            "1.5",
        ]);
        assert!(out.status.success(), "set: {out:?}");
    }

    fn record_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[test]
fn malformed_gauge_json_is_not_recorded_as_manual_cost() {
    let fx = Fixture::new("red", "{not json");
    fx.settled_run("r1");
    let out = fx.condukt(&["state", "record-run", "--run", "r1"]);
    let log = fx.record_log();
    assert!(
        log.is_empty(),
        "gauge printed malformed JSON, yet an episode was recorded as if gauge \
         had answered 'no data' (manual cost used): log={log:?} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn control_valid_gauge_without_match_records_manual_cost() {
    let fx = Fixture::new("control", "[]");
    fx.settled_run("r2");
    let out = fx.condukt(&["state", "record-run", "--run", "r2"]);
    assert!(out.status.success(), "record-run: {out:?}");
    let log = fx.record_log();
    assert!(
        log.contains("--cost 1.5") && log.contains("--role worker"),
        "control: valid empty gauge output must fall back to the manual cost: {log:?}"
    );
}
