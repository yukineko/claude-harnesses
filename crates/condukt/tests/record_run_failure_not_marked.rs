// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt state record-run --all` must not mark a run recorded when a
//! fugu-router call failed, must not re-emit episodes that already landed,
//! and must treat a failing `fingerprint` as a failure. HOME is isolated and
//! fugu-router is a stub (argv log + controllable exit codes via marker
//! files), so the user's real `~/.fugu-router` is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

struct Env {
    _tmp: tempfile::TempDir,
    cwd: PathBuf,
    home: PathBuf,
    ctl: PathBuf,
    log: PathBuf,
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
            log,
            path_env,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
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

    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn count(&self, title: &str, role: &str) -> usize {
        self.log_lines()
            .iter()
            .filter(|l| {
                l.starts_with("record")
                    && l.contains(&format!("--title {title} "))
                    && l.contains(&format!("--role {role}"))
            })
            .count()
    }

    /// Create a settled run: t1 verified, optionally with a verifier model.
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

#[test]
fn failed_verifier_record_leaves_run_unrecorded_and_fails_loudly() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    env.flag("fail-verifier", true);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert!(
        env.recorded_at(&rid).is_null(),
        "recorded_at must stay null after a failed record call; stdout={out} stderr={err}"
    );
    assert_ne!(
        code, 0,
        "record-run must exit non-zero on a failed record; stderr={err}"
    );
    assert!(
        err.contains(&rid),
        "stderr must name the run id {rid}: {err}"
    );
}

#[test]
fn retry_after_partial_failure_does_not_re_emit_landed_episodes() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    env.flag("fail-verifier", true);
    let _ = env.run(&["state", "record-run", "--all"]);
    env.flag("fail-verifier", false);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert_eq!(
        code, 0,
        "retry with healthy fugu-router must succeed: {out} {err}"
    );
    assert!(
        !env.recorded_at(&rid).is_null(),
        "recorded_at must be set after the successful retry"
    );
    assert_eq!(
        env.count("TaskOne", "worker"),
        1,
        "worker episode already landed on run 1 and must not be re-emitted; log={:?}",
        env.log_lines()
    );
    assert_eq!(
        env.count("TaskOne", "verifier"),
        1,
        "verifier episode must be emitted exactly once (the successful retry); log={:?}",
        env.log_lines()
    );
}

#[test]
fn failing_fingerprint_marks_nothing_and_exits_nonzero() {
    let env = Env::new(true);
    let rid = env.settled_run(false);
    env.flag("fail-fingerprint", true);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert!(
        env.recorded_at(&rid).is_null(),
        "no run may be marked recorded when fingerprint fails; stdout={out} stderr={err}"
    );
    assert_ne!(
        code, 0,
        "record-run must exit non-zero when fingerprint fails; stderr={err}"
    );
}

#[test]
fn all_success_still_marks_recorded_and_exits_zero() {
    let env = Env::new(true);
    let rid = env.settled_run(true);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(!env.recorded_at(&rid).is_null(), "recorded_at must be set");
    assert_eq!(env.count("TaskOne", "worker"), 1);
    assert_eq!(env.count("TaskOne", "verifier"), 1);
}

#[test]
fn missing_fugu_router_is_a_soft_noop() {
    let env = Env::new(false);
    let rid = env.settled_run(true);
    let (code, out, err) = env.run(&["state", "record-run", "--all"]);
    assert_eq!(
        code, 0,
        "unresolvable fugu-router stays a soft no-op: {out} {err}"
    );
    assert!(
        env.recorded_at(&rid).is_null(),
        "recorded_at must stay null"
    );
}
