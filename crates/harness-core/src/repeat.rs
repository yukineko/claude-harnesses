//! The session-scoped repeat ledger: **a gate does not get to override the
//! same user instruction twice.**
//!
//! # The ruling this implements (operator, 2026-09-18)
//!
//! Quoted verbatim, because the rest of this module is a consequence of it and
//! a later reader must be able to check the consequence against the words:
//!
//! > 「ユーザの指示を2度やぶるgateはいらない。2度目はaskせよ」
//! > 「他のgateでも同じルールを適用して」
//! > 「askのだせないgateもゴミ」
//!
//! A gate blocking once is the gate doing its job: it surfaces something the
//! user had not considered. A gate blocking the *same* thing a second time,
//! after the user has seen the reason and asked again anyway, is no longer
//! informing a decision — the user has made the decision, and the gate is
//! substituting its own. That second block carries no information the first did
//! not already carry.
//!
//! So the second occurrence of an identical finding resolves to whichever
//! "hand it back to the human" answer the gate's protocol can express:
//!
//! | gate protocol | first occurrence | second occurrence |
//! |---|---|---|
//! | PreToolUse (`permissionDecision`) — blastguard, ctxrot, parallelguard | `deny` | **`ask`** |
//! | Stop (`{"decision":"block"}`) — donegate, reviewgate, propguard, tdd, budgetguard, autoflow, … | `block` | **allow** |
//!
//! The split is not a preference. `ask` does not exist in the Stop hook
//! protocol, so for a Stop gate the only answer that is not "override the user
//! again" is to let the stop through. The operator was asked precisely this and
//! ruled 「全 gate で 2度目は無条件に通す」 — no interactive-only carve-out, and
//! no exemption for headless or agent-driven runs.
//!
//! # What this gives up — stated plainly, because it is real
//!
//! This is a **fail-open on the second occurrence, by explicit ruling.** It is
//! the one mechanism in this repo that deliberately resolves a *known*
//! violation to the permissive side, so it must not be read as precedent:
//!
//! * A Stop gate that correctly blocks — donegate finding a real clippy
//!   failure, tdd finding a missing F→P oracle — **allows the stop the second
//!   time**, in headless runs included. The finding is still true; it is simply
//!   no longer enforced against a user who has now seen it twice.
//! * For blastguard, the second `rm -rf` of the same target is an `ask` rather
//!   than a `deny`. A human still has to answer it, but a human who answers
//!   "yes" is no longer overruled.
//! * This does **not** apply to a finding the user never saw. The ledger keys
//!   on an actually-emitted block, so the first block always stands at full
//!   strength.
//!
//! Every waiver is recorded (see [`waivers`]) so "the gate stopped firing" is
//! never silent — CLAUDE.md §4 forbids making an error invisible, and it
//! forbids it here too even though the waiver itself was ordered.
//!
//! # Where the fail-CLOSED rule still holds (CLAUDE.md §3)
//!
//! "Let the second one through" requires *knowing* it is the second one.
//! [`observe`] returns [`Determination::Undetermined`] when it cannot tell —
//! no session id, an unwritable state dir, an IO error — and an `Undetermined`
//! caller MUST keep its original strict verdict. Reading "I could not check the
//! ledger" as "this must be the second time" would collapse every gate to
//! permanently open on a single unwritable directory, which is the opposite of
//! what was asked for: the ruling is about the user's *second* instruction, not
//! about a broken ledger.
//!
//! # Identity and lifetime
//!
//! Operator ruling, same session: 「同一セッション内のみ」. Two occurrences are
//! "the same" when the gate name and the finding fingerprint match **within one
//! `CLAUDE_CODE_SESSION_ID`**. A new session starts every finding back at
//! first-occurrence strength, so a waiver granted last week cannot silence
//! today's block.

use crate::hash::fnv1a32_hex;
use crate::verdict::Determination;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Whether a gate has already emitted this exact finding in this session.
///
/// Deliberately NOT `bool` and deliberately NOT `Default`. A bool would give
/// callers a `false` to fall back on, and `false` here means "first offence,
/// block at full strength" — which is the *safe* direction, but it would let an
/// IO failure masquerade as an observation. The three-valued
/// `Determination<Offense>` keeps "I could not check" expressible, per
/// CLAUDE.md §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offense {
    /// Never seen in this session. The gate's verdict stands unchanged.
    First,
    /// Seen before in this session. The gate must hand the decision back to the
    /// human: `ask` if its protocol has one, otherwise allow.
    Repeat,
}

impl Offense {
    /// `true` when the caller must downgrade its blocking verdict.
    ///
    /// Provided so call sites read as intent rather than as a match on an enum
    /// they might get backwards.
    #[must_use]
    pub fn must_yield(self) -> bool {
        matches!(self, Offense::Repeat)
    }
}

/// The directory holding one session's seen-findings, created on demand.
fn session_dir(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir.join("repeat").join(session_id)
}

/// The marker file for one (gate, fingerprint) pair.
///
/// The fingerprint is hashed rather than used directly: a finding fingerprint
/// is arbitrary text (a command line, a file path, a checker's message) and
/// would otherwise have to be escaped into a filename. The hash is only an
/// identity key — a collision costs one over-eager waiver, never a missed
/// block, because a collision can only make an unrelated finding look like a
/// repeat of one the user already saw.
fn marker(state_dir: &Path, session_id: &str, gate: &str, fingerprint: &str) -> PathBuf {
    session_dir(state_dir, session_id).join(format!(
        "{}-{}.seen",
        fnv1a32_hex(gate),
        fnv1a32_hex(fingerprint)
    ))
}

/// Record that `gate` is emitting `fingerprint`, and report whether it already
/// did so in this session.
///
/// Check and record are a single atomic step — `create_new` either creates the
/// marker (this is the first occurrence) or fails with `AlreadyExists` (it is
/// not). Doing it as a separate `exists()` + `create()` would let two
/// concurrent hooks both read "absent" and both report `First`, which would
/// make a gate that fires from parallel sessions un-waivable.
///
/// Returns [`Determination::Undetermined`] — never `First`, never `Repeat` —
/// when the answer could not be established. Callers keep their strict verdict
/// on `Undetermined`; see the module docs.
pub fn observe(
    state_dir: &Path,
    session_id: &str,
    gate: &str,
    fingerprint: &str,
) -> Determination<Offense> {
    if session_id.trim().is_empty() {
        return Determination::undetermined(
            "no session id (CLAUDE_CODE_SESSION_ID is unset) — a repeat is scoped to one session, \
             so without one this cannot be shown to be a second occurrence",
        );
    }
    if fingerprint.trim().is_empty() {
        return Determination::undetermined(
            "empty finding fingerprint — an empty key would make every finding of this gate look \
             like the same one",
        );
    }
    let dir = session_dir(state_dir, session_id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Determination::undetermined(format!(
            "could not create the repeat ledger at {}: {e}",
            dir.display()
        ));
    }
    let path = marker(state_dir, session_id, gate, fingerprint);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            // Body is diagnostic only; a write failure does not change the
            // verdict, because the marker's EXISTENCE is the record.
            let _ = writeln!(f, "{gate}\t{fingerprint}");
            Determination::known(Offense::First)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Determination::known(Offense::Repeat)
        }
        Err(e) => Determination::undetermined(format!(
            "could not write the repeat ledger entry at {}: {e}",
            path.display()
        )),
    }
}

/// Append a waiver to the session's audit trail.
///
/// Called by a gate that just downgraded a blocking verdict, so that "this gate
/// went quiet" is a recorded event rather than an absence. Best-effort by
/// design: the waiver has already been granted by the time this runs, and
/// failing to log it must not turn into a second block (that would re-create
/// exactly the override this module removes). The tradeoff is stated here
/// rather than hidden — an unwritable ledger loses the audit line, not the
/// ruling.
pub fn record_waiver(state_dir: &Path, session_id: &str, gate: &str, detail: &str) {
    let dir = session_dir(state_dir, session_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("waivers.log"))
    {
        let _ = writeln!(f, "{gate}\t{detail}");
    }
}

/// Every waiver recorded in this session, oldest first.
///
/// Returns [`Determination::Undetermined`] when the log exists but could not be
/// read: an empty `Vec` would be indistinguishable from "no gate was waived",
/// and reporting "nothing was waived" when the answer is unknown is the empty-set
/// fail-open CLAUDE.md §3 names explicitly. A genuinely absent log IS a known
/// empty list — nothing has been waived yet.
pub fn waivers(state_dir: &Path, session_id: &str) -> Determination<Vec<String>> {
    let path = session_dir(state_dir, session_id).join("waivers.log");
    match std::fs::read_to_string(&path) {
        Ok(s) => Determination::known(s.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(Vec::new()),
        Err(e) => Determination::undetermined(format!(
            "could not read the waiver log at {}: {e}",
            path.display()
        )),
    }
}

/// The fleet-wide ledger root.
///
/// One location for every gate rather than each gate's own `state_dir`: the
/// ledger is already keyed by gate name, and a per-gate root would exclude the
/// gates that have no state dir of their own (`autoflow`, `ctxrot`) from a rule
/// the operator applied to all of them.
#[must_use]
pub fn default_state_dir() -> PathBuf {
    crate::config::home().join(".claude").join("harness-state")
}

/// The session a hook is running under, or `""` when the environment did not
/// say — which [`observe`] resolves to `Undetermined`, i.e. keeps the block.
#[must_use]
pub fn session_id() -> String {
    std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default()
}

/// Whether a gate must yield its blocking verdict for this finding.
///
/// The one-call form for gates that already have their own emit path (a
/// PreToolUse `deny` → `ask` downgrade, say). `false` on `Undetermined`, so a
/// ledger that could not answer leaves the gate exactly as strict as it was.
#[must_use]
pub fn must_yield(gate: &str, fingerprint: &str) -> bool {
    let session = session_id();
    match observe(&default_state_dir(), &session, gate, fingerprint) {
        Determination::Known(o) if o.must_yield() => {
            record_waiver(
                &default_state_dir(),
                &session,
                gate,
                &format!("2nd occurrence, yielded: {fingerprint}"),
            );
            true
        }
        // `Known(First)` keeps the block because the user has not seen it yet;
        // `Undetermined` keeps it because we could not show they had.
        _ => false,
    }
}

/// The Stop-hook block line to print, or `None` when this gate already blocked
/// this exact finding in this session.
///
/// `None` means "print nothing", which for a Stop hook IS allowing the stop —
/// the second-occurrence waiver the operator ordered for gates whose protocol
/// has no `ask`. The waiver is logged before this returns, so the gate going
/// quiet is recoverable from [`waivers`] rather than invisible.
#[must_use]
pub fn stop_block_or_waive(gate: &str, reason: &str) -> Option<serde_json::Value> {
    if must_yield(gate, reason) {
        return None;
    }
    Some(serde_json::json!({ "decision": "block", "reason": reason }))
}

/// Apply the second-occurrence waiver to a Stop decision a gate has already
/// built — typically [`crate::verdict::Verdict::stop_decision`].
///
/// Wraps the gate's existing emit path rather than replacing it, so a call site
/// becomes `repeat::waive_stop_decision("donegate", v.stop_decision())` and the
/// gate keeps ownership of *what* it blocks on. `None` in stays `None` (a clean
/// verdict was never blocking); `Some(block)` becomes `None` only on a proven
/// second occurrence.
///
/// A decision whose `reason` is missing or empty is returned **unchanged**: no
/// fingerprint can be formed from it, so it cannot be shown to be a repeat, and
/// CLAUDE.md §3 sends that to the strict side.
#[must_use]
pub fn waive_stop_decision(
    gate: &str,
    decision: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let value = decision?;
    let reason = value.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    if reason.trim().is_empty() {
        return Some(value);
    }
    if must_yield(gate, reason) {
        return None;
    }
    Some(value)
}

/// Print a Stop-hook decision line, unless this gate already blocked this exact
/// finding in this session.
///
/// The one-line form every Stop gate calls in place of its own
/// `println!("{}", json!({"decision":"block", ...}))`.
///
/// # A waiver is announced, never silent
///
/// Printing nothing IS allowing the stop, so a waived block would otherwise be
/// indistinguishable from a gate that found nothing — the exact "cannot tell
/// 'checked' from 'could not check'" confusion CLAUDE.md §3 forbids, and the
/// invisibility §4 forbids. So the waiver path still prints a line: a
/// `systemMessage` (user-visible, and NOT a `decision`, so the stop proceeds)
/// saying which finding was let through and that it is still unresolved. The
/// gate goes quiet about *blocking*, never about *having found something*.
pub fn emit_stop_block(gate: &str, reason: &str) {
    match stop_block_or_waive(gate, reason) {
        Some(decision) => println!("{decision}"),
        None => println!(
            "{}",
            serde_json::json!({
                "systemMessage": format!(
                    "{gate}: 同じ指摘で2度目のブロックになるため、この stop は通しました\
                     （2026-09-18 の運用判断「ユーザの指示を2度やぶる gate はいらない」）。\
                     指摘そのものは解消していません:\n{reason}"
                )
            })
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hc-repeat-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn known(d: Determination<Offense>) -> Offense {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(r) => panic!("expected Known, got Undetermined({r})"),
        }
    }

    /// THE RULE. First occurrence stands, second yields.
    #[test]
    fn second_occurrence_of_the_same_finding_yields() {
        let d = tmp("second");
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::First,
            "the first block must stand at full strength"
        );
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::Repeat,
            "the user has now seen this reason and asked again — the gate must yield"
        );
    }

    /// CONTROL — must hold before and after. A *different* finding from the
    /// same gate is not a repeat; without this the first waiver would silence
    /// the whole gate.
    #[test]
    fn a_different_finding_from_the_same_gate_is_still_first() {
        let d = tmp("diff-finding");
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::First
        );
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "fmt failed")),
            Offense::First,
            "a finding the user has NOT seen must block at full strength"
        );
    }

    /// CONTROL. The same finding from a different gate is that gate's first.
    #[test]
    fn the_same_finding_from_a_different_gate_is_still_first() {
        let d = tmp("diff-gate");
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::First
        );
        assert_eq!(
            known(observe(&d, "sess1", "reviewgate", "clippy failed")),
            Offense::First
        );
    }

    /// CONTROL for the operator's 「同一セッション内のみ」 ruling: a waiver
    /// earned in one session must not silence another.
    #[test]
    fn a_new_session_starts_back_at_first_occurrence() {
        let d = tmp("new-session");
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::First
        );
        assert_eq!(
            known(observe(&d, "sess1", "donegate", "clippy failed")),
            Offense::Repeat
        );
        assert_eq!(
            known(observe(&d, "sess2", "donegate", "clippy failed")),
            Offense::First,
            "a new session must not inherit the previous session's waiver"
        );
    }

    /// CLAUDE.md §3: no session id is "cannot determine", NOT "second time".
    #[test]
    fn missing_session_id_is_undetermined_not_repeat() {
        let d = tmp("no-session");
        for id in ["", "   "] {
            match observe(&d, id, "donegate", "clippy failed") {
                Determination::Undetermined(_) => {}
                Determination::Known(v) => {
                    panic!("no session id resolved to Known({v:?}); it must be Undetermined")
                }
            }
        }
    }

    /// CLAUDE.md §3. An empty fingerprint would collapse every finding of a
    /// gate onto one key, so the second *unrelated* block would be waived.
    #[test]
    fn empty_fingerprint_is_undetermined_not_repeat() {
        let d = tmp("empty-fp");
        match observe(&d, "sess1", "donegate", "") {
            Determination::Undetermined(_) => {}
            Determination::Known(v) => {
                panic!("empty fingerprint resolved to Known({v:?}); it must be Undetermined")
            }
        }
    }

    /// CLAUDE.md §3, the load-bearing one: an unusable ledger must never read
    /// as "this is the second time". If it did, one unwritable directory would
    /// open every gate in the fleet.
    #[test]
    fn an_unusable_state_dir_is_undetermined_not_repeat() {
        let d = tmp("unusable");
        // A regular FILE where the ledger directory must go: create_dir_all
        // cannot succeed under it.
        let blocked = d.join("blocker");
        std::fs::write(&blocked, b"x").unwrap();
        match observe(&blocked, "sess1", "donegate", "clippy failed") {
            Determination::Undetermined(_) => {}
            Determination::Known(v) => panic!(
                "an unusable state dir resolved to Known({v:?}); a broken ledger must not waive \
                 anything"
            ),
        }
    }

    /// `must_yield` is the predicate call sites use; pin it to the enum so a
    /// later inversion breaks here rather than silently in every gate.
    #[test]
    fn must_yield_is_true_only_for_repeat() {
        assert!(!Offense::First.must_yield());
        assert!(Offense::Repeat.must_yield());
    }

    /// A waiver must leave a trace: CLAUDE.md §4 forbids a gate going quiet
    /// invisibly, even when the quiet was ordered.
    #[test]
    fn waivers_are_recorded_and_readable() {
        let d = tmp("waivers");
        match waivers(&d, "sess1") {
            Determination::Known(v) => assert!(v.is_empty(), "a fresh session has no waivers"),
            Determination::Undetermined(r) => panic!("absent log should be a known empty: {r}"),
        }
        record_waiver(
            &d,
            "sess1",
            "donegate",
            "clippy failed — 2nd occurrence, allowed",
        );
        record_waiver(
            &d,
            "sess1",
            "blastguard",
            "rm -rf x — 2nd occurrence, downgraded to ask",
        );
        match waivers(&d, "sess1") {
            Determination::Known(v) => {
                assert_eq!(v.len(), 2, "both waivers must be readable, got {v:?}");
                assert!(v[0].starts_with("donegate\t"), "oldest first: {v:?}");
                assert!(v[1].starts_with("blastguard\t"), "newest last: {v:?}");
            }
            Determination::Undetermined(r) => panic!("expected Known, got Undetermined({r})"),
        }
    }
}
