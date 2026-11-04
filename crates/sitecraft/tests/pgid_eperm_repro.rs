//! Reproduction of the intermittent "cannot probe process group <pgid>:
//! Operation not permitted" failure of `sitecraft serve stop` (and of
//! `check capability` threejs, which calls the same `serve::stop`).
//!
//! Measured on macOS (Darwin 25.6, see the commit message for the script):
//!
//! * `killpg(pgid, 0)` on a group whose only member is a zombie (exited, not
//!   yet reaped by its parent) returns **EPERM**, not ESRCH, for the zombie's
//!   parent and for any other process alike. `killpg(pgid, SIGTERM|SIGKILL)`
//!   likewise returns EPERM. After the reap it returns ESRCH.
//! * A zombie leader with a live member, or a live leader with a zombie
//!   member, answers 0 (alive) — those are not the trigger.
//! * `serve::wait_gone` reaps with `waitpid(pgid, WNOHANG)` and only then
//!   probes. When the leader dies *between* that waitpid and the probe (the
//!   usual case is the SIGTERM sent just before the first iteration), the
//!   probe sees a zombie-only group, gets EPERM, and `group_liveness`'s `?`
//!   aborts the stop as Undetermined at once. That window is microseconds
//!   wide in the flaky tests; a model of the loop without the 50 ms sleep hit
//!   EPERM as the first non-alive answer in 200 of 200 runs.
//!
//! The race window cannot be widened from outside while the leader is
//! sitecraft's own child, so this test makes the same kernel condition
//! permanent instead: the recorded group's leader is a child of *this test
//! process*, which does not reap it while `serve stop` runs. sitecraft cannot
//! reap it (ECHILD), so every probe sees exactly what the flaky probe saw: a
//! group whose processes have all exited, leaving only a zombie.
//!
//! Correct behaviour asserted (CLAUDE.md §3 — undetermined stays restrictive):
//!
//! * zombie-only group (every member has exited; nothing can hold the port) →
//!   the group is **gone**: `serve stop` is `Known(Stopped)` (exit 0) and the
//!   state file is removed. Saying "undetermined" here is the bug.
//! * a live group owned by another user (EPERM because we may not signal it)
//!   → **Undetermined** (exit 2), state file kept, the group untouched. This
//!   control holds at HEAD and must keep holding after the fix: EPERM is not
//!   "gone" by itself.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use spec_common::*;

/// State file of `root` under `state` (same layout as `serve::state_file`:
/// `<state>/serve/<sha256(canonical root)>.json`).
fn state_file(p: &Project) -> PathBuf {
    p.state.join("serve").join(format!(
        "{}.json",
        sha256_hex(p.root.as_os_str().as_encoded_bytes())
    ))
}

/// Record `pgid` as the dev server of `p` on a port nobody listens on.
fn write_state(p: &Project, pgid: i32) -> (PathBuf, u16) {
    let port = free_port();
    let sf = state_file(p);
    let st = serde_json::json!({
        "pgid": pgid,
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "root": p.root,
    });
    write(&sf, serde_json::to_vec_pretty(&st).unwrap());
    (sf, port)
}

/// Kills and reaps the stub leader when the test ends (pass or panic).
struct Leader(Child);

impl Drop for Leader {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A one-process group led by a child of this test that dies on SIGTERM.
fn spawn_group_leader() -> Leader {
    let child = Command::new("sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn sleep");
    Leader(child)
}

fn killpg_errno(pgid: i32) -> Option<i32> {
    // SAFETY: signal 0 only checks existence/permission.
    if unsafe { libc::killpg(pgid, 0) } == 0 {
        None
    } else {
        std::io::Error::last_os_error().raw_os_error()
    }
}

/// The recorded group's only process exits on `stop`'s SIGTERM and stays an
/// unreaped zombie (its parent is this test, not sitecraft). Every process of
/// the group has exited and nothing listens on the port: the server is
/// stopped. HEAD answers "cannot probe process group …: Operation not
/// permitted" (exit 2) instead.
#[test]
fn stop_of_a_group_whose_only_process_exited_unreaped_is_stopped() {
    let p = Project::new();
    let leader = spawn_group_leader();
    let pgid = i32::try_from(leader.0.id()).unwrap();
    assert_eq!(killpg_errno(pgid), None, "precondition: the group is alive");
    let (sf, _port) = write_state(&p, pgid);

    let o = run_deadline(p.cmd(&["serve", "stop"]), None, Duration::from_secs(60));

    // The leader really is dead (a zombie): SIGTERM reached it. Observed
    // before reaping, so the assertion below is about a group that was
    // zombie-only for the whole of `stop`.
    let mut leader = leader;
    let status = leader.0.wait().expect("reap the stub leader");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "precondition: `serve stop` must have SIGTERMed the recorded group\n{}",
        o.describe()
    );

    expect_code(
        &o,
        0,
        "serve stop of a group whose only process has exited (zombie-only group)",
    );
    let v = o.json_objects().pop().unwrap_or_else(|| {
        panic!("serve stop printed no JSON\n{}", o.describe());
    });
    assert_eq!(v["status"], "stopped", "{}", o.describe());
    assert_eq!(v["pgid"], pgid, "{}", o.describe());
    assert!(
        !sf.exists(),
        "a stopped server's state file must be removed\n{}",
        o.describe()
    );
    assert!(
        !o.all().contains("cannot probe process group"),
        "{}",
        o.describe()
    );
}

/// A live process group of another user (root): EPERM here means "not ours
/// to judge", so `stop` must stay Undetermined, keep the state file, and leave
/// the group alone. This holds at HEAD; it pins that a fix must not read
/// EPERM as "gone".
#[test]
fn stop_of_another_users_live_group_stays_undetermined() {
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    assert_ne!(
        euid, 0,
        "this test signals another user's group and cannot be run as root (it would hit a system daemon)"
    );
    let ps = Command::new("ps")
        .args(["-axo", "pid=,pgid=,uid=,stat="])
        .output()
        .expect("run ps");
    assert!(ps.status.success(), "ps failed: {ps:?}");
    let text = String::from_utf8(ps.stdout).expect("ps output is UTF-8");
    let pgid: i32 = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            match f.as_slice() {
                [pid, pgid, uid, stat] if pid == pgid && *uid == "0" && !stat.contains('Z') => {
                    pid.parse::<i32>().ok().filter(|&n| n > 1)
                }
                _ => None,
            }
        })
        .next()
        .expect("a live root-owned process group leader (pid > 1) exists on macOS");
    assert_eq!(
        killpg_errno(pgid),
        Some(libc::EPERM),
        "precondition: the root group {pgid} answers EPERM"
    );
    let p = Project::new();
    let (sf, _port) = write_state(&p, pgid);

    let o = run_deadline(p.cmd(&["serve", "stop"]), None, Duration::from_secs(60));

    expect_code(&o, 2, "serve stop of another user's live process group");
    assert!(
        sf.exists(),
        "an undetermined stop must keep the state file\n{}",
        o.describe()
    );
    assert_eq!(
        killpg_errno(pgid),
        Some(libc::EPERM),
        "the root group {pgid} must still be there\n{}",
        o.describe()
    );
}
