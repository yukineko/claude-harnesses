// Integration test: unwrap/expect/panic are the failure channel here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tests of `support/bounded.rs::run_bounded` itself, against stub scripts.
//!
//! Every case runs `run_bounded` on a worker thread behind a watchdog
//! (`recv_timeout`), so a hang in the helper becomes a failed assertion here
//! and never blocks the test binary forever. Each stub ends itself (`sleep 120`
//! at most) and records its pids in a file so the test can `kill` leftovers.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

enum Outcome<T> {
    Returned(T),
    Panicked(String),
    TimedOut,
}

/// Run `f` on its own thread; give up waiting after `limit`.
fn watchdog<T: Send + 'static>(
    limit: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Outcome<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(r);
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(v)) => Outcome::Returned(v),
        Ok(Err(p)) => {
            let msg = if let Some(s) = p.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = p.downcast_ref::<&str>() {
                (*s).to_string()
            } else {
                "<non-string panic>".to_string()
            };
            Outcome::Panicked(msg)
        }
        Err(_) => Outcome::TimedOut,
    }
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bounded-helper-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `body` as an executable `sh` script and return a Command for it.
fn stub(dir: &Path, body: &str) -> Command {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("stub.sh");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(script)
}

fn pids_in(file: &Path, want: usize) -> Vec<u32> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let pids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
        if pids.len() >= want || Instant::now() >= until {
            return pids;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

fn kill(pid: u32) {
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Poll up to `limit` for `pid` to be gone.
fn dead_within(pid: u32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(pid)
}

#[test]
fn timeout_kills_the_child_and_its_descendants() {
    let dir = fresh_dir("group-kill");
    let pidfile = dir.join("pids");
    let mut cmd = stub(
        &dir,
        &format!(
            "echo $$ > {p}\nsleep 120 &\necho $! >> {p}\nwait",
            p = pidfile.display()
        ),
    );
    let outcome = watchdog(Duration::from_secs(30), move || {
        bounded::run_bounded(&mut cmd, b"", Duration::from_secs(2))
    });
    let pids = pids_in(&pidfile, 2);
    let both_dead = pids.len() == 2 && pids.iter().all(|p| dead_within(*p, Duration::from_secs(3)));
    for p in &pids {
        kill(*p);
    }
    assert!(
        matches!(outcome, Outcome::Panicked(_)) && both_dead,
        "after a timeout the helper must panic and leave neither the stub nor its sleep child alive; pids={pids:?}"
    );
}

#[test]
fn large_stdout_is_returned_in_full() {
    let dir = fresh_dir("large-out");
    let mut cmd = stub(&dir, "head -c 204800 /dev/zero");
    let outcome = watchdog(Duration::from_secs(10), move || {
        bounded::run_bounded(&mut cmd, b"", Duration::from_secs(5))
    });
    let len = match outcome {
        Outcome::Returned(o) => Some(o.stdout.len()),
        _ => None,
    };
    assert_eq!(len, Some(204_800));
}

#[test]
fn stdin_write_is_inside_the_deadline() {
    let dir = fresh_dir("stdin-deadline");
    let pidfile = dir.join("pids");
    let mut cmd = stub(
        &dir,
        &format!("echo $$ > {}\nexec sleep 120", pidfile.display()),
    );
    let payload = vec![b'x'; 1024 * 1024];
    let outcome = watchdog(Duration::from_secs(10), move || {
        bounded::run_bounded(&mut cmd, &payload, Duration::from_secs(2))
    });
    let timed_out = matches!(outcome, Outcome::TimedOut);
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    assert!(
        !timed_out,
        "run_bounded blocked past the 10s watchdog on a 1MB stdin write to a child that never reads (2s deadline)"
    );
}

#[test]
fn drain_join_is_bounded_when_a_grandchild_holds_stdout() {
    let dir = fresh_dir("drain-join");
    let pidfile = dir.join("pids");
    let mut cmd = stub(
        &dir,
        &format!("sleep 120 &\necho $! > {}\nexit 0", pidfile.display()),
    );
    let outcome = watchdog(Duration::from_secs(10), move || {
        bounded::run_bounded(&mut cmd, b"", Duration::from_secs(2))
    });
    let timed_out = matches!(outcome, Outcome::TimedOut);
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    assert!(
        !timed_out,
        "run_bounded blocked past the 10s watchdog joining the drain threads while a grandchild holds stdout"
    );
}

#[test]
fn stdin_write_failure_is_reported() {
    let dir = fresh_dir("stdin-fail");
    let mut cmd = stub(&dir, "exit 0");
    let payload = vec![b'x'; 1024 * 1024];
    let outcome = watchdog(Duration::from_secs(10), move || {
        bounded::run_bounded(&mut cmd, &payload, Duration::from_secs(5))
    });
    let msg = match outcome {
        Outcome::Panicked(m) => Some(m),
        _ => None,
    };
    assert!(
        msg.as_deref().is_some_and(|m| m.contains("stdin")),
        "expected a panic mentioning the stdin write failure, got {msg:?}"
    );
}
