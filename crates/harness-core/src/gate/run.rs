//! Stop-hook entry helpers shared by the gates: the fail-closed panic barrier
//! (whose only allow is a gate's second CONSECUTIVE panic), the session-scoped
//! one-shot skip, and the `max_attempts` concession.
//!
//! The last two are the same shape and exist for the same reason: a Stop is
//! adjudicated by four independent processes, so a hatch one gate spends can be
//! consumed by a stop another gate then blocks. Both are therefore bounded by
//! `stop_hook_active` — honoured while the chain is still being blocked, spent
//! on the first stop that actually completes.

use std::path::{Path, PathBuf};

use crate::verdict::{Determination, Required};

/// What a panicking gate body resolves to, once the run mode and this gate's
/// panic history are known. Pulled out of [`run_guarded`] so the policy is
/// unit-testable without actually exiting the process.
#[derive(Debug, PartialEq, Eq)]
enum PanicAction {
    /// Manual/interactive run: surface the crash on stderr and exit 1. No stop
    /// decision is emitted (there is no live turn to block).
    InteractiveError,
    /// Real Stop hook: the gate crashed before it could decide, so it cannot
    /// certify the stop is safe. Emit a **block** decision (fail closed) and
    /// exit 0. Taken on the first stop of a chain, and on a re-entry
    /// (`stop_hook_active`) whose PREVIOUS stop of this same gate did not panic
    /// — `stop_hook_active` is set whenever ANY gate blocked, so on its own it
    /// says nothing about this gate.
    FailClosedBlock,
    /// Real Stop hook, re-entry, and this gate's previous stop in this session
    /// was ALSO a panic (its marker was read back). This is the second
    /// consecutive crash: the first already surfaced a fail-closed block, and
    /// blocking again would trap the session, so the stop is allowed.
    BoundedAllow,
    /// Real Stop hook, re-entry, and whether this gate's previous stop also
    /// panicked could NOT be determined — no usable session id, the marker
    /// could not be read or cleared, or this panic could not be recorded. A
    /// block here would be unbounded (the next re-entry would be just as
    /// blind, so a deterministic crash plus a persistently broken store would
    /// loop forever), so the stop is allowed — but never silently: a
    /// `systemMessage` tells the user this gate did not check the stop, and
    /// why. Carries that reason.
    UndeterminedAllow(String),
}

/// The record "this gate's previous stop in this session was a panic":
/// `<base_dir("harness")>/state/panic-markers/<gate>/<session>.panic`.
///
/// Presence is the whole signal. It is cleared at the start of every hook-mode
/// run ([`PanicMarker::take`]) and re-written only when that run panics, so a
/// run that ends any other way — a clean allow, a block, an operator escape,
/// all of which `process::exit` and never return to the guard — leaves no
/// marker behind and the next panic counts as a FIRST panic again.
struct PanicMarker {
    path: PathBuf,
}

impl PanicMarker {
    /// Locate the marker for gate `name` in the current session. The session
    /// is `CLAUDE_CODE_SESSION_ID` (the run_guarded signature carries no
    /// payload); without a usable one there is no per-session key, which is
    /// undetermined — not "no previous panic".
    fn locate(name: &str) -> Determination<PanicMarker> {
        let session = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        if !usable_session_id(&session) {
            return Determination::undetermined(
                "panic-streak marker: no usable session id (CLAUDE_CODE_SESSION_ID unset or \
                 unusable), so this gate's previous-stop panic cannot be looked up",
            );
        }
        if !usable_session_id(name) {
            return Determination::undetermined(format!(
                "panic-streak marker: gate name {name:?} is not a usable path component"
            ));
        }
        Determination::known(PanicMarker {
            path: crate::config::base_dir("harness")
                .join("state")
                .join("panic-markers")
                .join(name)
                .join(format!("{session}.panic")),
        })
    }

    /// Read-and-clear: did this gate's previous stop panic? Clears the marker
    /// so a run that does not panic resets the streak. Absent → `false`; any
    /// other IO outcome (unreadable, or present but not removable — a marker
    /// that cannot be cleared would make a later FIRST panic look like a
    /// second one) → undetermined.
    fn take(&self) -> Determination<bool> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => match std::fs::remove_file(&self.path) {
                Ok(()) => Determination::known(true),
                Err(e) => Determination::undetermined(format!(
                    "panic-streak marker {} exists but could not be cleared: {e}",
                    self.path.display()
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(false),
            Err(e) => Determination::undetermined(format!(
                "panic-streak marker {} could not be read: {e}",
                self.path.display()
            )),
        }
    }

    /// Record that this run panicked.
    fn record(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&self.path, b"panicked\n")
            .map_err(|e| format!("could not write {}: {e}", self.path.display()))
    }
}

/// The panic policy, given the run mode, this gate's previous-stop panic
/// state, and a way to record the current panic. Pure apart from `record`.
///
/// * interactive → [`PanicAction::InteractiveError`].
/// * first stop of a chain (`!stop_hook_active`) → record, then
///   [`PanicAction::FailClosedBlock`] regardless of the record's outcome (the
///   re-entry that follows handles a store that could not record it).
/// * re-entry, previous stop of this gate panicked → record (keeps the streak
///   across further re-entries), [`PanicAction::BoundedAllow`].
/// * re-entry, previous stop did not panic → this is a FIRST panic: block, but
///   only if it was recorded. If it could not be recorded, the next re-entry
///   would see "no previous panic" again and block forever, so the outcome is
///   [`PanicAction::UndeterminedAllow`] instead.
/// * re-entry, previous state undetermined → [`PanicAction::UndeterminedAllow`].
///
/// Termination: on any chain of re-entries in which this gate keeps panicking,
/// a block is only ever emitted after a marker was written, and the next
/// re-entry either reads it (allow) or cannot (loud allow) — at most one block
/// per streak once the chain is re-entering.
fn decide(
    interactive: bool,
    stop_hook_active: bool,
    prev: Determination<bool>,
    record: impl FnOnce() -> Result<(), String>,
) -> PanicAction {
    if interactive {
        return PanicAction::InteractiveError;
    }
    if !stop_hook_active {
        if let Err(e) = record() {
            eprintln!("panic-streak marker not recorded ({e}); the next re-entry cannot see it");
        }
        return PanicAction::FailClosedBlock;
    }
    match prev.require() {
        Required::Determined(true) => {
            if let Err(e) = record() {
                eprintln!("panic-streak marker not recorded ({e})");
            }
            PanicAction::BoundedAllow
        }
        Required::Determined(false) => match record() {
            Ok(()) => PanicAction::FailClosedBlock,
            Err(e) => {
                let why = format!(
                    "this first panic on a re-entry could not be recorded ({e}), so a block \
                     here could never be bounded"
                );
                crate::undetermined::record(&why, std::panic::Location::caller());
                PanicAction::UndeterminedAllow(why)
            }
        },
        Required::Blocked(why) => PanicAction::UndeterminedAllow(why.as_str().to_string()),
    }
}

/// Run a Stop-hook body under the fail-closed panic barrier.
///
/// `body` is the gate logic; it returns `!` because it always ends in a
/// `process::exit`. A real `process::exit` inside `body` terminates the process
/// directly and never unwinds here — so only a genuine *panic* reaches this
/// guard. A panic means the gate's own logic crashed *before* it emitted any
/// allow/block decision (the decision paths `process::exit`, which never
/// unwinds), so the check did not run and its verdict is unknown. We resolve
/// that unknown to the restrictive side rather than silently letting the turn
/// end unchecked:
///   * hook mode (`interactive == false`), first stop (`stop_hook_active ==
///     false`) → emit a `{"decision":"block"}` decision on stdout and exit 0
///     (**fail closed**: block the stop and surface the crash, since a crashed
///     gate cannot certify the stop is safe).
///   * hook mode, re-entry (`stop_hook_active == true`) → allow the stop (exit
///     0) ONLY when this same gate's previous stop in this session was also a
///     panic — the second consecutive crash (CLAUDE.md §1). `stop_hook_active`
///     alone is not enough: Claude Code sets it whenever ANY gate blocked, so a
///     gate panicking for the first time while another gate's block is being
///     worked off is still a first panic and blocks. The streak is a
///     per-gate, per-session marker ([`PanicMarker`]) cleared at the start of
///     every hook-mode run, so a run that does not panic resets it.
///   * hook mode, re-entry, and the streak cannot be determined (no
///     `CLAUDE_CODE_SESSION_ID`, marker IO failure) → allow with a
///     `systemMessage` on stdout naming the gate and the reason. Blocking would
///     be unbounded there (see [`decide`]); the first stop of a chain still
///     blocks.
///   * interactive/manual mode → print `<name>: internal error` and exit 1.
///
/// Historically hook mode swallowed the panic and exited 0 (allow) — but an
/// exit-0-with-no-decision is *indistinguishable from a passing gate*, so a
/// crashing gate silently let every stop through. That is exactly the
/// "cannot-determine collapsed into allow" fail-open this repo forbids.
///
/// `body` is wrapped in `AssertUnwindSafe`: on a panic we exit the process
/// immediately (only a stdout decision line + exit — never observing the
/// possibly-inconsistent captured state), so unwind-safety is not a concern.
///
/// `body` returns `!` in practice (it always ends in `process::exit`), making
/// the inferred `R` the never type; the signature stays generic over `R` so the
/// `!` type need not be named.
///
/// **Caller contract (load-bearing since this fails closed):** because a panic
/// now *blocks* the stop, `body` MUST evaluate its panic-free operator escapes —
/// the `disabled` toggles and the `consume_session_skip(state_dir, session)` skip —
/// *before* any panic-prone verification (config is fail-soft; the checkers /
/// git / subprocess work is not). Otherwise a deterministically-crashing gate
/// would be unescapable: the operator's skip marker or `enabled = false` would be
/// dead code behind the crash. The `tests/gate_escape_ordering.rs` guard pins
/// this ordering across all four gates. (The `give-up`/`max_attempts` hatch may
/// stay after `evaluate`; the *crash* case is instead bounded by the
/// consecutive-panic `BoundedAllow` above.)
pub fn run_guarded<R, F: FnOnce() -> R>(
    name: &str,
    interactive: bool,
    stop_hook_active: bool,
    body: F,
) -> R {
    // Interactive runs have no session to key a streak on and never allow, so
    // the marker is only touched in hook mode.
    let marker = (!interactive).then(|| PanicMarker::locate(name));
    // Read-and-clear BEFORE the body: the body never returns (it exits), so
    // clearing up front is the only way a non-panicking run resets the streak.
    let prev = match &marker {
        Some(Determination::Known(m)) => m.take(),
        Some(Determination::Undetermined(why)) => {
            // Forward the reason without re-recording it (already recorded
            // when `locate` minted it).
            Determination::Undetermined(why.clone())
        }
        None => Determination::known(false),
    };
    let record = || match &marker {
        Some(Determination::Known(m)) => m.record(),
        Some(Determination::Undetermined(why)) => Err(why.as_str().to_string()),
        None => Ok(()),
    };
    match guard(interactive, stop_hook_active, prev, record, body) {
        Ok(value) => value,
        Err(action) => panic_exit(name, action),
    }
}

/// Testable core of [`run_guarded`]: run `body`, returning its value on success
/// or the [`PanicAction`] ([`decide`]) to take if it panicked. Never exits, so
/// the policy can be asserted directly.
fn guard<R, F: FnOnce() -> R>(
    interactive: bool,
    stop_hook_active: bool,
    prev: Determination<bool>,
    record: impl FnOnce() -> Result<(), String>,
    body: F,
) -> Result<R, PanicAction> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(v) => Ok(v),
        Err(_) => Err(decide(interactive, stop_hook_active, prev, record)),
    }
}

/// The fail-closed block decision emitted when a gate body panics and this is
/// not its second consecutive panic: a serialized
/// `{"decision":"block","reason":...}` line (the gates' own block protocol).
/// Pure — split out so its shape/reason is unit-testable without exiting the
/// process.
fn fail_closed_block_json(name: &str) -> String {
    let reason = format!(
        "{name}: internal error — the gate crashed before it could run, so this \
         stop is blocked as fail-closed (the check did not execute; its result \
         is unknown). Address the cause and continue; if {name} crashes again on \
         its very next stop it is allowed through, so the session is never trapped."
    );
    serde_json::json!({ "decision": "block", "reason": reason }).to_string()
}

/// The non-silent allow emitted for [`PanicAction::UndeterminedAllow`]: a
/// `{"systemMessage": ...}` line, so the user sees that this gate did not
/// check the stop and why the panic streak could not be determined.
fn undetermined_allow_json(name: &str, why: &str) -> String {
    let msg = format!(
        "{name}: internal error on a stop re-entry — the gate crashed and did NOT check \
         this stop. Whether its previous stop also crashed could not be determined \
         ({why}), and blocking without that record could loop forever, so this stop is \
         allowed. Treat this stop as unchecked by {name}."
    );
    serde_json::json!({ "systemMessage": msg }).to_string()
}

/// Carry out a [`PanicAction`]: emit the appropriate diagnostic/decision and
/// exit. Split from [`run_guarded`] so the policy (in [`guard`]) stays pure.
fn panic_exit(name: &str, action: PanicAction) -> ! {
    match action {
        PanicAction::InteractiveError => {
            eprintln!("{name}: internal error");
            std::process::exit(1);
        }
        PanicAction::FailClosedBlock => {
            // Fail closed: the gate crashed before deciding, so it cannot vouch
            // that this stop is safe. Block it (the gates' own block protocol:
            // a `decision:block` JSON on stdout, exit 0) rather than let the
            // turn end unchecked. The consecutive-panic marker bounds this.
            println!("{}", fail_closed_block_json(name));
            std::process::exit(0);
        }
        PanicAction::BoundedAllow => {
            // Second consecutive crash of THIS gate (its marker from the
            // previous stop was read back). The first crash already surfaced a
            // fail-closed block; blocking again would trap the session, so
            // allow the stop (bounded fail-open). Still surface it on stderr.
            eprintln!(
                "{name}: internal error again on its consecutive stop — allowing \
                 this stop to avoid trapping the session (a fail-closed block was \
                 already surfaced for the previous crash)."
            );
            std::process::exit(0);
        }
        PanicAction::UndeterminedAllow(why) => {
            // Bounded but never silent: the user is told on stdout
            // (systemMessage) and the hook log gets the same on stderr.
            println!("{}", undetermined_allow_json(name, &why));
            eprintln!("{name}: internal error on stop re-entry; panic streak undetermined ({why}) — allowing loudly");
            std::process::exit(0);
        }
    }
}

/// Why a session-scoped skip could not be issued.
#[derive(Debug, PartialEq, Eq)]
pub enum SkipIssueError {
    /// No session to attribute the skip to. The caller could not read
    /// `CLAUDE_CODE_SESSION_ID`, so the skip would have to be filed under a
    /// placeholder — which is the unattributable shared marker this API exists
    /// to remove. Refused rather than guessed.
    NoSession,
    /// The session id is not a usable single path component (empty, or carrying
    /// a separator or `..`). Accepting it would let the marker be written
    /// outside the state dir, or under a name another session also computes.
    UnusableSessionId,
    /// A skip must say WHY. An unexplained bypass is invisible to review even
    /// when it is recorded.
    EmptyReason,
    /// The marker could not be written.
    Io(String),
}

impl std::fmt::Display for SkipIssueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipIssueError::NoSession => write!(
                f,
                "no session id (CLAUDE_CODE_SESSION_ID is unset) — a skip must be attributable to \
the session that asked for it"
            ),
            SkipIssueError::UnusableSessionId => write!(
                f,
                "session id is not a usable path component (empty, or contains a separator or `..`)"
            ),
            SkipIssueError::EmptyReason => {
                write!(
                    f,
                    "a skip requires a reason; refusing to record an unexplained bypass"
                )
            }
            SkipIssueError::Io(e) => write!(f, "could not record the skip: {e}"),
        }
    }
}

/// True when `session_id` is a single, safe path component that identifies ONE
/// session.
///
/// `"_local"` is rejected on purpose. It is what `HookInput::session_key`
/// substitutes when the payload carries no session id, so it is the SAME key
/// for every such run — keying a skip on it would rebuild the shared marker
/// this API exists to delete, under a new name. "Which session is this?" being
/// unanswerable resolves to "no skip" (CLAUDE.md §3), not to a skip everyone
/// shares.
fn usable_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id != "."
        && session_id != ".."
        && session_id != "_local"
        && !session_id.contains('/')
        && !session_id.contains('\\')
        && !session_id.contains('\0')
}

/// Where the one-shot skip for `session_id` lives: `<state_dir>/skips/<id>.skip`.
fn session_skip_path(state_dir: &Path, session_id: &str) -> std::path::PathBuf {
    state_dir.join("skips").join(format!("{session_id}.skip"))
}

/// Issue a one-shot, reason-required skip **for `session_id` only**.
///
/// This replaces the project-root marker files (`.donegate-skip` and friends)
/// that `consume_skip` used to read. Those sat in the SHARED project root, so
/// whichever session's Stop hook fired next consumed them — waving that
/// session's legitimate gate through on an exception someone else asked for.
/// CLAUDE.md §5 names that mechanism as forbidden under parallel sessions, and
/// it was the only in-session hatch that existed: the documented env-var
/// alternatives never reach a Stop hook (the hook is a child of the Claude Code
/// app and inherits ITS environment, not the Bash tool's), and
/// `~/.claude/settings.json` is permission-denied for editing. An escape route
/// that only exists in a forbidden form is what turns gate work into a game of
/// getting past the gate.
///
/// The replacement is STRICTER, not looser: an unattributable, unexplained,
/// unrecorded shared file becomes an attributed, reason-carrying, recorded and
/// session-limited one. Every refusal here resolves the undeterminable case to
/// "no skip", per CLAUDE.md §3.
pub fn issue_session_skip(
    state_dir: &Path,
    session_id: &str,
    reason: &str,
) -> Result<std::path::PathBuf, SkipIssueError> {
    if session_id.is_empty() {
        return Err(SkipIssueError::NoSession);
    }
    if !usable_session_id(session_id) {
        return Err(SkipIssueError::UnusableSessionId);
    }
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(SkipIssueError::EmptyReason);
    }
    let path = session_skip_path(state_dir, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SkipIssueError::Io(e.to_string()))?;
    }
    std::fs::write(&path, format!("{reason}\n")).map_err(|e| SkipIssueError::Io(e.to_string()))?;
    append_jsonl(
        state_dir,
        &serde_json::json!({
            "event": "skip_issued",
            "session_id": session_id,
            "reason": reason,
        }),
    );
    Ok(path)
}

/// The `<gate> skip --reason "…"` CLI action, shared by every gate so the four
/// cannot drift apart — one capability, one implementation. (Two rules for one
/// capability is how this repo's mirrors keep diverging.)
///
/// The session is read from `CLAUDE_CODE_SESSION_ID`, which is measured to be
/// the same value a Stop hook receives as `session_id` (observed 2026-09-08:
/// the env var and `~/.donegate/state/log.jsonl`'s `session_id` agreed). If it
/// is unset there is no session to attribute the skip to, and the request is
/// refused rather than filed under a shared placeholder.
pub fn skip_command(gate: &str, state_dir: &Path, reason: &str) -> Result<(), SkipIssueError> {
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    issue_session_skip(state_dir, &session, reason)?;
    eprintln!(
        "{gate}: one-shot skip recorded for session {session}\n  reason: {}\n  \
It applies to THIS session's next stop only; no other session can consume it, \
and the consumption is written to the gate's log.",
        reason.trim()
    );
    Ok(())
}

/// Where an already-honoured skip is parked until the stop it authorised
/// actually completes: `<state_dir>/skips/<id>.skip.honoured`.
fn honoured_session_skip_path(state_dir: &Path, session_id: &str) -> std::path::PathBuf {
    state_dir
        .join("skips")
        .join(format!("{session_id}.skip.honoured"))
}

/// Consume the one-shot skip belonging to `session_id`, if there is one.
///
/// Returns the reason it carried while the escape is still owed, and `None`
/// once it has been spent — or when it cannot be honoured. A skip issued by a
/// DIFFERENT session is not visible here and is left untouched — that is the
/// whole point, and it is what the shared marker could not do. Consumption is
/// written to `log.jsonl` by this function rather than by each caller, so a
/// gate cannot consume a bypass without leaving a record.
///
/// # The skip is spent by a completed stop, not by this gate's own verdict
///
/// A Stop is adjudicated by four independent processes (donegate, reviewgate,
/// tdd, propguard), each holding only its own verdict. Session-scoping the
/// marker fixed *whose* skip gets consumed; it did not fix *when*. This
/// function used to delete the marker the instant it read it, so when donegate
/// honoured the session's skip and allowed while reviewgate blocked the same
/// stop, **no stop happened** and the operator's one-shot escape had been spent
/// on nothing: they had to re-issue it on every re-entry, for every gate, until
/// the four-way conjunction went green at once — the standing pressure toward a
/// *permanent* bypass that CLAUDE.md 第5節 forbids.
///
/// `stop_hook_active` is the seam that fixes it. Claude Code sets it on the
/// stop that follows a block ([`crate::hook::HookInput`]), so it distinguishes
/// "this stop is a re-entry after somebody blocked" from "this stop starts a
/// fresh chain". The lifecycle is therefore:
///
/// 1. **Skip present** — honour it and `rename` it to `<id>.skip.honoured`.
///    The escape is now *owed*: it has authorised a stop that has not completed.
/// 2. **Only `<id>.skip.honoured` present, `stop_hook_active == true`** — some
///    gate blocked the stop this skip authorised, so the stop never happened.
///    Keep honouring it; the escape is still owed.
/// 3. **Only `<id>.skip.honoured` present, `stop_hook_active == false`** — the
///    chain ended, so a stop this skip authorised did complete. Delete the
///    record and return `None`. This is what keeps the escape one-shot rather
///    than permanent.
///
/// A `rename` (rather than a second bookkeeping file) is used so that a skip
/// the operator *re-issues* is never mistaken for an outstanding honour: the
/// two states have different filenames and step 1 always wins.
///
/// # Failure modes, all resolved to the restrictive side
///
/// * Marker unreadable → `None` (not honoured). A token we cannot read is not a
///   token we may act on (CLAUDE.md 第3節).
/// * `rename` fails → the honour cannot be recorded, so it cannot be bounded.
///   Fall back to `remove_file`, honouring it exactly once. If that fails too,
///   the skip can be neither bounded nor cleared, so it is **not** honoured.
/// * Empty marker → `None`, and the file is removed: `issue_session_skip`
///   refuses an empty reason, so a blank file is hand-made and must not become
///   a silent, permanent hatch.
pub fn consume_session_skip(
    state_dir: &Path,
    session_id: &str,
    stop_hook_active: bool,
) -> Option<String> {
    if !usable_session_id(session_id) {
        return None;
    }
    let path = session_skip_path(state_dir, session_id);
    let honoured = honoured_session_skip_path(state_dir, session_id);

    let record = |reason: &str, reentry: bool| {
        append_jsonl(
            state_dir,
            &serde_json::json!({
                "event": "skip_consumed",
                "session_id": session_id,
                "reason": reason,
                // True when this honour is a re-entry after some gate blocked
                // the stop the skip authorised — the same escape, not a second
                // one. Without this field the log would read as N bypasses.
                "reentry": reentry,
            }),
        );
    };

    // (1) A skip nobody has honoured yet — or one the operator has just
    // re-issued, which supersedes any stale record.
    if path.exists() {
        let reason = std::fs::read_to_string(&path).ok()?.trim().to_string();
        if reason.is_empty() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        if std::fs::rename(&path, &honoured).is_err() && std::fs::remove_file(&path).is_err() {
            // Neither bounded nor cleared: honouring it now would create a
            // standing bypass, so do not honour it.
            return None;
        }
        record(&reason, false);
        return Some(reason);
    }

    if !honoured.exists() {
        return None;
    }

    // (2) The stop this skip authorised was blocked by some other gate, so it
    // never happened: the escape is still owed.
    if stop_hook_active {
        let reason = std::fs::read_to_string(&honoured).ok()?.trim().to_string();
        if reason.is_empty() {
            return None;
        }
        record(&reason, true);
        return Some(reason);
    }

    // (3) A stop this skip authorised completed. Spend it.
    let _ = std::fs::remove_file(&honoured);
    None
}

fn concession_path(state_dir: &Path, session_id: &str) -> std::path::PathBuf {
    state_dir
        .join("concessions")
        .join(format!("{session_id}.giveup"))
}

/// Record that this gate exhausted `max_attempts` and ALLOWED the stop.
///
/// # Why a concession has to be remembered
///
/// `max_attempts` is a deliberate, bounded fail-open: after N consecutive
/// blocks the gate stops enforcing so a genuinely stuck agent is never trapped.
/// The concession is earned — the operator paid N blocked stops for it.
///
/// But a Stop is adjudicated by **four independent processes** (donegate,
/// reviewgate, propguard, tdd), each seeing only its own verdict. When donegate
/// gives up and allows while tdd blocks the same stop, **the stop does not
/// happen** — and the old code had just called `state::reset`, erasing the
/// counter. On the re-entry donegate counts from 1 again and blocks, so the
/// agent must pay another N blocked stops for a concession it already earned,
/// every time some other gate is still red. That is the same defect the skip
/// token had ([`consume_session_skip`]), on the same seam, and it is unbounded:
/// while another gate keeps blocking, the concession can never be collected.
///
/// Call this INSTEAD of `state::reset` on the give-up path. It clears the
/// attempt counter exactly as `reset` did (the file the counter lives in is
/// untouched by this function — the caller still resets it) and additionally
/// leaves a marker that [`concession_owed`] can find on the re-entry.
pub fn concede(state_dir: &Path, session_id: &str) {
    if !usable_session_id(session_id) {
        return;
    }
    let path = concession_path(state_dir, session_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Content is informational only; presence is the signal. Written as one
    // atomic `write` so a concurrent reader never sees a half-file.
    let _ = std::fs::write(&path, b"gave up after max_attempts\n");
    append_jsonl(
        state_dir,
        &serde_json::json!({
            "event": "concession_recorded",
            "session_id": session_id,
        }),
    );
}

/// Is a give-up this gate already made still owed?
///
/// Mirrors [`consume_session_skip`]'s three states, for the same reason and on
/// the same seam:
///
/// 1. **No marker** → nothing was conceded. `false`; enforce normally.
/// 2. **Marker + `stop_hook_active`** → Claude Code re-entered after *some*
///    gate blocked, so the stop this concession authorised never happened. The
///    concession is **still owed**: `true`, and the marker stays.
/// 3. **Marker + not a re-entry** → a stop this concession authorised
///    completed. Spend it: delete the marker and return `false`.
///
/// State 3 is what keeps this bounded. Without it "remember the give-up" would
/// become a permanent bypass — the gate would never enforce again for the rest
/// of the session, which is strictly worse than the defect being fixed.
///
/// A marker that exists but cannot be removed is **not** honoured (`false`):
/// honouring it would be unbounded, since the same failure blocks state 3 from
/// ever spending it. Undeterminable → restrictive side (CLAUDE.md 第3節).
#[must_use]
pub fn concession_owed(state_dir: &Path, session_id: &str, stop_hook_active: bool) -> bool {
    if !usable_session_id(session_id) {
        return false;
    }
    let path = concession_path(state_dir, session_id);
    if !path.exists() {
        return false;
    }
    if stop_hook_active {
        append_jsonl(
            state_dir,
            &serde_json::json!({
                "event": "concession_still_owed",
                "session_id": session_id,
            }),
        );
        return true;
    }
    // A stop this concession authorised completed. Spend it.
    if std::fs::remove_file(&path).is_err() {
        // Cannot clear it, so state 3 can never fire for it either: honouring
        // it now would hand out an unbounded bypass instead of a one-stop one.
        append_jsonl(
            state_dir,
            &serde_json::json!({
                "event": "concession_unclearable",
                "session_id": session_id,
            }),
        );
        return false;
    }
    append_jsonl(
        state_dir,
        &serde_json::json!({
            "event": "concession_spent",
            "session_id": session_id,
        }),
    );
    false
}

/// Append `entry` as one JSON line to `<state_dir>/log.jsonl`, creating the
/// directory if needed. The shared event-log sink for the Stop gates
/// (donegate/reviewgate/tdd): each builds its own crate-specific `entry`, this
/// owns the write. Best-effort — a serialization or IO failure is swallowed,
/// since an observability log must never break the turn it records.
pub fn append_jsonl(state_dir: &Path, entry: &serde_json::Value) {
    let path = state_dir.join("log.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(line) = serde_json::to_string(entry) {
        // Single atomic append (body + '\n' in one write) — see issue #15.
        crate::append::append_line_reporting(&path, &line, "gate run log");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Result<(), String> {
        Ok(())
    }

    fn fail() -> Result<(), String> {
        Err("disk full".to_string())
    }

    fn no_prev() -> Determination<bool> {
        Determination::known(false)
    }

    fn panicked_prev() -> Determination<bool> {
        Determination::known(true)
    }

    #[test]
    fn guard_passes_value_through() {
        // A clean body's value is returned regardless of mode / re-entry flag.
        assert_eq!(guard(true, false, no_prev(), ok, || 7), Ok(7));
        assert_eq!(guard(false, false, no_prev(), ok, || 7), Ok(7));
        assert_eq!(guard(false, true, panicked_prev(), ok, || 7), Ok(7));
    }

    #[test]
    fn guard_maps_panic_to_action() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let interactive: Result<(), PanicAction> =
            guard(true, false, no_prev(), ok, || panic!("boom"));
        // Real Stop hook, first stop: fail CLOSED (block), NOT allow.
        let first_stop: Result<(), PanicAction> =
            guard(false, false, panicked_prev(), ok, || panic!("boom"));
        // Re-entry, but this gate's previous stop did not panic: a FIRST panic.
        let first_on_reentry: Result<(), PanicAction> =
            guard(false, true, no_prev(), ok, || panic!("boom"));
        // Re-entry, previous stop of this gate panicked: second consecutive.
        let second: Result<(), PanicAction> =
            guard(false, true, panicked_prev(), ok, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(interactive, Err(PanicAction::InteractiveError));
        assert_eq!(
            first_stop,
            Err(PanicAction::FailClosedBlock),
            "a crashed gate on the first stop must fail CLOSED (block), not silently allow"
        );
        assert_eq!(
            first_on_reentry,
            Err(PanicAction::FailClosedBlock),
            "stop_hook_active alone (another gate blocked) must not allow a first panic"
        );
        assert_eq!(
            second,
            Err(PanicAction::BoundedAllow),
            "only a second consecutive crash of the same gate allows the stop"
        );
    }

    #[test]
    fn decide_unrecordable_first_panic_on_reentry_allows_loudly() {
        // A first panic on a re-entry that cannot be recorded would block
        // forever (the next re-entry is equally blind), so it is a loud allow.
        assert!(matches!(
            decide(false, true, no_prev(), fail),
            PanicAction::UndeterminedAllow(_)
        ));
        // On the first stop of a chain the record failure does not matter:
        // it still blocks.
        assert_eq!(
            decide(false, false, no_prev(), fail),
            PanicAction::FailClosedBlock
        );
    }

    #[test]
    fn undetermined_allow_json_carries_a_system_message_and_no_decision() {
        let v: serde_json::Value =
            serde_json::from_str(&undetermined_allow_json("donegate", "no session")).unwrap();
        assert!(
            v.get("decision").is_none(),
            "must not carry a decision: {v}"
        );
        let m = v["systemMessage"].as_str().unwrap();
        assert!(m.contains("donegate") && m.contains("no session") && m.contains("NOT"));
    }

    #[test]
    fn fail_closed_block_json_is_a_block_decision() {
        // The panic fail-closed path must emit the gates' own block protocol:
        // `{"decision":"block","reason":...}`. A parse + field check pins that a
        // refactor can't silently turn it into an allow (no decision / approve).
        let v: serde_json::Value =
            serde_json::from_str(&fail_closed_block_json("donegate")).unwrap();
        assert_eq!(v["decision"], "block", "must block the stop, not allow it");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains("donegate"), "reason names the gate");
        assert!(
            reason.contains("fail-closed"),
            "reason states it is a fail-closed block"
        );
    }

    #[test]
    fn interactive_takes_precedence_over_stop_hook_active() {
        // `interactive` wins even if the (irrelevant, no-payload) re-entry flag
        // were set: a manual run has no live turn to block.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Result<(), PanicAction> = guard(true, true, panicked_prev(), ok, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(r, Err(PanicAction::InteractiveError));
    }

    #[test]
    fn append_jsonl_creates_dir_and_appends_lines() {
        let dir = std::env::temp_dir()
            .join(format!("hc-gate-log-{}", std::process::id()))
            .join("nested"); // parent does not exist yet
        let _ = std::fs::remove_dir_all(&dir);
        append_jsonl(&dir, &serde_json::json!({ "verdict": "pass" }));
        append_jsonl(&dir, &serde_json::json!({ "verdict": "fail" }));
        let body = std::fs::read_to_string(dir.join("log.jsonl")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "each call appends exactly one line");
        assert!(lines[0].contains("\"pass\"") && lines[1].contains("\"fail\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
