#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Slow-helper contract of `backlog add`'s cross-session claim check (backlog
//! 420f1eec, round 3). On macOS the first exec of a freshly written binary takes
//! 382-752ms (OS scanning), so a helper that ANSWERS CORRECTLY but slowly must
//! not be refused; only a helper that never answers may be. The shims here are
//! deliberately NOT warmed (warming is what hid this defect in
//! claim_check_contract.rs), with one exception:
//! `exit_then_pipe_held_open_shares_one_deadline` warms its shim, because it
//! must reach the exit-then-read path before the deadline (its own doc says why).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TITLE: &str = "claim slow helper probe";

struct Fx {
    home: PathBuf,
    linked: PathBuf,
    shim: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

fn fx(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!(
        "backlog-claimslow-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let home = root.join("home");
    let main = root.join("main");
    let linked = root.join("linked");
    let shim = root.join("shim");
    for d in [&home, &main, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "t@t.t"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );
    Fx { home, linked, shim }
}

impl Fx {
    /// Written but NOT warmed.
    fn write_shim(&self, body: &str) {
        let p = self.shim.join("condukt");
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn add(&self, extra: &[&str]) -> (i32, String, String, Duration) {
        let proj = self.linked.to_str().unwrap();
        let mut args = vec!["add", "--title", TITLE, "--project", proj];
        args.extend_from_slice(extra);
        let t0 = Instant::now();
        let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .args(&args)
            .env("HOME", &self.home)
            // Shim + minimal system dirs only: an inherited PATH could hold a
            // real `condukt` that answers in the shim's place.
            .env("PATH", common::isolated_path_with(&self.shim))
            .current_dir(&self.linked)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            t0.elapsed(),
        )
    }

    fn stored(&self) -> String {
        let mut s = String::new();
        if let Ok(rd) = std::fs::read_dir(self.linked.join(".backlog")) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) == Some("toml") {
                    s.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
                }
            }
        }
        s
    }
}

const SLOW_ANSWER: &str = r#"sleep 0.6
echo '{"hashkey":"x","claimed":false,"holder_run":null}'
exit 1"#;

#[test]
fn slow_but_correct_helper_600ms_is_not_refused() {
    let f = fx("slow");
    f.write_shim(SLOW_ANSWER);
    let (rc, out, err, _) = f.add(&[]);
    assert_eq!(
        rc, 0,
        "a correct answer after ~600ms must not be refused; out={out:?} err={err:?}"
    );
    assert!(
        f.stored().contains(TITLE),
        "task must be stored; err={err:?}"
    );
}

#[test]
fn never_answering_helper_is_refused_within_bounded_time() {
    let f = fx("hang");
    f.write_shim("sleep 30\necho '{\"claimed\":false}'\nexit 1");
    let (rc, out, err, took) = f.add(&[]);
    assert_ne!(rc, 0, "a hung helper must refuse; out={out:?} err={err:?}");
    assert!(
        err.contains("timed out"),
        "stderr must say timed out; err={err:?}"
    );
    assert!(!f.stored().contains(TITLE), "task must NOT be stored");
    assert!(
        took < Duration::from_secs(10),
        "add must return in bounded time, took {took:?}"
    );
}

#[test]
fn force_bypasses_a_never_answering_helper() {
    let f = fx("hangforce");
    f.write_shim("sleep 30\necho '{\"claimed\":false}'\nexit 1");
    let (rc, out, err, took) = f.add(&["--force"]);
    assert_eq!(rc, 0, "--force must add; out={out:?} err={err:?}");
    assert!(f.stored().contains(TITLE), "task must be stored");
    assert!(
        took < Duration::from_secs(10),
        "--force add must return in bounded time, took {took:?}"
    );
}

// ---- round 4 (backlog 420f1eec): pin the TOTAL-deadline and the absolute bound.
//
// Hard numbers on purpose. TASKS_LOCK_STALE_SECS is 10s and the bound may hold
// the tasks lock for at most a quarter of it, so a never-answering helper must
// be refused in well under 4s wall time (spawn + 2.5s bound + process startup).
// These are NOT relative to IS_CLAIMED_TIMEOUT: raising that constant to 5s
// must turn them RED.

/// Absolute pin: a helper that never answers is refused in < 4s of wall time.
/// `exec` so the kill lands on the sleeper itself and nothing is orphaned.
#[test]
fn never_answering_helper_is_refused_in_under_4s_absolute() {
    let f = fx("abs4");
    f.write_shim("exec sleep 12");
    let (rc, out, err, took) = f.add(&[]);
    assert_ne!(rc, 0, "a hung helper must refuse; out={out:?} err={err:?}");
    assert!(
        err.contains("timed out"),
        "stderr must say timed out; err={err:?}"
    );
    assert!(!f.stored().contains(TITLE), "task must NOT be stored");
    assert!(
        took < Duration::from_secs(4),
        "the claim check holds the tasks lock; a never-answering helper must be \
         refused in < 4s (a quarter of the 10s stale-reap window plus startup), took {took:?}"
    );
}

/// Shared-deadline pin. The helper exits (code 1, no parseable body) after
/// ~1.5s but leaves a backgrounded grandchild holding its stdout open. The
/// deadline clock starts BEFORE spawn, so with ONE deadline for exit-wait +
/// read the call returns at ~2.5s (the read gets the ~1.0s left); with
/// separate waits (read gets a full fresh 2.5s after the exit) it takes
/// ~1.5s + 2.5s = ~4.0s while the tasks lock is held. Limit 3.2s leaves ~0.7s
/// margin above the shared case and ~0.8s below the separate case.
///
/// The shim is WARMED first (`warm` arg exits at once) so the cold-exec cost
/// (382-752ms) cannot push the helper's exit past the 2.5s bound and silently
/// route the call through the timeout path (which let the separate-wait mutant
/// survive). The test also asserts the refusal reason came from the read path
/// ("without a parseable claimed:false") and NOT from "timed out", so a run that
/// accidentally takes the timeout path fails loudly. The grandchild sleeps 9s
/// (outlives the separate-wait total) and has stderr/stdin redirected away so
/// it holds ONLY the claim-check pipe, never the test harness's pipes.
#[test]
fn exit_then_pipe_held_open_shares_one_deadline() {
    let f = fx("shared");
    f.write_shim(
        "[ \"$1\" = warm ] && exit 0\nsleep 1.5\n(exec sleep 9 </dev/null 2>/dev/null) &\nexit 1",
    );
    let warm = Command::new(f.shim.join("condukt"))
        .arg("warm")
        .output()
        .expect("warm run");
    assert!(warm.status.success(), "shim warm-up failed: {warm:?}");
    let (rc, out, err, took) = f.add(&[]);
    assert_ne!(
        rc, 0,
        "no parseable body after exit 1 is Undetermined and must refuse; out={out:?} err={err:?}"
    );
    assert!(
        err.contains("without a parseable claimed:false"),
        "refusal must come from the exit-then-read path; err={err:?}"
    );
    assert!(
        !err.contains("timed out"),
        "helper exits at ~1.5s, well inside the bound; a timeout means the exit-then-read \
         path was not exercised. err={err:?} took={took:?}"
    );
    assert!(!f.stored().contains(TITLE), "task must NOT be stored");
    assert!(
        took < Duration::from_millis(3200),
        "exit-wait and stdout-read must share ONE 2.5s deadline (~2.5s); separate \
         waits take ~4.0s. took {took:?} err={err:?}"
    );
    assert!(
        took >= Duration::from_millis(1400),
        "shim sleeps 1.5s before exiting; returning earlier means the shim did not run as designed. took {took:?}"
    );
}
