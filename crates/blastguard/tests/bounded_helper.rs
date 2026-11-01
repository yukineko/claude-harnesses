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

/// Like [`stub`], but runs the script as `/bin/sh <script>` instead of executing
/// the freshly written file: the first exec of a new file can stall for seconds
/// under load, which eats into a deadline that starts at spawn.
fn sh_stub(dir: &Path, body: &str) -> Command {
    let script = dir.join("stub.sh");
    std::fs::write(&script, format!("{body}\n")).unwrap();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(script);
    cmd
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
    // The deadline runs from spawn, so it must outlast process-start latency
    // under load (the stub has to have written both pids before the timeout
    // for the test to see them): 6s tolerates seconds of latency, and the
    // result is judged on ranges, not on tight instants.
    let deadline = Duration::from_secs(6);
    let mut cmd = sh_stub(
        &dir,
        &format!(
            "echo $$ > {p}\nsleep 120 &\necho $! >> {p}\nwait",
            p = pidfile.display()
        ),
    );
    let (outcome, took) = {
        let started = Instant::now();
        let o = watchdog(Duration::from_secs(40), move || {
            bounded::run_bounded(&mut cmd, b"", deadline)
        });
        (o, started.elapsed())
    };
    let pids = pids_in(&pidfile, 2);
    let alive_after: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|p| !dead_within(*p, Duration::from_secs(10)))
        .collect();
    for p in &pids {
        kill(*p);
    }
    assert_eq!(
        pids.len(),
        2,
        "the stub never recorded both pids (stub and its sleep child) within the {deadline:?} deadline plus 5s: it did not start in time, so nothing was proven; pids={pids:?}"
    );
    let msg = match outcome {
        Outcome::Panicked(m) => m,
        Outcome::Returned(_) => {
            panic!("run_bounded returned instead of timing out a child that never exits")
        }
        Outcome::TimedOut => panic!("run_bounded hung past the 40s watchdog"),
    };
    assert!(
        msg.contains("timed out waiting for exit"),
        "the panic must name the exit-wait phase, got {msg:?}"
    );
    assert!(
        took >= deadline && took < deadline + Duration::from_secs(8),
        "the timeout must fire within [{deadline:?}, {deadline:?}+8s), took {took:?}"
    );
    assert!(
        alive_after.is_empty(),
        "after a timeout neither the stub nor its sleep child may stay alive; still alive: {alive_after:?} of {pids:?}"
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

/// Run `cmd` through `run_bounded` behind a watchdog; return the outcome and
/// how long it took.
fn timed_run(
    mut cmd: Command,
    payload: Vec<u8>,
    deadline: Duration,
    limit: Duration,
) -> (Outcome<std::process::Output>, Duration) {
    let started = Instant::now();
    let outcome = watchdog(limit, move || {
        bounded::run_bounded(&mut cmd, &payload, deadline)
    });
    (outcome, started.elapsed())
}

#[test]
fn slow_stdin_read_then_overrun_is_cut_at_the_deadline() {
    let dir = fresh_dir("slow-read-overrun");
    let pidfile = dir.join("pids");
    // Reads its 1MB stdin only after 2s (finishing inside the 3s deadline),
    // then keeps running far past it.
    let cmd = stub(
        &dir,
        &format!(
            "echo $$ > {}\nsleep 2\ncat > /dev/null\nsleep 120",
            pidfile.display()
        ),
    );
    let (outcome, took) = timed_run(
        cmd,
        vec![b'x'; 1024 * 1024],
        Duration::from_secs(3),
        Duration::from_secs(12),
    );
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    let msg = match outcome {
        Outcome::Panicked(m) => Some(m),
        _ => None,
    };
    assert!(
        msg.as_deref().is_some_and(|m| m.contains("timed out")),
        "expected a timeout panic for a child that overran the deadline after reading stdin, got {msg:?}"
    );
    assert!(
        took < Duration::from_secs(6),
        "the single 3s deadline was not honoured: run_bounded took {took:?}"
    );
}

#[test]
fn slow_stdin_read_plus_slow_exit_is_charged_to_one_deadline() {
    let dir = fresh_dir("slow-read-slow-exit");
    let pidfile = dir.join("pids");
    // 2s before reading stdin plus 2s more before exiting: each phase fits in
    // the 3s deadline, their sum (4s) does not.
    let cmd = stub(
        &dir,
        &format!(
            "echo $$ > {}\nsleep 2\ncat > /dev/null\nsleep 2\nexit 0",
            pidfile.display()
        ),
    );
    let (outcome, took) = timed_run(
        cmd,
        vec![b'x'; 1024 * 1024],
        Duration::from_secs(3),
        Duration::from_secs(12),
    );
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    let returned = matches!(outcome, Outcome::Returned(_));
    assert!(
        !returned,
        "run_bounded returned after {took:?}, past its single 3s deadline: phase time was not summed"
    );
}

#[test]
fn drain_after_a_long_wait_is_cut_at_the_one_deadline_not_a_fresh_one() {
    let dir = fresh_dir("slow-wait-slow-drain");
    let pidfile = dir.join("pids");
    // No stdin payload. The stub exits 3s after it starts (the deadline is 6s
    // from spawn, so process-start latency under load of up to ~3s is
    // tolerated) while a grandchild holds stdout open for 120s. Correct: cut
    // at ~6s while draining stdout, whatever the start latency. A drain given
    // a fresh 6s budget would be cut at start latency + 3s + 6s >= 9s.
    let cmd = sh_stub(
        &dir,
        &format!(
            "sleep 120 &\necho $! > {}\nsleep 3\nexit 0",
            pidfile.display()
        ),
    );
    let (outcome, took) = timed_run(
        cmd,
        Vec::new(),
        Duration::from_secs(6),
        Duration::from_secs(25),
    );
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    let msg = match outcome {
        Outcome::Panicked(m) => Some(m),
        _ => None,
    };
    assert!(
        msg.as_deref()
            .is_some_and(|m| m.contains("timed out draining stdout")),
        "expected a timeout panic naming the stdout drain, got {msg:?} after {took:?}"
    );
    assert!(
        took < Duration::from_millis(7500),
        "the single 6s deadline was not honoured (a fresh drain budget would cut at >= 9s): run_bounded took {took:?}"
    );
}

#[test]
fn exit_after_slow_read_with_stdin_held_by_a_grandchild_is_cut_at_the_deadline() {
    let dir = fresh_dir("slow-read-stdin-held");
    let pidfile = dir.join("pids");
    // The stub exits 3s after it starts (the deadline is 6s from spawn, so
    // process-start latency under load of up to ~3s is tolerated) while a
    // grandchild keeps the stdin pipe open and never reads it: the 1MB write
    // stays blocked. Correct: cut at ~6s, whatever the start latency. A
    // stdin-write wait given a fresh 6s budget after the exit would be cut at
    // start latency + 3s + 6s >= 9s.
    let cmd = sh_stub(
        &dir,
        &format!(
            "sleep 120 <&0 &\necho $! > {}\nsleep 3\nexit 0",
            pidfile.display()
        ),
    );
    let (outcome, took) = timed_run(
        cmd,
        vec![b'x'; 1024 * 1024],
        Duration::from_secs(6),
        Duration::from_secs(25),
    );
    for p in pids_in(&pidfile, 1) {
        kill(p);
    }
    let msg = match outcome {
        Outcome::Panicked(m) => Some(m),
        _ => None,
    };
    assert!(
        msg.as_deref()
            .is_some_and(|m| m.contains("timed out writing stdin")),
        "expected a timeout panic naming the stdin write, got {msg:?} after {took:?}"
    );
    assert!(
        took < Duration::from_millis(7500),
        "the single 6s deadline was not honoured (a fresh write-wait budget would cut at >= 9s): run_bounded took {took:?}"
    );
}
