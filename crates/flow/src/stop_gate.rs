//! `flow stop-gate` — the Stop hook that keeps a registered /flow driver
//! session running while backlog work remains.
//!
//! Decision-bearing: the body runs under `harness_core::gate::run::run_guarded`
//! (a panic blocks the stop, bounded per session), not `run_hook`.
//!
//! Flow of one stop (see `decide` for the pure decision):
//! 1. Stop payload → session id (payload, else `CLAUDE_CODE_SESSION_ID`).
//! 2. Driver registry scan, spawning nothing: `~/.backlog/drivers/*/<slug>.driver`.
//!    No registry / no record → allow silently.
//! 3. Per registered project (steps 3-6): liveness, remaining work, circuit
//!    breaker.
//! 4. Known work + circuit continue → block (step 7); undeterminable → block
//!    once, then a visible allow on this gate's next consecutive undetermined
//!    stop (step 8; per-session marker under `~/.flow/state/stop-gate/`).

use std::io::IsTerminal;
use std::path::PathBuf;

use harness_core::config::base_dir;
use harness_core::hook::HookInput;
use serde_json::json;

use crate::decide::{self, Final, Marker, MarkerOp, Obs, Outcome};
use crate::probes::{self, Real, Registry};

/// Gate name shared with the `bin/flow` launcher's missing-binary streak.
pub const GATE: &str = "flow-stop-gate";

/// Parsed view of the Stop payload.
struct Payload {
    session: Option<String>,
    /// `None` when the payload could not be parsed at all.
    stop_hook_active: Option<bool>,
}

fn usable_session(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn main() -> ! {
    let stdin_tty = std::io::stdin().is_terminal();
    let raw = harness_core::gate::run::read_stop_payload_guarded(
        GATE,
        stdin_tty,
        harness_core::hook::try_read_stdin(),
    );
    if stdin_tty || raw.trim().is_empty() {
        eprintln!(
            "flow stop-gate: no Stop event on stdin — a decision will NOT be fabricated for a \
             manual invocation."
        );
        std::process::exit(1);
    }
    let env_session = std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .and_then(|s| usable_session(&s));
    let payload = match HookInput::parse(&raw) {
        Some(h) => Payload {
            session: usable_session(&h.session_id).or(env_session),
            stop_hook_active: Some(h.stop_hook_active),
        },
        None => Payload {
            session: env_session,
            stop_hook_active: None,
        },
    };
    let sha = payload.stop_hook_active.unwrap_or(false);
    harness_core::gate::run::run_guarded(GATE, false, sha, move || run(payload))
}

fn drivers_root() -> PathBuf {
    base_dir("backlog").join("drivers")
}

fn marker_path(session: &str) -> PathBuf {
    base_dir("flow")
        .join("state")
        .join("stop-gate")
        .join(format!("{}.undetermined", probes::session_slug(session)))
}

fn read_marker(session: Option<&str>) -> Marker {
    let Some(s) = session else {
        return Marker::Unusable("no session id to key the bound marker on".into());
    };
    let p = marker_path(s);
    match std::fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => Marker::Present,
        Ok(_) => Marker::Unusable(format!("{} is not a regular file", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Marker::Absent,
        Err(e) => Marker::Unusable(format!("cannot stat {}: {e}", p.display())),
    }
}

fn write_marker(session: Option<&str>) -> Result<(), String> {
    let s = session.ok_or("no session id to key the bound marker on")?;
    let p = marker_path(s);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    std::fs::write(&p, "undetermined\n").map_err(|e| format!("write {}: {e}", p.display()))
}

fn clear_marker(session: Option<&str>) {
    let Some(s) = session else { return };
    let p = marker_path(s);
    match std::fs::remove_file(&p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("flow stop-gate: could not clear {}: {e}", p.display()),
    }
}

/// Steps 1-6: gather and decide, before the bound is applied.
fn gather(session: Option<&str>) -> Outcome {
    let root = drivers_root();
    let Some(session) = session else {
        // No session id: if there is no driver record at all, nobody is a
        // driver and the stop is not ours to gate. Otherwise undetermined.
        return match probes::any_driver_record(&root) {
            Obs::Known(false) => Outcome::AllowSilent,
            Obs::Known(true) => Outcome::Undetermined(vec![
                "the session id (neither the Stop payload nor CLAUDE_CODE_SESSION_ID carries \
                 one, and driver records exist, so whether this session is a /flow driver is \
                 unknown)"
                    .into(),
            ]),
            Obs::Unknown(why) => Outcome::Undetermined(vec![format!(
                "the session id (absent) and the driver registry {}: {why}",
                root.display()
            )]),
        };
    };
    let projects = match probes::scan_registry(&root, session) {
        Registry::Projects(p) => p,
        Registry::Unknown(why) => {
            return Outcome::Undetermined(vec![format!(
                "the /flow driver registry {}: {why}",
                root.display()
            )])
        }
    };
    if projects.is_empty() {
        return Outcome::AllowSilent;
    }
    let mut real = Real {
        session: session.to_string(),
    };
    decide::combine(
        projects
            .iter()
            .map(|p| decide::decide_project(session, p, &mut real))
            .collect(),
    )
}

fn run(payload: Payload) -> ! {
    let session = payload.session.as_deref();
    let outcome = gather(session);
    let marker = match outcome {
        Outcome::Undetermined(_) => read_marker(session),
        _ => Marker::Absent,
    };
    let (mut fin, op) = decide::resolve(outcome, payload.stop_hook_active, &marker);
    match op {
        MarkerOp::Clear => clear_marker(session),
        MarkerOp::Write => {
            if let Err(e) = write_marker(session) {
                eprintln!("flow stop-gate: bound marker not recorded: {e}");
                fin = decide::on_write_failure(fin, payload.stop_hook_active, &e);
            }
        }
    }
    emit(fin)
}

fn emit(fin: Final) -> ! {
    let line = match fin {
        Final::Silent => None,
        Final::SystemMessage(m) => Some(json!({ "systemMessage": m })),
        Final::Block(r) => Some(json!({ "decision": "block", "reason": r })),
    };
    if let Some(v) = line {
        println!("{v}");
    }
    std::process::exit(0)
}
