//! F13 — `sitecraft serve start`'s ready probe must accept a dev server that
//! listens only on the IPv6 loopback.
//!
//! Observed 2026-11-03 (sitecraft 0.1.3, testbed ~/src/3d-pipeline-web):
//! Vite 8.3.5 on Node v26.3.0 binds `localhost`, which resolved to `::1` only.
//! `serve start` reported
//! "UNDETERMINED - dev server not ready on http://127.0.0.1:5173/ within 60s
//! (no HTTP answer)" while the server log showed "ready in 84 ms": the probe
//! only ever connects to 127.0.0.1.
//!
//! Contract pinned here:
//!
//! 1. `serve_start_detects_server_listening_only_on_ipv6_loopback` — a server
//!    answering HTTP 200 on `[::1]:<port>` only (IPV6_V6ONLY=1, 127.0.0.1
//!    refuses) is detected ready: `serve start` exits 0 and the following
//!    `serve stop` exits 0. (The recorded `url` is deliberately not asserted:
//!    how the state names the origin is the fix's choice.)
//! 2. `serve_start_still_undetermined_when_nothing_ever_answers` — control
//!    against a fail-open fix: a server that never listens on any family still
//!    yields exit 2 ("not ready").
//!
//! No vacuous pass: if the host cannot bind `::1`, test 1 panics with
//! "host has no IPv6 loopback; cannot test".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sitecraft");

fn stub_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serve/stub_server.py")
}

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

fn accepts_v6(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

fn accepts_v4(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    out: PathBuf,
    port: u16,
}

impl Env {
    fn new(mode: &str, port: u16, ready_timeout_secs: u64) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("site");
        let state = tmp.path().join("state");
        let out = tmp.path().join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&out).unwrap();
        let cmd = serde_json::to_string(&vec![
            "python3".to_string(),
            stub_script().display().to_string(),
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

/// SIGKILLs every pid / pgid the stub recorded, pass or panic.
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
        if let Some(g) = read("pgid") {
            if g > 1 && g != own {
                // SAFETY: plain signal delivery to the group the stub created.
                unsafe { libc::killpg(g, libc::SIGKILL) };
            }
        }
        for p in [read("leader.pid"), read("grandchild.pid")]
            .into_iter()
            .flatten()
        {
            if p > 1 {
                // SAFETY: plain signal delivery to a pid the stub recorded.
                unsafe { libc::kill(p, libc::SIGKILL) };
            }
        }
    }
}

/// Run the serve6 stub directly (no sitecraft) on `port` and prove it answers
/// HTTP 200 on [::1] while 127.0.0.1 refuses; then kill it. This makes a RED
/// in test 1 attributable to sitecraft's probe, not to the fixture.
fn assert_serve6_fixture_is_ipv6_only(port: u16) {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new("python3")
        .arg(stub_script())
        .arg("serve6")
        .arg(port.to_string())
        .arg(tmp.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let answer = loop {
        let addr = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
        if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            use std::io::{Read, Write};
            s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            break b;
        }
        assert!(
            Instant::now() < deadline,
            "serve6 stub never listened on [::1]:{port}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let v4 = accepts_v4(port);
    let grandchild = fs::read_to_string(tmp.path().join("grandchild.pid")).ok();
    let _ = child.kill();
    let _ = child.wait();
    if let Some(p) = grandchild.and_then(|s| s.trim().parse::<i32>().ok()) {
        // SAFETY: plain signal delivery to the stub's own sleep child.
        unsafe { libc::kill(p, libc::SIGKILL) };
    }
    assert!(
        answer.starts_with("HTTP/1.0 200"),
        "fixture: serve6 must answer HTTP 200 on [::1]:{port}, got {answer:?}"
    );
    assert!(!v4, "fixture: serve6 must not accept on 127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while accepts_v6(port) {
        assert!(
            Instant::now() < deadline,
            "fixture: [::1]:{port} not released"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 1. Server answers HTTP 200 on [::1] only. start must report ready (exit 0).
#[test]
fn serve_start_detects_server_listening_only_on_ipv6_loopback() {
    let port = free_port();
    assert_serve6_fixture_is_ipv6_only(port);
    let env = Env::new("serve6", port, 5);
    let _reaper = env.reaper();
    env.trust();

    let started = Instant::now();
    let o = env.run(&["serve", "start"]);
    // Scenario preconditions, checked whatever start answered: if start
    // killed the group on its deadline the listener is gone, so observe the
    // stub's own record that it spawned in serve6 mode.
    assert_eq!(
        fs::read_to_string(env.out.join("marker")).ok().as_deref(),
        Some("serve6"),
        "scenario precondition: the serve6 stub was spawned: {}",
        describe(&o)
    );
    if o.status.code() == Some(0) {
        assert!(
            accepts_v6(env.port),
            "scenario precondition: stub listens on [::1]:{}",
            env.port
        );
        assert!(
            !accepts_v4(env.port),
            "scenario precondition: 127.0.0.1:{} must refuse (the stub is IPv6-only)",
            env.port
        );
    }
    assert_eq!(
        o.status.code(),
        Some(0),
        "serve start must detect a dev server that answers HTTP 200 on [::1]:{} only \
         (took {:?}): {}",
        env.port,
        started.elapsed(),
        describe(&o)
    );

    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
    assert!(!accepts_v6(env.port), "[::1] free after stop");
}

/// 2. Control: nothing ever listens on any family → still exit 2.
#[test]
fn serve_start_still_undetermined_when_nothing_ever_answers() {
    let env = Env::new("noready", free_port(), 2);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert!(
        env.out.join("marker").exists(),
        "scenario precondition: the noready stub was spawned: {}",
        describe(&o)
    );
    assert_eq!(
        o.status.code(),
        Some(2),
        "a server that never answers must stay Undetermined: {}",
        describe(&o)
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("not ready"),
        "message must say not ready: {}",
        describe(&o)
    );
}
