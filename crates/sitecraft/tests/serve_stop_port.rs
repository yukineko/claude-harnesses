//! `sitecraft serve stop` / early-exit honesty when a listener escapes the
//! recorded process group.
//!
//! `stop` (and the failed-start cleanup) kill the recorded process group. A
//! dev server that daemonizes into a new session (`os.setsid()` after a fork)
//! is outside that group: killing the group leaves it listening on the port.
//! "The group is gone" is therefore not evidence that the server is stopped;
//! only "the port is free" is. These tests pin that:
//!
//! 1. `stop_reports_port_still_in_use_when_listener_escaped_the_group` — stop
//!    must not report success while the port still accepts connections: exit 2
//!    and stderr names the port.
//! 2. `early_exit_with_surviving_listener_does_not_claim_termination` — the
//!    early-exit message must not claim cleanup succeeded while a survivor
//!    holds the port.
//! 3. `normal_stop_exits_zero_and_frees_the_port` — control: the ordinary
//!    stub server stops with exit 0 and its port is free afterwards.
//!
//! Fixtures: `tests/fixtures/serve/escaping_server.py` (escaping daemon) and
//! `tests/fixtures/serve/stub_server.py` (ordinary server, control).
//!
//! Orphan safety: every test holds a [`Reaper`] that SIGKILLs the escaped
//! daemon (its pid is also its own pgid/sid), the leader's group and the
//! leader on drop, pass or panic. Both fixtures also exit on their own.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sitecraft");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/serve")
        .join(name)
}

/// A port nobody is listening on right now (bind :0, read it, release it).
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
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

    fn pid_file(&self, name: &str) -> Option<i32> {
        fs::read_to_string(self.out.join(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn wait_pid_file(&self, name: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(p) = self.pid_file(name) {
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

/// Does something accept TCP connections on 127.0.0.1:`port`?
fn port_accepts(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

/// SIGKILLs the escaped daemon (pid == its own pgid), the leader's group and
/// the leader on drop, so a failing assertion leaks nothing.
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

/// Defect 1: the leader's group is killed, but the daemon that escaped it via
/// `setsid` still listens on the recorded port. `stop` must check the port
/// after killing, and since it still accepts connections, exit 2 with a
/// message naming the port — not "stopped" / exit 0.
#[test]
fn stop_reports_port_still_in_use_when_listener_escaped_the_group() {
    let env = Env::new("escaping_server.py", "idle", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "precondition: start (leader alive, escaped daemon answers): {}",
        describe(&o)
    );
    let daemon = env.wait_pid_file("daemon.pid");
    assert!(daemon > 1, "daemon pid recorded");

    let o = env.run(&["serve", "stop"]);
    // Precondition of the scenario (not of the implementation): the escaped
    // daemon really is still listening after stop killed the group.
    assert!(
        port_accepts(env.port),
        "scenario precondition: escaped daemon {daemon} must still listen on {} after stop",
        env.port
    );
    assert_eq!(
        o.status.code(),
        Some(2),
        "stop must not report success while port {} still accepts connections: {}",
        env.port,
        describe(&o)
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains(&env.port.to_string()),
        "stop's message must name the port still in use ({}): {}",
        env.port,
        describe(&o)
    );
}

/// Defect 2: the leader exits before ready while the daemon it forked (new
/// session) keeps listening on the port. serve.rs's early-exit path currently
/// appends `(process group <pgid> terminated; log: …)` once the leader's
/// (now empty) group is gone — a claim that cleanup succeeded, false here
/// because a survivor still holds the port.
///
/// Choice of assertions (from reading `start_inner`'s `fail` closure): the
/// message must not contain the word `terminated` (the false claim), and it
/// must name the port together with a "still in use" / "survivor" style
/// explanation (`in use` or `surviv`), so the user learns what to clean up.
#[test]
fn early_exit_with_surviving_listener_does_not_claim_termination() {
    let env = Env::new("escaping_server.py", "exit", free_port(), 30);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    let daemon = env.wait_pid_file("daemon.pid");
    assert!(
        port_accepts(env.port),
        "scenario precondition: escaped daemon {daemon} must hold port {}",
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
        "early-exit message must not claim the group/server was terminated while a survivor holds port {}: {}",
        env.port,
        describe(&o)
    );
    assert!(
        stderr.contains(&format!("port {}", env.port))
            && (stderr.contains("in use") || stderr.contains("surviv")),
        "early-exit message must say port {} is still in use / held by a survivor: {}",
        env.port,
        describe(&o)
    );
}

/// Control: an ordinary server (whole tree in the recorded group) stops with
/// exit 0, and its port is free afterwards (connect refused, bindable).
#[test]
fn normal_stop_exits_zero_and_frees_the_port() {
    let env = Env::new("stub_server.py", "serve", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(0), "start: {}", describe(&o));
    assert!(port_accepts(env.port), "started server must listen");

    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stop stdout is not JSON ({e}): {}", describe(&o)));
    assert_eq!(v["status"], "stopped", "stop JSON: {v}");
    assert!(
        !port_accepts(env.port),
        "port {} must be free after a successful stop",
        env.port
    );
    TcpListener::bind(("127.0.0.1", env.port))
        .unwrap_or_else(|e| panic!("port {} must be bindable after stop: {e}", env.port));
}
