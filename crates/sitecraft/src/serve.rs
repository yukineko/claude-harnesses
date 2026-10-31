//! `sitecraft serve start|stop|status` — the dev-server manager.
//!
//! `start` spawns the trusted `sitecraft.toml` `command` in its own process
//! group, after checking the port is free, and waits (with a deadline) for an
//! HTTP response on `http://127.0.0.1:<port><ready_path>`. Its state is a JSON
//! file under `<state>/serve/`, keyed by the canonical project root. `stop`
//! terminates the whole process group (SIGTERM, grace, SIGKILL, wait).
//!
//! Every failure — untrusted config, port taken, early exit, ready deadline
//! missed, corrupt state file — is `Undetermined` (exit 2). Nothing here
//! maps an error to "running" or "stopped". Every undetermined message says
//! how to fix it.
//!
//! Limitation: the state file records only the process-group id. If the
//! recorded group died without `stop` and the OS reused its id for an
//! unrelated group of the same user, `stop` cannot tell the two apart.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, Loaded};
use crate::trust::{self, Trust};
use crate::Determination;

/// Sub-directory of the state dir holding the dev-server state files.
const SERVE_DIR: &str = "serve";
/// Time the group gets between SIGTERM and SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// Time the group gets to disappear after SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// Pause between readiness probes.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound of one readiness probe's connect / IO timeout.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Persisted state of a started dev server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerState {
    /// Process-group id (= pid of the spawned leader).
    pub pgid: i32,
    /// Listening port.
    pub port: u16,
    /// Base URL, `http://127.0.0.1:<port>`.
    pub url: String,
    /// Canonical project root the server belongs to.
    pub root: PathBuf,
}

/// Answer of [`status`] (printed as JSON by `serve status`:
/// `{"status":"running",…state}` or `{"status":"stopped"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Status {
    /// A state file exists and its process group is alive.
    Running(ServerState),
    /// No state file for this root (or it was cleaned up after the group died).
    Stopped,
}

/// Answer of [`stop`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Stopped {
    /// The recorded process group was terminated and reaped.
    Stopped(ServerState),
    /// There was no state file for this root: nothing to stop.
    NotRunning,
}

/// Lift an internal `Result` into the crate's three-valued answer.
#[track_caller]
fn lift<T>(r: Result<T, String>) -> Determination<T> {
    match r {
        Ok(v) => Determination::known(v),
        Err(why) => Determination::undetermined(why),
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(root).map_err(|e| {
        format!(
            "cannot canonicalize project root {}: {e}; pass --root <existing project directory>",
            root.display()
        )
    })
}

fn state_file_for(state: &Path, canonical: &Path) -> PathBuf {
    let hex: String = Sha256::digest(canonical.as_os_str().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    state.join(SERVE_DIR).join(format!("{hex}.json"))
}

/// Path of the state file for `root` under `state`.
///
/// Contract: `root` that cannot be canonicalized → `Undetermined`.
pub fn state_file(state: &Path, root: &Path) -> Determination<PathBuf> {
    lift(canonical_root(root).map(|c| state_file_for(state, &c)))
}

/// What is on disk for a root: no state file, or a validated state.
enum OnDisk {
    Absent,
    Present(ServerState),
}

/// Read and validate the state file. NotFound → `Absent`; any other read
/// error, invalid JSON, wrong shape, unknown key, or a state that does not
/// belong to `canonical` → `Err` (nothing may be acted on).
fn read_state(path: &Path, canonical: &Path) -> Result<OnDisk, String> {
    let fix = format!(
        "inspect it; if no dev server of yours is running for this root, delete {} and retry",
        path.display()
    );
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(OnDisk::Absent),
        Err(e) => {
            return Err(format!(
                "cannot read state file {}: {e}; {fix}",
                path.display()
            ))
        }
    };
    let st: ServerState = serde_json::from_str(&text)
        .map_err(|e| format!("state file {} is invalid: {e}; {fix}", path.display()))?;
    if st.pgid <= 1 {
        return Err(format!(
            "state file {} records pgid {} which is not a dev-server group; {fix}",
            path.display(),
            st.pgid
        ));
    }
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    if st.pgid == own {
        return Err(format!(
            "state file {} records sitecraft's own process group {}; {fix}",
            path.display(),
            st.pgid
        ));
    }
    if st.root != canonical {
        return Err(format!(
            "state file {} belongs to {}, not {}; {fix}",
            path.display(),
            st.root.display(),
            canonical.display()
        ));
    }
    if st.url != base_url(st.port) {
        return Err(format!(
            "state file {} has url {:?} inconsistent with port {}; {fix}",
            path.display(),
            st.url,
            st.port
        ));
    }
    Ok(OnDisk::Present(st))
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Liveness of a process group.
#[derive(Debug, PartialEq, Eq)]
enum Group {
    Alive,
    Gone,
}

/// `killpg(pgid, 0)`: success → alive; ESRCH → gone; anything else (EPERM:
/// the id belongs to someone else) → `Err`.
fn group_liveness(pgid: i32) -> Result<Group, String> {
    // SAFETY: signal 0 performs only the existence/permission check.
    let r = unsafe { libc::killpg(pgid, 0) };
    if r == 0 {
        return Ok(Group::Alive);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ESRCH) => Ok(Group::Gone),
        _ => Err(format!(
            "cannot probe process group {pgid}: {e} (the id may now belong to another user's group)"
        )),
    }
}

/// Send `sig` to the group. ESRCH (already gone) is fine; other errors are `Err`.
fn signal_group(pgid: i32, sig: libc::c_int) -> Result<(), String> {
    // SAFETY: plain signal delivery to a validated (> 1, not our own) group.
    let r = unsafe { libc::killpg(pgid, sig) };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ESRCH) => Ok(()),
        _ => Err(format!("cannot signal process group {pgid}: {e}")),
    }
}

/// Wait (bounded) until the group is gone, reaping `child` (our own leader,
/// if we spawned it) so its zombie does not keep the group alive.
fn wait_gone(pgid: i32, child: &mut Option<&mut Child>, within: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(c) = child.as_mut() {
            c.try_wait()
                .map_err(|e| format!("cannot reap dev-server leader {pgid}: {e}"))?;
        }
        if group_liveness(pgid)? == Group::Gone {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Terminate the whole group: SIGTERM, grace, SIGKILL, wait, reap.
/// `Ok` only when the group is observed gone.
fn terminate_group(pgid: i32, mut child: Option<&mut Child>) -> Result<(), String> {
    signal_group(pgid, libc::SIGTERM)?;
    if !wait_gone(pgid, &mut child, TERM_GRACE)? {
        signal_group(pgid, libc::SIGKILL)?;
        if !wait_gone(pgid, &mut child, KILL_GRACE)? {
            return Err(format!(
                "process group {pgid} survived SIGKILL; kill it by hand (`kill -KILL -{pgid}`)"
            ));
        }
    }
    if let Some(c) = child {
        c.wait()
            .map_err(|e| format!("cannot reap dev-server leader {pgid}: {e}"))?;
    }
    Ok(())
}

/// Write the state file atomically (tmp file in the same dir, fsync, rename).
fn write_state(path: &Path, st: &ServerState) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(st).map_err(|e| format!("cannot encode the state: {e}"))?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        // Best-effort cleanup of our own tmp file; the write error is reported.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "cannot write state file {}: {e}; check that the state dir is writable",
            path.display()
        ));
    }
    Ok(())
}

/// Is `port` free on 127.0.0.1? Free only when we can bind it **and** a
/// connect is refused; anything else is in use or undetermined (`Err`).
fn check_port_free(port: u16) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let fix = format!("stop whatever listens on port {port} or change `port` in sitecraft.toml (then `sitecraft trust`)");
    match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => {
            return Err(format!(
                "port {port} is already in use (a connect succeeded); {fix}"
            ))
        }
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
        Err(e) => {
            return Err(format!(
                "cannot tell whether port {port} is free (connect probe: {e}); {fix}"
            ))
        }
    }
    match TcpListener::bind(addr) {
        Ok(l) => {
            drop(l);
            Ok(())
        }
        Err(e) => Err(format!("port {port} is not bindable ({e}); {fix}")),
    }
}

/// One HTTP probe. `Some(code)` when an HTTP status line was read; `None`
/// when nothing answered with HTTP yet (refused / reset / timeout / garbage).
/// `None` only ever means "not ready": the caller keeps waiting until its
/// deadline, so an IO error here never becomes a ready answer.
fn probe_http(port: u16, path: &str, budget: Duration) -> Option<u16> {
    let timeout = budget.min(PROBE_TIMEOUT).max(Duration::from_millis(10));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = [0u8; 64];
    let mut got = 0;
    while got < buf.len() {
        match s.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                if buf[..got].contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    parse_status_line(&buf[..got])
}

/// `HTTP/1.x NNN …` → `NNN`.
fn parse_status_line(b: &[u8]) -> Option<u16> {
    let line = std::str::from_utf8(b).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

/// Start the dev server for `root`.
///
/// Contract: config not `Config` or not `Trusted` → `Undetermined` without
/// spawning; port already listening → `Undetermined` without spawning; child
/// exits before ready, or ready deadline passes → group killed, `Undetermined`;
/// already running for this root → `Undetermined`; else `Known(state)` with the
/// state file written.
pub fn start(state: &Path, root: &Path) -> Determination<ServerState> {
    lift(start_inner(state, root))
}

fn start_inner(state: &Path, root: &Path) -> Result<ServerState, String> {
    let cfg_path = config::path_for(root);
    let (cfg, bytes) = match config::load(root) {
        Loaded::Config(c, b) => (c, b),
        Loaded::Absent => {
            return Err(format!(
                "no {} at {}; create it (command, port, ready_path, …) and run `sitecraft trust`",
                config::FILE_NAME,
                root.display()
            ))
        }
        Loaded::Undetermined(why) => {
            return Err(format!(
                "{why}; fix {} and run `sitecraft trust`",
                cfg_path.display()
            ))
        }
    };
    match trust::check_bytes(state, root, &cfg_path, &bytes) {
        Trust::Trusted => {}
        Trust::Untrusted(why) => return Err(format!("refusing to run the command: {why}")),
        Trust::Undetermined(why) => {
            return Err(format!(
                "cannot verify trust of {}: {why}; fix the trust store or run `sitecraft trust`",
                cfg_path.display()
            ))
        }
    }

    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    match read_state(&sf, &canonical)? {
        OnDisk::Absent => {}
        OnDisk::Present(st) => {
            return match group_liveness(st.pgid)? {
                Group::Alive => Err(format!(
                    "a dev server is already running for {} (pgid {}, {}); run `sitecraft serve stop` first",
                    canonical.display(),
                    st.pgid,
                    st.url
                )),
                Group::Gone => Err(format!(
                    "a stale state file {} names process group {} which is gone; run `sitecraft serve stop` to clear it",
                    sf.display(),
                    st.pgid
                )),
            }
        }
    }

    check_port_free(cfg.port)?;

    let dir = sf
        .parent()
        .ok_or_else(|| format!("state file {} has no parent dir", sf.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| {
        format!(
            "cannot create {}: {e}; check that the state dir is writable",
            dir.display()
        )
    })?;
    let log_path = sf.with_extension("log");
    let log = File::create(&log_path).map_err(|e| {
        format!(
            "cannot create the server log {}: {e}; check that the state dir is writable",
            log_path.display()
        )
    })?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("cannot duplicate the log handle: {e}"))?;

    let (prog, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| format!("`command` in {} is empty", cfg_path.display()))?;
    let mut child = Command::new(prog)
        .args(args)
        .current_dir(&canonical)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .process_group(0)
        .spawn()
        .map_err(|e| {
            format!(
                "cannot spawn {prog:?}: {e}; fix `command` in {} (then `sitecraft trust`)",
                cfg_path.display()
            )
        })?;
    let pgid = match i32::try_from(child.id()) {
        Ok(p) => p,
        Err(e) => {
            // Cannot address the group by id: kill the leader directly.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("spawned pid {} is not a pgid: {e}", child.id()));
        }
    };

    let fail = |child: &mut Child, why: String| -> String {
        match terminate_group(pgid, Some(child)) {
            Ok(()) => format!(
                "{why} (process group {pgid} terminated; log: {})",
                log_path.display()
            ),
            Err(kill_err) => format!("{why}; additionally, cleanup failed: {kill_err}"),
        }
    };

    let deadline = Instant::now() + Duration::from_secs(cfg.ready_timeout_secs);
    let mut last_code: Option<u16> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(fail(
                    &mut child,
                    format!(
                        "dev server exited before it was ready ({status}); check the log and fix `command` in {}",
                        cfg_path.display()
                    ),
                ))
            }
            Ok(None) => {}
            Err(e) => {
                return Err(fail(
                    &mut child,
                    format!("cannot observe the dev server process: {e}; retry `sitecraft serve start`"),
                ))
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let seen = match last_code {
                Some(c) => format!("last answer was HTTP {c}"),
                None => "no HTTP answer".to_string(),
            };
            return Err(fail(
                &mut child,
                format!(
                    "dev server not ready on {}{} within {}s ({seen}); raise `ready_timeout_secs` or fix `command`/`ready_path` in {} (then `sitecraft trust`)",
                    base_url(cfg.port),
                    cfg.ready_path,
                    cfg.ready_timeout_secs,
                    cfg_path.display()
                ),
            ));
        }
        if let Some(code) = probe_http(cfg.port, &cfg.ready_path, deadline - now) {
            if (200..400).contains(&code) {
                break;
            }
            last_code = Some(code);
        }
        std::thread::sleep(PROBE_INTERVAL);
    }

    // Ready — but a different process may have answered; the leader must
    // still be ours and alive.
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            return Err(fail(
                &mut child,
                format!(
                    "dev server exited ({status}) while the ready probe was answered (by another process?); free port {} and retry",
                    cfg.port
                ),
            ))
        }
        Err(e) => {
            return Err(fail(
                &mut child,
                format!("cannot observe the dev server process: {e}; retry `sitecraft serve start`"),
            ))
        }
    }

    let st = ServerState {
        pgid,
        port: cfg.port,
        url: base_url(cfg.port),
        root: canonical,
    };
    if let Err(why) = write_state(&sf, &st) {
        return Err(fail(&mut child, why));
    }
    Ok(st)
}

/// Stop the dev server for `root`.
///
/// Contract: no state file → `Known(NotRunning)`; state file unreadable or
/// invalid JSON → `Undetermined` (nothing killed); group terminated → state
/// file removed, `Known(Stopped)`; group survives SIGKILL → `Undetermined`.
pub fn stop(state: &Path, root: &Path) -> Determination<Stopped> {
    lift(stop_inner(state, root))
}

fn stop_inner(state: &Path, root: &Path) -> Result<Stopped, String> {
    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    let st = match read_state(&sf, &canonical)? {
        OnDisk::Absent => return Ok(Stopped::NotRunning),
        OnDisk::Present(st) => st,
    };
    terminate_group(st.pgid, None)?;
    std::fs::remove_file(&sf).map_err(|e| {
        format!(
            "process group {} stopped but the state file {} cannot be removed: {e}; delete it by hand",
            st.pgid,
            sf.display()
        )
    })?;
    Ok(Stopped::Stopped(st))
}

/// Status of the dev server for `root`.
///
/// Contract: no state file → `Known(Stopped)`; unreadable / invalid →
/// `Undetermined`; recorded group alive → `Known(Running)`; recorded group dead
/// → `Undetermined` (it died without `stop`; report, do not guess).
pub fn status(state: &Path, root: &Path) -> Determination<Status> {
    lift(status_inner(state, root))
}

fn status_inner(state: &Path, root: &Path) -> Result<Status, String> {
    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    let st = match read_state(&sf, &canonical)? {
        OnDisk::Absent => return Ok(Status::Stopped),
        OnDisk::Present(st) => st,
    };
    match group_liveness(st.pgid) {
        Ok(Group::Alive) => Ok(Status::Running(st)),
        Ok(Group::Gone) => Err(format!(
            "process group {} recorded in {} is gone (the dev server died without `stop`); run `sitecraft serve stop` to clear the state, then `sitecraft serve start`",
            st.pgid,
            sf.display()
        )),
        Err(why) => Err(format!(
            "{why}; inspect it, then run `sitecraft serve stop` or delete {}",
            sf.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.0 404 Not Found\r\n"), Some(404));
        assert_eq!(parse_status_line(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 abc\r\n"), None);
        assert_eq!(parse_status_line(b""), None);
    }
}
