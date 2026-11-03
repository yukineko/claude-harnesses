//! Spec `pages-url-spec` P1-P4: `sitecraft check pages` without `--base-url`
//! targets the base the running dev server recorded (`serve status`), so an
//! IPv6-only dev server (Vite on macOS listens on `::1` only) is reachable.
//!
//! Written by an independent test author (CLAUDE.md 2(a)); the implementer
//! makes these pass without editing them.
//!
//! # Method
//!
//! * The "server" is a fake HTTP listener bound ONLY to `[::1]:<port>` (or only
//!   to `127.0.0.1:<port>` for the controls), recording request lines.
//! * `serve start` is not used: the serve state file is written exactly as
//!   `serve start` records it (`<state>/serve/<sha256(canonical root)>.json`,
//!   path from `sitecraft::serve::state_file`), with `pgid` of a live
//!   `sleep` process in its own process group, so `serve status` reports
//!   `running`. A dead pgid / corrupt body gives an undetermined status.
//! * `SITECRAFT_NODE` is a stub that records its argv[2] (the JSON input
//!   carrying `base_url`) and reports a clean run; node's own `base_url` is
//!   therefore observable, and `node-ran` proves it was spawned.
//! * `api_paths = ["/api/health"]` makes the api GET observable on the wire.
//!
//! If the host has no IPv6 loopback the `[::1]` tests print a note and return.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv6Addr, TcpListener};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use spec_common::*;

fn ipv6_available() -> bool {
    TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_ok()
}

/// Skip (with a note) when there is no IPv6 loopback.
macro_rules! need_ipv6 {
    () => {
        if !ipv6_available() {
            eprintln!("SKIP: host has no IPv6 loopback; cannot test [::1]");
            return;
        }
    };
}

/// Listener recording request lines; answers 200 `{}`.
fn serve_on(l: TcpListener) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let rec = Arc::clone(&rec);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                }
                rec.lock().unwrap().push(line.trim().to_string());
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            });
        }
    });
    seen
}

/// A process in its own process group, killed on drop: the "dev server" whose
/// pgid the state file records.
struct Leader(Child);
impl Leader {
    fn spawn() -> Self {
        let c = Command::new("sleep")
            .arg("120")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        Leader(c)
    }
    fn pgid(&self) -> i32 {
        self.0.id() as i32
    }
}
impl Drop for Leader {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    p: Project,
    port: u16,
    node: PathBuf,
}

impl Env {
    /// Project with `api_paths = ["/api/health"]` and a clean node stub.
    /// `v6`: the chosen port is also bound on `[::1]` (retried until it is).
    fn new(v6: bool) -> (Self, Option<TcpListener>) {
        for _ in 0..50 {
            let p = Project::new();
            let port = p.write_sitecraft_toml("api_paths = [\"/api/health\"]\n");
            let l6 = if v6 {
                match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
                    Ok(l) => Some(l),
                    Err(_) => continue,
                }
            } else {
                None
            };
            let node = p.base.join("bin/node-stub");
            write_exec(
                &node,
                &format!(
                    "#!/bin/sh\nmkdir -p '{m}'\n: > '{m}/node-ran'\nprintf '%s' \"$2\" > '{m}/node-arg'\necho '{{\"schema\":\"sitecraft-check-pages/1\",\"status\":\"ok\",\"pages_checked\":1,\"findings\":[]}}'\nexit 0\n",
                    m = p.base.join("markers").display()
                ),
            );
            return (Env { p, port, node }, l6);
        }
        panic!("could not find a port free on both loopbacks");
    }

    fn run(&self, extra: &[&str]) -> Out {
        let mut args = vec!["check", "pages"];
        args.extend_from_slice(extra);
        self.p
            .run_env(&args, &[("SITECRAFT_NODE", os(&self.node))])
    }

    fn state_file(&self) -> PathBuf {
        match sitecraft::serve::state_file(&self.p.state, &self.p.root) {
            sitecraft::Determination::Known(p) => p,
            sitecraft::Determination::Undetermined(why) => {
                panic!("serve::state_file must locate the state file: {why}")
            }
        }
    }

    fn write_state_body(&self, body: &str) {
        write(&self.state_file(), body);
    }

    /// State exactly as `serve start` records it.
    fn record_running(&self, pgid: i32, url: &str) {
        let body = serde_json::json!({
            "pgid": pgid,
            "port": self.port,
            "url": url,
            "root": self.p.canonical_root(),
        });
        self.write_state_body(&body.to_string());
    }

    fn node_ran(&self) -> bool {
        self.p.base.join("markers/node-ran").exists()
    }

    /// The `base_url` the node stub was handed.
    fn node_base_url(&self) -> String {
        let t = std::fs::read_to_string(self.p.base.join("markers/node-arg"))
            .expect("node stub must have been spawned and recorded its input");
        let v: serde_json::Value = serde_json::from_str(&t).expect("node input is JSON");
        v["base_url"].as_str().expect("base_url string").to_string()
    }
}

fn saw_api_get(seen: &Arc<Mutex<Vec<String>>>) -> bool {
    seen.lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("GET /api/health HTTP/1."))
}

// ------------------------------------------------------------------- P1

#[test]
fn p1_running_ipv6_server_is_the_base_for_pages_and_api_paths() {
    need_ipv6!();
    let (e, l6) = Env::new(true);
    let seen = serve_on(l6.unwrap());
    let leader = Leader::spawn();
    e.record_running(leader.pgid(), &format!("http://[::1]:{}", e.port));
    // Precondition: serve status reports running (so P1 applies).
    let st = e.p.run(&["serve", "status"]);
    expect_code(&st, 0, "precondition: serve status with a live recorded group");
    assert!(st.stdout.contains("running"), "{}", st.describe());

    let o = e.run(&[]);
    expect_code(&o, 0, "check pages, [::1]-only server recorded as running");
    assert_eq!(
        e.node_base_url(),
        format!("http://[::1]:{}", e.port),
        "node must be handed the recorded url as base_url\n{}",
        o.describe()
    );
    assert!(
        saw_api_get(&seen),
        "the api_paths GET must reach the [::1]-only server; saw {:?}\n{}",
        seen.lock().unwrap(),
        o.describe()
    );
}

#[test]
fn p1_running_ipv4_server_recorded_url_is_the_base() {
    // Control: the recorded 127.0.0.1 url also drives the base (passes today).
    let (e, _) = Env::new(false);
    let seen = serve_on(TcpListener::bind(("127.0.0.1", e.port)).unwrap());
    let leader = Leader::spawn();
    e.record_running(leader.pgid(), &format!("http://127.0.0.1:{}", e.port));
    let o = e.run(&[]);
    expect_code(&o, 0, "check pages, 127.0.0.1 server recorded as running");
    assert_eq!(e.node_base_url(), format!("http://127.0.0.1:{}", e.port));
    assert!(saw_api_get(&seen), "{:?}", seen.lock().unwrap());
}

// ------------------------------------------------------------------- P2

#[test]
fn p2_no_state_keeps_127_0_0_1() {
    let (e, _) = Env::new(false);
    let seen = serve_on(TcpListener::bind(("127.0.0.1", e.port)).unwrap());
    assert!(!e.state_file().exists(), "precondition: no serve state");
    let st = e.p.run(&["serve", "status"]);
    assert!(st.stdout.contains("stopped"), "{}", st.describe());
    let o = e.run(&[]);
    expect_code(&o, 0, "check pages with no recorded server");
    assert_eq!(e.node_base_url(), format!("http://127.0.0.1:{}", e.port));
    assert!(saw_api_get(&seen), "{:?}", seen.lock().unwrap());
}

#[test]
fn p2_no_state_does_not_use_an_ipv6_listener() {
    // With nothing recorded, a [::1]-only listener must NOT receive the GET
    // (the base is 127.0.0.1, where nothing listens -> failing exit).
    need_ipv6!();
    let (e, l6) = Env::new(true);
    let seen = serve_on(l6.unwrap());
    let o = e.run(&[]);
    assert_eq!(e.node_base_url(), format!("http://127.0.0.1:{}", e.port));
    assert!(
        !saw_api_get(&seen),
        "stopped state must not probe [::1]; saw {:?}",
        seen.lock().unwrap()
    );
    expect_failure(&o, "api GET to 127.0.0.1 with nothing listening");
}

// ------------------------------------------------------------------- P3

fn assert_undetermined_names_serve_state(e: &Env, o: &Out) {
    expect_code(o, 2, "undeterminable serve state");
    assert!(
        !e.node_ran(),
        "node must not be spawned when the serve state is undetermined\n{}",
        o.describe()
    );
    let all = o.all();
    let sf = e.state_file().display().to_string();
    assert!(
        all.contains(&sf) || all.to_lowercase().contains("serve state"),
        "the reason must name the serve state (its file {sf} or the words `serve state`)\n{}",
        o.describe()
    );
}

#[test]
fn p3_corrupt_state_file_is_undetermined_and_node_not_spawned() {
    let (e, _) = Env::new(false);
    let _seen = serve_on(TcpListener::bind(("127.0.0.1", e.port)).unwrap());
    e.write_state_body("{ this is not json");
    // Precondition: serve status itself is undetermined (exit 2).
    expect_code(
        &e.p.run(&["serve", "status"]),
        2,
        "precondition: serve status on a corrupt state file",
    );
    let o = e.run(&[]);
    assert_undetermined_names_serve_state(&e, &o);
}

#[test]
fn p3_dead_recorded_group_is_undetermined_and_node_not_spawned() {
    let (e, _) = Env::new(false);
    let _seen = serve_on(TcpListener::bind(("127.0.0.1", e.port)).unwrap());
    // A pgid that is gone: spawn, kill, reap.
    let leader = Leader::spawn();
    let dead = leader.pgid();
    drop(leader);
    e.record_running(dead, &format!("http://127.0.0.1:{}", e.port));
    expect_code(
        &e.p.run(&["serve", "status"]),
        2,
        "precondition: serve status with a dead recorded group",
    );
    let o = e.run(&[]);
    assert_undetermined_names_serve_state(&e, &o);
}

// ------------------------------------------------------------------- P4

#[test]
fn p4_explicit_base_url_wins_over_a_running_record() {
    need_ipv6!();
    let (e, l6) = Env::new(true);
    let seen6 = serve_on(l6.unwrap());
    let leader = Leader::spawn();
    // Recorded as 127.0.0.1, explicit says [::1].
    e.record_running(leader.pgid(), &format!("http://127.0.0.1:{}", e.port));
    let explicit = format!("http://[::1]:{}", e.port);
    let o = e.run(&["--base-url", &explicit]);
    expect_code(&o, 0, "explicit --base-url [::1]");
    assert_eq!(e.node_base_url(), explicit);
    assert!(saw_api_get(&seen6), "{:?}", seen6.lock().unwrap());
}

#[test]
fn p4_explicit_base_url_ignores_a_corrupt_state_file() {
    need_ipv6!();
    let (e, l6) = Env::new(true);
    let seen6 = serve_on(l6.unwrap());
    e.write_state_body("{ this is not json");
    let explicit = format!("http://[::1]:{}", e.port);
    let o = e.run(&["--base-url", &explicit]);
    expect_code(&o, 0, "explicit --base-url with a corrupt serve state");
    assert!(e.node_ran(), "{}", o.describe());
    assert_eq!(e.node_base_url(), explicit);
    assert!(saw_api_get(&seen6), "{:?}", seen6.lock().unwrap());
}
