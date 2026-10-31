//! `sitecraft serve start|stop|status` — end-to-end contract tests.
//!
//! Every test drives the real binary (`CARGO_BIN_EXE_sitecraft`) with an
//! isolated `SITECRAFT_STATE_DIR` (tempdir) and a project root whose
//! `sitecraft.toml` `command` runs `tests/fixtures/serve/stub_server.py`
//! (python3 standard library only; no npm / Vite). The stub writes a `marker`
//! file the moment it is spawned, plus its pid, its process-group id and the
//! pid of a `sleep` grandchild it forks into the same group, so the tests can
//! observe (a) whether the command ran at all and (b) whether `stop` / the
//! failure paths really tore down the whole process group.
//!
//! Trusting the config goes through the CLI (`sitecraft trust`), never by
//! writing the trust store directly: the store format is the implementation's.
//!
//! Each "nothing was spawned" assertion is paired with a positive control in
//! the same test (the identical config does spawn once its precondition is
//! met), so an implementation that never spawns anything cannot pass.
//!
//! Orphan safety: every test holds a [`Reaper`] that SIGKILLs every pid / pgid
//! the stub recorded when the test ends (pass or panic), and the stub itself
//! exits after 90 s regardless.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_sitecraft");

fn stub_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serve/stub_server.py")
}

/// A port nobody is listening on right now (bind :0, read it, release it).
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// One test's sandbox: project root, state dir, stub output dir.
struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    out: PathBuf,
    port: u16,
}

impl Env {
    /// `mode` is the stub mode (`serve` / `noready` / `exit1`).
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

    /// `sitecraft trust` must succeed: every trusted-path test depends on it.
    fn trust(&self) {
        let o = self.run(&["trust"]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "precondition: `sitecraft trust` must record the config: {}",
            describe(&o)
        );
    }

    fn marker(&self) -> bool {
        self.out.join("marker").exists()
    }

    fn pid_file(&self, name: &str) -> Option<i32> {
        fs::read_to_string(self.out.join(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Wait (bounded) until the stub has recorded `name`.
    fn wait_pid_file(&self, name: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(p) = self.pid_file(name) {
                return p;
            }
            assert!(
                Instant::now() < deadline,
                "stub never wrote {name} into {}",
                self.out.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn reaper(&self) -> Reaper {
        Reaper {
            out: self.out.clone(),
            extra: Vec::new(),
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

fn stdout_json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON value ({e}): {}", describe(o)))
}

/// Is `pid` alive? `kill(pid, 0)` succeeds (or fails with EPERM).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs only the existence/permission check.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Is `pid` a zombie (exited, not yet reaped)? `ps -o stat=` starts with `Z`.
/// A zombie of a killed process is dead for our purposes; its parent (the
/// stub, itself dead) hands it to launchd/init for reaping.
fn zombie(pid: i32) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_start()
                .starts_with('Z')
        })
        .unwrap_or(false)
}

/// Wait (bounded) for `pid` to be gone. Returns whether it is gone.
fn gone_within(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if !alive(pid) || zombie(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// SIGKILLs everything the stub recorded (and any extra pgids) on drop, so a
/// failing assertion or a non-cleaning implementation leaves no orphans.
struct Reaper {
    out: PathBuf,
    extra: Vec<i32>,
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
        let mut groups: Vec<i32> = self.extra.clone();
        groups.extend(read("pgid"));
        for g in groups {
            if g > 1 && g != own {
                // SAFETY: plain signal delivery to a group we created.
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

// ---------------------------------------------------------------- trust gate

/// An untrusted config's command is never run (exit 2, no marker); the same
/// config, once trusted via `sitecraft trust`, does run (positive control).
#[test]
fn untrusted_config_is_not_spawned_then_trusted_one_is() {
    let env = Env::new("serve", free_port(), 10);
    let _reaper = env.reaper();

    let o = env.run(&["serve", "start"]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "untrusted start: {}",
        describe(&o)
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !env.marker(),
        "untrusted start must not spawn the command (marker exists)"
    );

    env.trust();
    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(0), "trusted start: {}", describe(&o));
    assert!(env.marker(), "trusted start must have spawned the stub");
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
}

/// Editing the config after trusting it revokes trust: exit 2, not spawned.
#[test]
fn config_changed_after_trust_is_not_spawned() {
    let env = Env::new("serve", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();
    let cfg = env.root.join("sitecraft.toml");
    let mut body = fs::read_to_string(&cfg).unwrap();
    body.push_str("# edited after trust\n");
    fs::write(&cfg, body).unwrap();

    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(2), "changed config: {}", describe(&o));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!env.marker(), "changed config must not spawn the command");
}

// --------------------------------------------------- start / status / stop

/// Trusted + stub answers 200: start exit 0 with the state JSON, status says
/// running (exit 0), a second start is refused (exit 2), stop kills the whole
/// group including the forked grandchild, and status then says stopped.
#[test]
fn start_status_stop_kills_whole_process_group() {
    let env = Env::new("serve", free_port(), 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(0), "start: {}", describe(&o));
    let started = stdout_json(&o);
    assert_eq!(started["port"], env.port, "start JSON: {started}");
    assert_eq!(
        started["url"],
        format!("http://127.0.0.1:{}", env.port),
        "start JSON: {started}"
    );

    let leader = env.wait_pid_file("leader.pid");
    let grandchild = env.wait_pid_file("grandchild.pid");
    let pgid = env.wait_pid_file("pgid");
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    assert_ne!(
        pgid, own,
        "the dev server must run in its own process group"
    );
    assert_eq!(
        started["pgid"], pgid,
        "recorded pgid must be the stub's group"
    );

    let o = env.run(&["serve", "status"]);
    assert_eq!(o.status.code(), Some(0), "status: {}", describe(&o));
    let st = stdout_json(&o);
    assert_eq!(st["status"], "running", "status JSON: {st}");
    assert_eq!(st["pgid"], pgid, "status JSON: {st}");
    assert_eq!(st["port"], env.port, "status JSON: {st}");

    let o = env.run(&["serve", "start"]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "start while already running must be refused: {}",
        describe(&o)
    );

    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
    let stopped = stdout_json(&o);
    assert_eq!(stopped["status"], "stopped", "stop JSON: {stopped}");
    assert!(
        gone_within(leader, Duration::from_secs(3)),
        "stub leader {leader} survived stop"
    );
    assert!(
        gone_within(grandchild, Duration::from_secs(3)),
        "grandchild {grandchild} (same process group) survived stop"
    );

    let o = env.run(&["serve", "status"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "status after stop: {}",
        describe(&o)
    );
    assert_eq!(stdout_json(&o)["status"], "stopped");
}

/// No state file at all: status says stopped, stop says not-running (exit 0).
/// Positive control for the corrupt-state tests below.
#[test]
fn no_state_file_is_stopped_and_not_running() {
    let env = Env::new("serve", free_port(), 10);
    let o = env.run(&["serve", "status"]);
    assert_eq!(o.status.code(), Some(0), "status: {}", describe(&o));
    assert_eq!(stdout_json(&o)["status"], "stopped");
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
    assert_eq!(stdout_json(&o)["status"], "not-running");
}

// ------------------------------------------------------------- port taken

/// A port the test is already listening on: start exit 2 and the stub is
/// never spawned. Control: after releasing the port the same config starts.
#[test]
fn port_already_listening_is_refused_without_spawning() {
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let env = Env::new("serve", port, 10);
    let _reaper = env.reaper();
    env.trust();

    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(2), "port taken: {}", describe(&o));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!env.marker(), "port taken: the command must not be spawned");

    drop(held);
    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(0), "port freed: {}", describe(&o));
    assert!(env.marker(), "port freed: the stub must have been spawned");
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(0), "stop: {}", describe(&o));
}

// ------------------------------------------------------- ready / early exit

/// The stub never listens: the 1 s ready deadline passes, start exits 2, and
/// the spawned group (leader and grandchild) is cleaned up. No state is left
/// behind (status says stopped).
#[test]
fn ready_deadline_missed_is_undetermined_and_cleaned_up() {
    let env = Env::new("noready", free_port(), 1);
    let _reaper = env.reaper();
    env.trust();

    let t0 = Instant::now();
    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(2), "ready deadline: {}", describe(&o));
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "start must honour the 1 s ready deadline (took {:?})",
        t0.elapsed()
    );
    assert!(env.marker(), "the stub must have been spawned");
    let leader = env.wait_pid_file("leader.pid");
    let grandchild = env.wait_pid_file("grandchild.pid");
    assert!(
        gone_within(leader, Duration::from_secs(3)),
        "stub leader {leader} survived the failed start"
    );
    assert!(
        gone_within(grandchild, Duration::from_secs(3)),
        "grandchild {grandchild} survived the failed start"
    );

    let o = env.run(&["serve", "status"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "status after failed start: {}",
        describe(&o)
    );
    assert_eq!(stdout_json(&o)["status"], "stopped");
}

/// The stub exits 1 right away: start reports the early exit as exit 2
/// (well before the 30 s ready deadline) and leaves no running state.
#[test]
fn early_exit_is_undetermined() {
    let env = Env::new("exit1", free_port(), 30);
    let _reaper = env.reaper();
    env.trust();

    let t0 = Instant::now();
    let o = env.run(&["serve", "start"]);
    assert_eq!(o.status.code(), Some(2), "early exit: {}", describe(&o));
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "early exit must be detected, not waited out to the deadline (took {:?})",
        t0.elapsed()
    );
    assert!(env.marker(), "the stub must have been spawned");

    let o = env.run(&["serve", "status"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "status after early exit: {}",
        describe(&o)
    );
    assert_eq!(stdout_json(&o)["status"], "stopped");
}

// ----------------------------------------------------------- corrupt state

fn state_file(env: &Env) -> PathBuf {
    match sitecraft::serve::state_file(&env.state, &env.root) {
        sitecraft::Determination::Known(p) => p,
        sitecraft::Determination::Undetermined(why) => {
            panic!("serve::state_file must locate the state file: {why}")
        }
    }
}

fn write_state(env: &Env, body: &str) {
    let p = state_file(env);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

#[test]
fn corrupt_state_file_status_is_undetermined() {
    let env = Env::new("serve", free_port(), 10);
    write_state(&env, "{ this is not json");
    let o = env.run(&["serve", "status"]);
    assert_eq!(o.status.code(), Some(2), "status: {}", describe(&o));
}

#[test]
fn corrupt_state_file_stop_is_undetermined() {
    let env = Env::new("serve", free_port(), 10);
    write_state(&env, "{ this is not json");
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(2), "stop: {}", describe(&o));
}

/// Valid JSON of the wrong shape is as corrupt as garbage.
#[test]
fn wrong_shape_state_file_status_and_stop_are_undetermined() {
    let env = Env::new("serve", free_port(), 10);
    write_state(&env, r#"{"pgid":"not-a-number","port":1}"#);
    let o = env.run(&["serve", "status"]);
    assert_eq!(o.status.code(), Some(2), "status: {}", describe(&o));
    let o = env.run(&["serve", "stop"]);
    assert_eq!(o.status.code(), Some(2), "stop: {}", describe(&o));
}

/// A state file that names a live process group but is invalid (unknown key)
/// must not be acted on: stop exits 2 and the group is NOT killed.
#[test]
fn invalid_state_file_stop_kills_nothing() {
    use std::os::unix::process::CommandExt;

    let env = Env::new("serve", free_port(), 10);
    let mut victim = Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = i32::try_from(victim.id()).unwrap();
    let mut reaper = env.reaper();
    reaper.extra.push(pgid);

    let body = serde_json::json!({
        "pgid": pgid,
        "port": env.port,
        "url": format!("http://127.0.0.1:{}", env.port),
        "root": env.root,
        "unexpected": true,
    })
    .to_string();
    write_state(&env, &body);

    let o = env.run(&["serve", "stop"]);
    let survived = matches!(victim.try_wait(), Ok(None));
    let _ = victim.kill();
    let _ = victim.wait();
    assert_eq!(o.status.code(), Some(2), "stop: {}", describe(&o));
    assert!(
        survived,
        "stop on an invalid state file must not signal the group it names"
    );
}
