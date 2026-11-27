#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier (session-master-verify) F5: concurrent election.
//! N real processes elect at the same time against one temp state dir; exactly
//! one must end (and report itself) master. A lock holder that dies must not
//! leave two masters, nor zero masters forever.
mod support;
use std::process::{Command, Stdio};
use std::time::Duration;
use support::*;

const N: usize = 6;
const ROUNDS: usize = 25;

fn role_of(o: &Out) -> String {
    o.json()["role"].as_str().unwrap_or("").to_string()
}

/// Every session's own view: exactly one says master (exit 0), the rest member.
fn assert_exactly_one_master(sb: &Sandbox, ids: &[String], now: i64, ctx: &str) -> String {
    let masters: Vec<&String> = ids
        .iter()
        .filter(|id| {
            let o = sb.role(id, now);
            assert!(
                o.code == 0 || o.code == 1,
                "{ctx}: role {id} code {} {}",
                o.code,
                o.stderr
            );
            o.code == 0
        })
        .collect();
    assert_eq!(masters.len(), 1, "{ctx}: masters by role = {masters:?}");
    let st = sb.status(&ids[0], now);
    assert_eq!(
        st["master"]["session"],
        masters[0].as_str(),
        "{ctx}: status {st}"
    );
    masters[0].clone()
}

#[test]
fn concurrent_register_elects_exactly_one_master() {
    for round in 0..ROUNDS {
        let sb = Sandbox::new();
        let ids: Vec<String> = (0..N).map(|i| format!("S{i}")).collect();
        let outs: Vec<Out> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .iter()
                .map(|id| s.spawn(|| sb.register(id, T0)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (id, o) in ids.iter().zip(&outs) {
            assert_eq!(
                o.code, 0,
                "round {round}: register {id}: {} {}",
                o.stdout, o.stderr
            );
        }
        let reported: Vec<&String> = ids
            .iter()
            .zip(&outs)
            .filter(|(_, o)| role_of(o) == "master")
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            reported.len(),
            1,
            "round {round}: register replies say master: {reported:?}"
        );
        let st = sb.status(&ids[0], T0);
        assert_eq!(
            st["members"].as_array().unwrap().len(),
            N,
            "round {round}: lost update, status {st}"
        );
        let m = assert_exactly_one_master(&sb, &ids, T0, &format!("round {round}"));
        assert_eq!(&m, reported[0], "round {round}");
    }
}

#[test]
fn concurrent_promotion_after_master_death_elects_exactly_one() {
    for round in 0..ROUNDS {
        let sb = Sandbox::new();
        assert_eq!(sb.register("A", T0).code, 0);
        let ids: Vec<String> = (0..N).map(|i| format!("M{i}")).collect();
        for (k, id) in ids.iter().enumerate() {
            assert_eq!(sb.register(id, T0 + 1 + k as i64).code, 0);
        }
        // Members heartbeat at T0+50 (A does not), so at T0+70 A is past
        // TTL=60 and every member is still live; then they elect concurrently.
        for id in &ids {
            assert_eq!(sb.sm(&["heartbeat"], id, T0 + 50).code, 0);
        }
        let now = T0 + 70;
        let sbr = &sb;
        let outs: Vec<Out> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .iter()
                .map(|id| s.spawn(move || sbr.sm(&["heartbeat"], id, now)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for o in &outs {
            assert_eq!(o.code, 0, "round {round}: {} {}", o.stdout, o.stderr);
        }
        let mut all = ids.clone();
        all.push("A".into());
        let m = assert_exactly_one_master(&sb, &all, now, &format!("round {round}"));
        assert_eq!(m, "M0", "round {round}: oldest live member must win");
    }
}

fn lock_path(sb: &Sandbox) -> std::path::PathBuf {
    let groups = sb.home.join("sm").join("groups");
    let mut it = std::fs::read_dir(&groups).unwrap();
    let g = it.next().unwrap().unwrap().path();
    assert!(it.next().is_none(), "one group expected");
    g.join("lock")
}

/// Hold the group flock in a separate process, report when held.
fn spawn_lock_holder(lock: &std::path::Path) -> std::process::Child {
    let mut c = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(
            "import fcntl,sys,time\nf=open(sys.argv[1],'a')\nfcntl.flock(f,fcntl.LOCK_EX)\nprint('held',flush=True)\ntime.sleep(120)",
        )
        .arg(lock)
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3");
    let mut buf = [0u8; 5];
    std::io::Read::read_exact(c.stdout.as_mut().unwrap(), &mut buf).unwrap();
    assert_eq!(&buf, b"held\n");
    c
}

#[test]
fn lock_holder_crash_leaves_exactly_one_master() {
    let sb = Sandbox::new();
    assert_eq!(sb.register("A", T0).code, 0);
    let lock = lock_path(&sb);
    let mut holder = spawn_lock_holder(&lock);
    // While held: B cannot decide -> must not claim master (no split brain).
    let o = sb.register("B", T0 + 1);
    assert_ne!(
        o.code, 0,
        "register under a held lock must fail: {}",
        o.stdout
    );
    let g = sb.gate_bash("B", &sb.repo, "git push", T0 + 1);
    assert!(is_deny(&g), "push under held lock must Deny: {}", g.stdout);
    // Crash the holder (SIGKILL) mid-transaction.
    holder.kill().unwrap();
    holder.wait().unwrap();
    // Simulate a torn write left behind by the crashed process.
    let tmp = lock.with_file_name("state.json.tmp.999999");
    std::fs::write(&tmp, b"{\"master\": \"B\", \"sess").unwrap();
    assert_eq!(sb.register("B", T0 + 2).code, 0);
    assert_exactly_one_master(&sb, &["A".into(), "B".into()], T0 + 2, "after crash");
    assert_eq!(sb.role("A", T0 + 2).code, 0, "A stays master");
}

#[test]
fn lock_holder_crash_then_master_death_does_not_leave_zero_masters() {
    let sb = Sandbox::new();
    assert_eq!(sb.register("A", T0).code, 0);
    assert_eq!(sb.register("B", T0 + 1).code, 0);
    let lock = lock_path(&sb);
    let mut holder = spawn_lock_holder(&lock);
    holder.kill().unwrap();
    holder.wait().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    // A dies (no heartbeat); B keeps heartbeating past TTL -> B promoted.
    let now = T0 + 70;
    assert_eq!(sb.sm(&["heartbeat"], "B", now).code, 0);
    assert_exactly_one_master(&sb, &["A".into(), "B".into()], now, "after crash+death");
    assert_eq!(sb.role("B", now).code, 0);
}

#[test]
fn concurrent_register_and_gate_never_two_masters() {
    // Mix register + gate (which also elects) concurrently.
    for round in 0..ROUNDS {
        let sb = Sandbox::new();
        let ids: Vec<String> = (0..N).map(|i| format!("G{i}")).collect();
        let pushes: Vec<Out> = std::thread::scope(|s| {
            let hs: Vec<_> = ids
                .iter()
                .map(|id| {
                    let sb = &sb;
                    s.spawn(move || sb.gate_bash(id, &sb.repo, "git push", T0))
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let allowed = pushes.iter().filter(|o| o.code == 0 && !is_deny(o)).count();
        assert_eq!(allowed, 1, "round {round}: pushes allowed = {allowed}");
        assert_exactly_one_master(&sb, &ids, T0, &format!("round {round}"));
    }
}
