// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FAULT INJECTION — C1 (second half): does `harness_core::store::load_json`'s
//! collapse actually change a GATE crate's VERDICT?
//!
//! `crates/stuckguard/src/state.rs:65-67`
//! ```ignore
//! pub fn load(state_dir: &Path, session: &str) -> SessionState {
//!     harness_core::store::load_json(&path(state_dir, session))
//! }
//! ```
//! and `main.rs:123-126` feeds that straight into the detector:
//! ```ignore
//! let mut st = state::load(&cfg.state_dir, &session);
//! let seq = st.push(event, cfg.window);
//! let trip = detect::detect(&st.events, &cfg);
//! ```
//! So an unreadable session file empties the window, and the detector — whose
//! whole job is "the same action N times" — sees exactly one event.
//!
//! stuckguard is a `[[bin]]`-only crate (no `[lib]`), so `state::load` and
//! `detect::detect` are NOT reachable from an integration test. Rather than
//! make them public (a production change), this drives the real `watch` hook
//! end-to-end through the built binary — which is the stronger observation
//! anyway: it shows the emitted VERDICT, not just the loaded struct.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("stuckguard-fi-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Deterministic detector config: 3 identical actions trip a repeat, and no
    // cooldown suppression so the message is emitted the moment it trips.
    std::fs::write(
        dir.join("stuckguard.toml"),
        "repeat_threshold = 3\ncooldown_events = 0\nescalate_after = 2\n",
    )
    .unwrap();
    dir
}

/// The per-session state file `state::path()` resolves to under `HOME=home`.
fn session_state_path(home: &Path, session: &str) -> PathBuf {
    home.join(".stuckguard")
        .join("state")
        .join("sessions")
        .join(format!("{session}.json"))
}

/// One PostToolUse `watch` invocation in an isolated HOME/CWD.
fn watch(home: &Path, session: &str, payload: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stuckguard"))
        .arg("watch")
        .current_dir(home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.replace("$SESSION", session).as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The same Bash call, over and over — the textbook stuck loop.
const LOOPING_EVENT: &str = r#"{"hook_event_name":"PostToolUse","session_id":"$SESSION","tool_name":"Bash","tool_input":{"command":"cargo build --release"}}"#;

/// CONTROL (must be GREEN): with a readable state file, three identical Bash
/// calls trip the repeat detector and the hook emits a nudge. This pins the
/// fact that the test reaches the verdict path at all — without it, a silent
/// third call in the fault case would prove nothing.
#[test]
fn control_three_identical_calls_emit_a_nudge() {
    let home = temp_home("control");
    let (c1, o1) = watch(&home, "s1", LOOPING_EVENT);
    let (c2, o2) = watch(&home, "s1", LOOPING_EVENT);
    let (c3, o3) = watch(&home, "s1", LOOPING_EVENT);
    assert_eq!((c1, c2, c3), (0, 0, 0), "watch must always exit 0");
    assert!(o1.trim().is_empty(), "1st call: no trip yet, got {o1}");
    assert!(o2.trim().is_empty(), "2nd call: no trip yet, got {o2}");
    assert!(
        o3.contains("stuckguard"),
        "3rd identical call MUST trip the repeat detector, got: {o3:?}"
    );
}

/// C1 VERDICT-LEVEL FAULT INJECTION: same three calls, but the session state
/// file is made unreadable (chmod 0o000) after the first two are recorded.
/// `state::load` -> `load_json` swallows the EACCES and returns
/// `SessionState::default()`, so the detector's window holds only the third
/// event and the stuck loop becomes invisible.
///
/// "I cannot read the history" is being emitted as "no stuck loop here".
#[cfg(unix)]
#[test]
fn unreadable_session_state_silences_the_stuck_verdict() {
    use std::os::unix::fs::PermissionsExt;
    let home = temp_home("unreadable");
    watch(&home, "s1", LOOPING_EVENT);
    watch(&home, "s1", LOOPING_EVENT);

    let state = session_state_path(&home, "s1");
    assert!(
        state.exists(),
        "precondition: state file {} must exist after two events",
        state.display()
    );
    let before = std::fs::read_to_string(&state).unwrap();
    // The persisted event carries a normalized `sig` hash, not the raw command,
    // so assert on the sequence counter instead of the command text.
    assert!(
        before.contains("\"seq\":2"),
        "precondition: two real events recorded, got {before}"
    );

    let mut perms = std::fs::metadata(&state).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&state, perms).unwrap();

    let (code, out) = watch(&home, "s1", LOOPING_EVENT);

    // Restore BEFORE asserting so a failing assert never leaves an
    // unreadable dir behind (mirrors specguard/src/decision.rs:183-203).
    let mut restore = std::fs::metadata(&state).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&state, restore);

    assert_eq!(code, 0, "watch must always exit 0");
    assert!(
        out.contains("stuckguard"),
        "FAIL-OPEN IN A GATE CRATE: the session history was UNREADABLE, so \
         state::load returned SessionState::default() and detect() saw a \
         1-event window. The third identical `cargo build --release` produced \
         NO verdict at all (stdout={out:?}). 'I cannot read the history' was \
         emitted as 'no stuck loop'."
    );
}

/// Same, via an unsearchable PARENT dir rather than the file itself — the
/// shape a wrong-umask / foreign-uid state dir actually takes in the field.
#[cfg(unix)]
#[test]
fn unsearchable_state_dir_silences_the_stuck_verdict() {
    use std::os::unix::fs::PermissionsExt;
    let home = temp_home("unsearchable");
    watch(&home, "s1", LOOPING_EVENT);
    watch(&home, "s1", LOOPING_EVENT);

    let state = session_state_path(&home, "s1");
    let dir = state.parent().unwrap().to_path_buf();
    assert!(state.exists(), "precondition: state file must exist");

    let mut perms = std::fs::metadata(&dir).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&dir, perms).unwrap();

    let (code, out) = watch(&home, "s1", LOOPING_EVENT);

    let mut restore = std::fs::metadata(&dir).unwrap().permissions();
    restore.set_mode(0o700);
    let _ = std::fs::set_permissions(&dir, restore);

    assert_eq!(code, 0, "watch must always exit 0");
    assert!(
        out.contains("stuckguard"),
        "FAIL-OPEN IN A GATE CRATE: an UNSEARCHABLE session-state dir erased \
         the window and the stuck loop went unreported (stdout={out:?})."
    );
}

/// Corrupt (readable but unparseable) state — schema drift / truncated write.
/// `serde_json::from_str(..).ok()` drops it to `default()` just the same.
#[test]
fn corrupt_session_state_silences_the_stuck_verdict() {
    let home = temp_home("corrupt");
    watch(&home, "s1", LOOPING_EVENT);
    watch(&home, "s1", LOOPING_EVENT);

    let state = session_state_path(&home, "s1");
    assert!(state.exists(), "precondition: state file must exist");
    std::fs::write(&state, "{\"seq\":2,\"events\":[{\"trunc").unwrap();

    let (code, out) = watch(&home, "s1", LOOPING_EVENT);
    assert_eq!(code, 0, "watch must always exit 0");
    assert!(
        out.contains("stuckguard"),
        "FAIL-OPEN IN A GATE CRATE: CORRUPT session state parsed as a fresh \
         session, so the stuck loop went unreported (stdout={out:?})."
    );
}
