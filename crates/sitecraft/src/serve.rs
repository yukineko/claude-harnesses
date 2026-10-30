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
//! maps an error to "running" or "stopped".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Determination;

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

/// Path of the state file for `root` under `state`.
///
/// Contract: `root` that cannot be canonicalized → `Undetermined`.
pub fn state_file(state: &Path, root: &Path) -> Determination<PathBuf> {
    Determination::undetermined(format!(
        "sitecraft serve: state file location is not implemented yet (state {}, root {})",
        state.display(),
        root.display()
    ))
}

/// Start the dev server for `root`.
///
/// Contract: config not `Config` or not `Trusted` → `Undetermined` without
/// spawning; port already listening → `Undetermined` without spawning; child
/// exits before ready, or ready deadline passes → group killed, `Undetermined`;
/// already running for this root → `Undetermined`; else `Known(state)` with the
/// state file written.
pub fn start(state: &Path, root: &Path) -> Determination<ServerState> {
    Determination::undetermined(format!(
        "sitecraft serve start: not implemented yet (state {}, root {})",
        state.display(),
        root.display()
    ))
}

/// Stop the dev server for `root`.
///
/// Contract: no state file → `Known(NotRunning)`; state file unreadable or
/// invalid JSON → `Undetermined` (nothing killed); group terminated → state
/// file removed, `Known(Stopped)`; group survives SIGKILL → `Undetermined`.
pub fn stop(state: &Path, root: &Path) -> Determination<Stopped> {
    Determination::undetermined(format!(
        "sitecraft serve stop: not implemented yet (state {}, root {})",
        state.display(),
        root.display()
    ))
}

/// Status of the dev server for `root`.
///
/// Contract: no state file → `Known(Stopped)`; unreadable / invalid →
/// `Undetermined`; recorded group alive → `Known(Running)`; recorded group dead
/// → `Undetermined` (it died without `stop`; report, do not guess).
pub fn status(state: &Path, root: &Path) -> Determination<Status> {
    Determination::undetermined(format!(
        "sitecraft serve status: not implemented yet (state {}, root {})",
        state.display(),
        root.display()
    ))
}
