//! Stop-hook entry helpers shared by the gates: the never-break-a-turn panic
//! guard and the one-shot skip-marker consumer.

use std::path::Path;

/// What a panicking gate body resolves to, once the run mode is known. Pulled
/// out of [`run_guarded`] so the fail-closed policy is unit-testable without
/// actually exiting the process.
#[derive(Debug, PartialEq, Eq)]
enum PanicAction {
    /// Manual/interactive run: surface the crash on stderr and exit 1. No stop
    /// decision is emitted (there is no live turn to block).
    InteractiveError,
    /// Real Stop hook, first stop in the continuation chain: the gate crashed
    /// before it could decide, so it cannot certify the stop is safe. Emit a
    /// **block** decision (fail closed) and exit 0.
    FailClosedBlock,
    /// Real Stop hook, already a post-block re-entry (`stop_hook_active`): a
    /// second consecutive crash. Allow the stop (exit 0) to avoid trapping the
    /// session in an endless block loop — the first crash already surfaced a
    /// block. Bounded fail-open, and only after fail-closed fired once.
    BoundedAllow,
}

/// Run a Stop-hook body under the never-break-a-turn panic guard.
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
///   * hook mode, post-block re-entry (`stop_hook_active == true`) → allow the
///     stop (exit 0). A deterministically-panicking gate would otherwise block
///     forever; Claude Code sets `stop_hook_active` on the stop that follows a
///     block, so this bounds the fail-closed block to a single occurrence
///     (surface once, then let the session proceed). This mirrors how the Stop
///     nudges (ctxrot/budgetguard) bound themselves via `stop_hook_active`.
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
/// the `disabled` toggles and the `consume_skip(&root, ".<gate>-skip")` marker —
/// *before* any panic-prone verification (config is fail-soft; the checkers /
/// git / subprocess work is not). Otherwise a deterministically-crashing gate
/// would be unescapable: the operator's skip marker or `enabled = false` would be
/// dead code behind the crash. The `tests/gate_escape_ordering.rs` guard pins
/// this ordering across all four gates. (The `give-up`/`max_attempts` hatch may
/// stay after `evaluate`; the *crash* case is instead bounded by the
/// `stop_hook_active` `BoundedAllow` above.)
pub fn run_guarded<R, F: FnOnce() -> R>(
    name: &str,
    interactive: bool,
    stop_hook_active: bool,
    body: F,
) -> R {
    match guard(interactive, stop_hook_active, body) {
        Ok(value) => value,
        Err(action) => panic_exit(name, action),
    }
}

/// Testable core of [`run_guarded`]: run `body`, returning its value on success
/// or the [`PanicAction`] to take if it panicked. Does no IO and never exits, so
/// the fail-closed policy can be asserted directly.
fn guard<R, F: FnOnce() -> R>(
    interactive: bool,
    stop_hook_active: bool,
    body: F,
) -> Result<R, PanicAction> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(v) => Ok(v),
        Err(_) => Err(if interactive {
            PanicAction::InteractiveError
        } else if stop_hook_active {
            PanicAction::BoundedAllow
        } else {
            PanicAction::FailClosedBlock
        }),
    }
}

/// The fail-closed block decision emitted when a gate body panics on the first
/// stop: a serialized `{"decision":"block","reason":...}` line (the gates' own
/// block protocol). Pure — split out so its shape/reason is unit-testable
/// without exiting the process.
fn fail_closed_block_json(name: &str) -> String {
    let reason = format!(
        "{name}: internal error — the gate crashed before it could run, so this \
         stop is blocked as fail-closed (the check did not execute; its result \
         is unknown). Address the cause and continue; if {name} crashes again on \
         the next stop it is allowed through, so the session is never trapped."
    );
    serde_json::json!({ "decision": "block", "reason": reason }).to_string()
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
            // turn end unchecked. `stop_hook_active` bounds this to one block.
            println!("{}", fail_closed_block_json(name));
            std::process::exit(0);
        }
        PanicAction::BoundedAllow => {
            // Second consecutive crash on the post-block re-entry. The first
            // crash already surfaced a fail-closed block; blocking again would
            // trap the session, so allow the stop (bounded fail-open). Still
            // surface it on stderr for hook diagnostics.
            eprintln!(
                "{name}: internal error again on stop re-entry — allowing this \
                 stop to avoid trapping the session (a fail-closed block was \
                 already surfaced once)."
            );
            std::process::exit(0);
        }
    }
}

/// Read a marker's one-line reason: the trimmed contents, or
/// `"(no reason given)"` when it is empty but readable.
///
/// `None` means **判定不能** — the path exists but its contents could not be
/// read (it is a directory, a device, unreadable, …). That is deliberately NOT
/// mapped to a default reason: a token we cannot read is not a token we may act
/// on (CLAUDE.md 第3節). It used to become `"(no reason given)"`, i.e. an IO
/// failure was honoured as a valid escape — and since `remove_file` fails on
/// the same path, it was re-honoured on *every* later stop: a permanent bypass
/// created by accident.
fn skip_reason(p: &Path) -> Option<String> {
    let text = std::fs::read_to_string(p).ok()?;
    let trimmed = text.trim();
    Some(if trimmed.is_empty() {
        "(no reason given)".to_string()
    } else {
        trimmed.to_string()
    })
}

/// Where an already-honoured marker is parked until the stop it authorised
/// actually completes: `<marker>.honoured`, beside the marker itself.
fn honoured_name(marker: &str) -> String {
    format!("{marker}.honoured")
}

/// Consume a one-shot skip marker `<root>/<marker>` for the stop being
/// adjudicated right now: return its trimmed one-line reason (or
/// `"(no reason given)"` when the file is empty) while the escape is still
/// owed, and `None` once it has been spent — or when it cannot be honoured.
///
/// # The token is spent by a completed stop, not by this gate's own verdict
///
/// A Stop is adjudicated by four independent processes (donegate, reviewgate,
/// tdd, propguard), each holding only its own verdict. This function used to
/// `remove_file` the marker the instant *its* gate decided to allow. So when
/// donegate honoured `.donegate-skip` and allowed while reviewgate blocked the
/// same stop, **no stop happened** and the operator's one-shot escape had been
/// spent on nothing: they had to re-place the marker on every re-entry, for
/// every gate, until the four-way conjunction went green at once — the standing
/// pressure toward a *permanent* bypass that CLAUDE.md 第5節 forbids.
///
/// `stop_hook_active` is the seam that fixes it. Claude Code sets it on the
/// stop that follows a block ([`crate::hook::HookInput`]), so it distinguishes
/// "this stop is a re-entry after somebody blocked" from "this stop starts a
/// fresh chain". The lifecycle is therefore:
///
/// 1. **Marker present** — honour it and `rename` it to `<marker>.honoured`.
///    The escape is now *owed*: it has authorised a stop that has not completed.
/// 2. **Only `<marker>.honoured` present, `stop_hook_active == true`** — some
///    gate blocked the stop this token authorised, so the stop never happened.
///    Keep honouring it; the escape is still owed.
/// 3. **Only `<marker>.honoured` present, `stop_hook_active == false`** — the
///    chain ended, so a stop this token authorised did complete. Delete the
///    record and return `None`. This is what keeps the escape one-shot rather
///    than permanent (pinned by
///    `tests/gate_skip_token_survives_block.rs::token_is_consumed_exactly_once_on_the_ordinary_path`).
///
/// A `rename` (rather than a second bookkeeping file) is used so that a marker
/// the operator *re-places* is never mistaken for an outstanding honour: the
/// two states have different filenames and step 1 always wins.
///
/// # Failure modes, all resolved to the restrictive side
///
/// * Marker unreadable → `None` (not honoured); see [`skip_reason`].
/// * `rename` fails → the honour cannot be recorded, so it cannot be bounded.
///   Fall back to the old behaviour and `remove_file` the marker, honouring it
///   exactly once. If that fails too, the token can be neither bounded nor
///   cleared, so it is **not** honoured at all (`None`).
/// * `<marker>.honoured` unreadable on a re-entry → `None` (the gate checks).
///
/// # Residual, stated rather than hidden
///
/// `if !p.exists()` maps EACCES to "no marker" exactly as it does ENOENT, so an
/// unreadable-by-permission marker still reads as absent (restrictive here, but
/// it is a判定不能 rendered as a verdict). Pinning it needs an injectable
/// metadata probe — filed as backlog `dc85e1c8`, not fixed here.
pub fn consume_skip(root: &Path, marker: &str, stop_hook_active: bool) -> Option<String> {
    let p = root.join(marker);
    let honoured = root.join(honoured_name(marker));

    // (1) A token nobody has honoured yet — or one the operator has just
    // re-placed, which supersedes any stale record.
    if p.exists() {
        let reason = skip_reason(&p)?;
        if std::fs::rename(&p, &honoured).is_err() && std::fs::remove_file(&p).is_err() {
            // Neither bounded nor cleared: honouring it now would create a
            // standing bypass, so do not honour it.
            return None;
        }
        return Some(reason);
    }

    if !honoured.exists() {
        return None;
    }

    // (2) The stop this token authorised was blocked by some other gate, so it
    // never happened: the escape is still owed.
    if stop_hook_active {
        return skip_reason(&honoured);
    }

    // (3) A stop this token authorised completed. Spend it.
    let _ = std::fs::remove_file(&honoured);
    None
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
        crate::append::append_line(&path, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_passes_value_through() {
        // A clean body's value is returned regardless of mode / re-entry flag.
        assert_eq!(guard(true, false, || 7), Ok(7));
        assert_eq!(guard(false, false, || 7), Ok(7));
        assert_eq!(guard(false, true, || 7), Ok(7));
    }

    #[test]
    fn guard_maps_panic_to_action() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let interactive: Result<(), PanicAction> = guard(true, false, || panic!("boom"));
        // Real Stop hook, first stop: fail CLOSED (block), NOT allow.
        let first_stop: Result<(), PanicAction> = guard(false, false, || panic!("boom"));
        // Real Stop hook, post-block re-entry: bounded allow so it can't trap.
        let re_entry: Result<(), PanicAction> = guard(false, true, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(interactive, Err(PanicAction::InteractiveError));
        assert_eq!(
            first_stop,
            Err(PanicAction::FailClosedBlock),
            "a crashed gate on the first stop must fail CLOSED (block), not silently allow"
        );
        assert_eq!(
            re_entry,
            Err(PanicAction::BoundedAllow),
            "a second crash on re-entry allows the stop so the session is not trapped"
        );
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
        let r: Result<(), PanicAction> = guard(true, true, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(r, Err(PanicAction::InteractiveError));
    }

    fn skip_root(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hc-gate-skip-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skip_marker_absent_is_none() {
        let root = skip_root("absent");
        assert!(consume_skip(&root, ".x-skip", false).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_marker_with_reason_is_consumed_once() {
        let root = skip_root("reason");
        std::fs::write(root.join(".x-skip"), "  because\n").unwrap();
        assert_eq!(
            consume_skip(&root, ".x-skip", false).as_deref(),
            Some("because")
        );
        // consumed: a second call sees nothing.
        assert!(consume_skip(&root, ".x-skip", false).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_marker_empty_gives_default_reason() {
        let root = skip_root("empty");
        std::fs::write(root.join(".x-skip"), "   \n").unwrap();
        assert_eq!(
            consume_skip(&root, ".x-skip", false).as_deref(),
            Some("(no reason given)")
        );
        let _ = std::fs::remove_dir_all(&root);
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
