//! `sitecraft serve` port checks must cover the IPv6 loopback, not only
//! 127.0.0.1.
//!
//! A dev server (or an unrelated process) can hold `[::1]:<port>` with
//! `IPV6_V6ONLY=1` while 127.0.0.1:<port> is free. A browser resolving
//! `localhost` may well reach the `::1` listener, so "127.0.0.1 refuses" is not
//! evidence that the port is free. These tests pin:
//!
//! 1. `stop_reports_port_still_in_use_when_ipv6_survivor_holds_it` — the
//!    escaped daemon listens on `[::1]` only; stop must exit 2, name
//!    `port <N>` and say "still in use", and keep the state file.
//! 2. `early_exit_with_ipv6_survivor_does_not_claim_termination` — the
//!    early-exit message must not say "terminated" while `[::1]:<N>` is held.
//! 3. `start_refuses_when_ipv6_loopback_port_is_already_held` — an unrelated
//!    listener on `[::1]:<port>` is a port conflict: exit 2, nothing spawned.
//!    Paired positive control: once the squatter is gone the same config
//!    starts (so "never spawns" cannot pass).
//! 4. `normal_stop_exits_zero_and_frees_both_loopbacks` — control.
//!
//! No vacuous pass: if the host cannot bind `::1` at all, every test panics
//! with "host has no IPv6 loopback; cannot test".
//!
//! Orphan safety: every test holds a [`Reaper`] that SIGKILLs the escaped
//! daemon, the leader's group, the leader and any grandchild on drop.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sitecraft");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/serve")
        .join(name)
}

/// Fails loudly when the host has no IPv6 loopback (never a silent pass).
fn require_ipv6() {
    if let Err(e) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) {
        panic!("host has no IPv6 loopback; cannot test (bind [::1]:0 failed: {e})");
    }
}

/// A port free on both [::1] and 127.0.0.1 right now.
fn free_port() -> u16 {
    require_ipv6();
    for _ in 0..50 {
        let l6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let port = l6.local_addr().unwrap().port();
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("could not find a port free on both loopbacks");
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    out: PathBuf,
    port: u16,
}

impl Env {
    fn new(script: &str, mode: &str, port: u16, ready_timeout_secs: u64) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("site");
        let state = tmp.path().join("state");
        let out = tmp.path().join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&out).unwrap();
        let cmd = serde_json::to_string(&vec![
            "python3".to_string(),
            fixture(script).display().to_string(),
            mode.to_string(),
            port.to_string(),
            out.display().to_string(),
        ])
        .unwrap();
        let toml = format!(
            "command = {cmd}\n\
             port = {port}\n\
             ready_path = \"/\"\n\
             ready_timeout_secs = {ready_timeout_secs}\n\
             pages = [\"/\"]\n\
             build_dir = \"dist\"\n\
             web_globs = [\"src/**\", \"index.html\"]\n"
        );
        fs::write(root.join("sitecraft.toml"), toml).unwrap();
        Env {
            _tmp: tmp,
            root,
            state,
            out,
            port,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .env("SITECRAFT_STATE_DIR", &self.state)
            .output()
            .unwrap()
    }

    fn trust(&self) {
        let o = self.run(&["trust"]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "precondition: `sitecraft trust` must record the config: {}",
            describe(&o)
        );
    }

    fn wait_pid_file(&self, name: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(p) = fs::read_to_string(self.out.join(name))
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                return p;
            }
            assert!(
                Instant::now() < deadline,
                "fixture never wrote {name} into {}",
                self.out.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Serve state records: `*.json` files (anywhere under the state dir) that
    /// sit next to a same-stem `*.log` — the server log `start` creates beside
    /// its state record. The trust store has no such sibling.
    fn state_records(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, v: &mut Vec<PathBuf>) {
            let Ok(rd) = fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, v);
                } else if p.extension().and_then(|x| x.to_str()) == Some("json")
                    && p.with_extension("log").exists()
                {
                    v.push(p);
                }
            }
        }
        let mut v = Vec::new();
        walk(&self.state, &mut v);
        v
    }

    fn reaper(&self) -> Reaper {
        Reaper {
            out: self.out.clone(),
        }
    }
}

fn describe(o: &Output) -> String {
    format!(
        "exit={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn accepts_v6(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

fn accepts_v4(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

/// SIGKILLs every pid / pgid a fixture recorded, pass or panic.
struct Reaper {
    out: PathBuf,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        // SAFETY: getpgrp has no preconditions.
        let own = unsafe { libc::getpgrp() };
        let read = |n: &str| -> Option<i32> {
            fs::read_to_string(self.out.join(n))
                .ok()
                .and_then(|s| s.trim().parse().ok())
        };
        for g in [read("daemon.pid"), read("pgid")].into_iter().flatten() {
            if g > 1 && g != own {
                // SAFETY: plain signal delivery to a group a fixture created.
                unsafe { libc::killpg(g, libc::SIGKILL) };
            }
        }
        for p in [
            read("daemon.pid"),
            read("leader.pid"),
            read("grandchild.pid"),
        ]
        .into_iter()
        .flatten()
        {
            if p > 1 {
                // SAFETY: plain signal delivery to a pid a fixture recorded.
                unsafe { libc::kill(p, libc::SIGKILL) };
            }
        }
    }
}

/// 1. The leader (in the group) served HTTP on 127.0.0.1; the escaped daemon
///    holds [::1]:<port> only. After the group kill 127.0.0.1 refuses, but
///    ::1 still accepts: stop must not report success.
#[test]
fn stop_reports_port_still_in_use_when_ipv6_survivor_holds_it() {
    let env = Env::new("escaping_server.py", "idle6", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "precondition: start (leader answers on 127.0.0.1): {}",
        describe(&o)
    );
    let daemon = env.wait_pid_file("daemon.pid");
    assert!(
        accepts_v6(env.port),
        "scenario precondition: daemon {daemon} listens on [::1]:{}",
        env.port
    );
    assert_eq!(
        env.state_records().len(),
        1,
        "precondition: start wrote one state record"
    );

    let o = env.run(&["serve", "stop"]);
    assert!(
        accepts_v6(env.port),
        "scenario precondition: escaped daemon {daemon} must still hold [::1]:{} after stop",
        env.port
    );
    assert_eq!(
        o.status.code(),
        Some(2),
        "stop must not report success while [::1]:{} still accepts: {}",
        env.port,
        describe(&o)
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains(&format!("port {}", env.port)) && stderr.contains("still in use"),
        "stop's message must say port {} is still in use: {}",
        env.port,
        describe(&o)
    );
    assert_eq!(
        env.state_records().len(),
        1,
        "stop must keep the state file while the port is still held: {}",
        describe(&o)
    );
}

/// 2. The leader exits before ready; the daemon it forked (new session) holds
///    [::1]:<port>. The early-exit message must not claim termination.
#[test]
fn early_exit_with_ipv6_survivor_does_not_claim_termination() {
    let env = Env::new("escaping_server.py", "exit6", free_port(), 30);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    let daemon = env.wait_pid_file("daemon.pid");
    assert!(
        accepts_v6(env.port),
        "scenario precondition: escaped daemon {daemon} must hold [::1]:{}",
        env.port
    );
    assert_eq!(o.status.code(), Some(2), "early exit: {}", describe(&o));
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("exited before it was ready"),
        "scenario precondition: start must take the early-exit path: {}",
        describe(&o)
    );
    assert!(
        !stderr.contains("terminated"),
        "early-exit message must not claim termination while [::1]:{} is held: {}",
        env.port,
        describe(&o)
    );
    assert!(
        stderr.contains(&format!("port {}", env.port)) && stderr.contains("in use"),
        "early-exit message must say port {} is still in use: {}",
        env.port,
        describe(&o)
    );
}

/// 3. An unrelated listener already holds [::1]:<configured port>. start must
///    treat that as a port conflict (exit 2) and spawn nothing. Positive
///    control: once the squatter is gone the identical config does start.
#[test]
fn start_refuses_when_ipv6_loopback_port_is_already_held() {
    let port = free_port();
    let env = Env::new("stub_server.py", "serve", port, 10);
    let _reaper = env.reaper();
    env.trust();

    let squatter = TcpListener::bind((Ipv6Addr::LOCALHOST, port))
        .unwrap_or_else(|e| panic!("host has no IPv6 loopback; cannot test ([::1]:{port}: {e})"));
    assert!(
        !accepts_v4(port),
        "scenario precondition: 127.0.0.1:{port} is free"
    );
    assert!(
        accepts_v6(port),
        "scenario precondition: [::1]:{port} is held"
    );

    let o = env.run(&["serve", "start"]);
    let spawned = env.out.join("marker").exists();
    assert_eq!(
        o.status.code(),
        Some(2),
        "start must refuse when [::1]:{port} is already in use: {}",
        describe(&o)
    );
    assert!(
        !spawned,
        "start must not spawn the command while [::1]:{port} is held: {}",
        describe(&o)
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains(&format!("port {port}")),
        "conflict message must name port {port}: {}",
        describe(&o)
    );

    drop(squatter);
    let o = env.run(&["serve", "start"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "positive control: with the squatter gone the same config starts: {}",
        describe(&o)
    );
    assert!(env.out.join("marker").exists(), "positive control spawned");
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "control stop: {}", describe(&o));
}

/// 4. Control: an ordinary server stops with exit 0; both loopbacks refuse
///    afterwards and the state record is gone.
#[test]
fn normal_stop_exits_zero_and_frees_both_loopbacks() {
    let env = Env::new("stub_server.py", "serve", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(0), "start: {}", describe(&o));
    assert!(accepts_v4(env.port), "started server must listen");

    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stop stdout is not JSON ({e}): {}", describe(&o)));
    assert_eq!(v["status"], "stopped", "stop JSON: {v}");
    assert!(!accepts_v4(env.port), "127.0.0.1 free after stop");
    assert!(!accepts_v6(env.port), "[::1] free after stop");
    assert!(
        env.state_records().is_empty(),
        "state record removed after a successful stop"
    );
}
