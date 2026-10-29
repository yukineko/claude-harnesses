// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Quiet-stop item 2: `condukt state record-run --all --stop-hook` (the Stop
//! hook form) exits 0 with EMPTY stdout and stderr even when a fugu-router
//! record call fails; the failure is appended to
//! `<state_dir>/record-run-failures.log`, and the run stays unrecorded.
//! Without `--stop-hook` the CLI still exits non-zero (control).
//! Fixture mirrors tests/record_run_failure_not_marked.rs (HOME isolated,
//! fugu-router stub on a PATH of its own).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

struct Env {
    _tmp: tempfile::TempDir,
    cwd: PathBuf,
    home: PathBuf,
    ctl: PathBuf,
    path_env: std::ffi::OsString,
}

impl Env {
    fn new(with_stub: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let home = tmp.path().join("home");
        let bin_dir = tmp.path().join("bin");
        let ctl = tmp.path().join("ctl");
        for d in [&cwd, &home, &bin_dir, &ctl] {
            std::fs::create_dir_all(d).unwrap();
        }
        let log = tmp.path().join("fugu.log");
        if with_stub {
            // Marker files in ctl dir: fail-verifier, fail-fingerprint.
            // Only calls that SUCCEED are logged as `record ...` (a landed
            // episode); failed ones are logged as `FAILED record ...`.
            let script = format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = fingerprint ]; then\n\
                   [ -f '{ctl}/fail-fingerprint' ] && exit 3\n\
                   echo fp123; exit 0\n\
                 fi\n\
                 case \"$*\" in *'--role verifier'*) if [ -f '{ctl}/fail-verifier' ]; then echo \"FAILED $*\" >> '{log}'; exit 1; fi;; esac\n\
                 echo \"$*\" >> '{log}'\n\
                 exit 0\n",
                log = log.display(),
                ctl = ctl.display()
            );
            let p = bin_dir.join("fugu-router");
            std::fs::write(&p, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // PATH is ONLY the stub dir: no real fugu-router can be found.
        let path_env = std::env::join_paths([bin_dir]).unwrap();
        Env {
            _tmp: tmp,
            cwd,
            home,
            ctl,
            path_env,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = harness_core::git_env::scrub(&mut Command::new(BIN))
            .args(args)
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env("PATH", &self.path_env)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn flag(&self, name: &str, on: bool) {
        let p = self.ctl.join(name);
        if on {
            std::fs::write(&p, "").unwrap();
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }

    fn settled_run(&self, with_verifier: bool) -> String {
        let dec = serde_json::json!({
            "goal": "g",
            "tasks": [{"id": "t1", "title": "TaskOne", "touched_files": []}]
        });
        let dp = self.cwd.join("dec.json");
        std::fs::write(&dp, dec.to_string()).unwrap();
        let (c, o, e) = self.run(&["state", "init", "--file", dp.to_str().unwrap()]);
        assert_eq!(c, 0, "init: {o} {e}");
        let rid = o
            .lines()
            .chain(e.lines())
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("run-"))
            .expect("run id")
            .to_string();
        let (c, o, e) = self.run(&[
            "state", "set", "--run", &rid, "--task", "t1", "--status", "running",
        ]);
        assert_eq!(c, 0, "running: {o} {e}");
        let mut a = vec![
            "state", "set", "--run", &rid, "--task", "t1", "--status", "verified", "--model",
            "sonnet", "--cost", "0",
        ];
        if with_verifier {
            a.extend(["--verifier-model", "opus", "--verifier-cost", "0"]);
        }
        let (c, o, e) = self.run(&a);
        assert_eq!(c, 0, "verified: {o} {e}");
        rid
    }

    fn recorded_at(&self, rid: &str) -> serde_json::Value {
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
        let name = format!("{rid}.json");
        let p = find(&self.home, &name)
            .or_else(|| find(&self.cwd, &name))
            .unwrap_or_else(|| panic!("state json for {rid} not found"));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v["recorded_at"].clone()
    }
}

fn failures_log(env: &Env) -> String {
    std::fs::read_to_string(env.home.join(".condukt/state/record-run-failures.log"))
        .unwrap_or_default()
}

#[test]
fn control_without_stop_hook_failure_still_exits_nonzero() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    env.flag("fail-verifier", true);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert_ne!(code, 0, "CLI form must still fail loudly: {out} {err}");
    assert!(err.contains(&rid), "stderr names the run: {err}");
}

#[test]
fn stop_hook_failure_is_silent_exit0_and_logged() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    env.flag("fail-verifier", true);
    let (code, out, err) = env.run(&["state", "record-run", "--all", "--stop-hook"]);
    let log = failures_log(&env);
    assert_eq!(code, 0, "stop-hook mode exits 0; stdout={out} stderr={err}");
    assert!(out.trim().is_empty(), "stdout must be empty: {out:?}");
    assert!(err.trim().is_empty(), "stderr must be empty: {err:?}");
    assert!(
        log.contains("fugu-router record failed"),
        "failure must be appended to record-run-failures.log, got: {log:?}"
    );
    assert!(
        env.recorded_at(&rid).is_null(),
        "failed run must stay unrecorded (retry next time)"
    );
}

#[test]
fn stop_hook_success_is_silent_too() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    let (code, out, err) = env.run(&["state", "record-run", "--all", "--stop-hook"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.trim().is_empty(), "stdout must be empty: {out:?}");
    assert!(
        err.trim().is_empty(),
        "progress info must go to the log, not stderr: {err:?}"
    );
    assert!(!env.recorded_at(&rid).is_null(), "run recorded");
}
