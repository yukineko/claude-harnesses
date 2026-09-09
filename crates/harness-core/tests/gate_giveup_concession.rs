//! A gate's earned give-up must not be spent on a stop that never happened.
//!
//! # What this pins
//!
//! Every gate carries a `max_attempts` budget: after N consecutive blocks it
//! concedes and allows the stop, so an agent that genuinely cannot satisfy the
//! check is never trapped. That concession is EARNED — the operator paid N
//! blocked stops for it.
//!
//! But a Stop is adjudicated by **four independent processes** — donegate,
//! reviewgate, propguard and tdd. Each one knows only its own verdict. When one
//! gate concedes and allows while another blocks the same stop, **the stop did
//! not happen**: Claude Code re-invokes the whole set with
//! `stop_hook_active == true`. If the conceding gate treats its give-up as
//! consumed at that moment, it re-enforces from attempt 1 on the re-entry — so
//! while any other gate stays red, the concession can never actually be
//! collected. The agent pays for the hatch over and over and never gets through
//! it.
//!
//! `stop_hook_active` is the only signal available to distinguish "a re-entry
//! after some gate blocked" from "a fresh stop chain", so the contract below is
//! expressed entirely in terms of it.
//!
//! # Why the anti-vacuity tests matter as much as the contract ones
//!
//! The naive repair — "once conceded, always owed" — satisfies the survival
//! contract while turning a bounded, earned hatch into a **permanent bypass**:
//! the gate would be dead for the rest of the session, silently, with nothing in
//! the transcript saying so. That is strictly worse than the defect being fixed
//! (CLAUDE.md 第5節: skip 機構は理由を書いて一度だけ; 恒常的な迂回に使わない).
//! So every survival assertion below is paired with a spend assertion, and the
//! boundedness test ([`a_concession_is_spent_by_a_stop_that_completed`]) checks
//! BOTH `stop_hook_active` values after the spend — because a bypass that only
//! reappears on re-entries is still a bypass.

use std::path::{Path, PathBuf};

use harness_core::gate::run::{concede, concession_owed};

/// A fresh, isolated gate state dir. Tagged per test and stamped with the pid
/// and a nanosecond clock so parallel test threads (and parallel runs of the
/// suite) cannot collide on it.
fn state_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "hc-giveup-concession-{}-{tag}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Best-effort cleanup. Kept out of the assertions so that a failing test leaves
/// its temp dir behind for inspection.
fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// BASELINE / ANTI-VACUITY FLOOR.
///
/// A gate that never conceded is owed nothing, on either kind of stop. Without
/// this, an implementation that answered `true` unconditionally would pass every
/// survival test below while disabling the gate outright from the first stop of
/// the session — the failure mode this whole contract exists to bound.
#[test]
fn nothing_conceded_is_never_owed() {
    let dir = state_dir("baseline");
    let sess = "sess-baseline";

    assert!(
        !concession_owed(&dir, sess, false),
        "a gate that never gave up must not report a concession owed on a fresh \
         stop: doing so hands out the max_attempts hatch for free, so the gate \
         never enforces anything at all"
    );
    assert!(
        !concession_owed(&dir, sess, true),
        "nor on a re-entry: `stop_hook_active` means some gate blocked, which is \
         the moment enforcement matters most — treating it as a concession turns \
         every blocked stop into a free pass"
    );

    cleanup(&dir);
}

/// THE CONTRACT.
///
/// The gate exhausted its budget and conceded, allowing the stop. Some OTHER
/// gate — a different process, invisible to this one — blocked that same stop,
/// so the turn did not end and Claude Code re-entered with
/// `stop_hook_active == true`. The concession must still be owed: the stop it
/// authorised has not happened, so nothing has been collected yet.
///
/// If it is not owed here, the gate re-enforces from attempt 1 and the agent
/// must pay the full `max_attempts` price again — for as long as any other gate
/// stays red, which is unbounded.
#[test]
fn a_concession_survives_a_stop_that_another_gate_blocked() {
    let dir = state_dir("survives-block");
    let sess = "sess-survives-block";

    concede(&dir, sess);

    assert!(
        concession_owed(&dir, sess, true),
        "a stop that some OTHER gate blocked must not consume this gate's earned \
         give-up: no stop happened, so the concession is still owed. Consuming it \
         here makes the agent re-earn max_attempts on every re-entry, so while any \
         other gate stays red the hatch can never be collected at all"
    );

    cleanup(&dir);
}

/// The other gate can stay red for several rounds.
///
/// Nothing bounds how long a peer gate keeps blocking, and each of those rounds
/// is a re-entry. If the concession survived only the first one, the fix would
/// merely move the defect one attempt later instead of removing it.
#[test]
fn a_concession_survives_several_consecutive_reentries() {
    let dir = state_dir("multi-reentry");
    let sess = "sess-multi-reentry";

    concede(&dir, sess);

    for round in 1..=5 {
        assert!(
            concession_owed(&dir, sess, true),
            "the concession must survive re-entry #{round}: a peer gate may stay \
             red for many rounds, and an escape that expires after the first one \
             is still an escape the agent can never actually collect"
        );
    }

    cleanup(&dir);
}

/// ANTI-VACUITY / BOUNDEDNESS — the most important test here.
///
/// Once a stop the concession authorised actually completes, the concession is
/// SPENT. A completed stop is observable exactly as the next stop that is not a
/// re-entry (`stop_hook_active == false`): the chain ended and a new one began.
///
/// Both follow-up calls are checked, with `stop_hook_active` false AND true,
/// because "always return true" would satisfy the two survival tests above while
/// converting a bounded, earned hatch into a permanent bypass — a gate that is
/// silently dead for the rest of the session. A bypass that only reappears on
/// re-entries is no better: re-entries are precisely when a peer gate is
/// blocking, i.e. when enforcement still has work to do.
#[test]
fn a_concession_is_spent_by_a_stop_that_completed() {
    let dir = state_dir("bounded");
    let sess = "sess-bounded";

    concede(&dir, sess);

    assert!(
        !concession_owed(&dir, sess, false),
        "a stop that is not a re-entry means the previous stop completed, so the \
         concession has been collected and is spent: reporting it owed here keeps \
         the gate disabled for every later stop in the session"
    );
    assert!(
        !concession_owed(&dir, sess, false),
        "and it stays spent on the next fresh stop: an earned give-up buys ONE \
         stop, not a standing exemption (CLAUDE.md 第5節)"
    );
    assert!(
        !concession_owed(&dir, sess, true),
        "and it stays spent on a later re-entry too: a hatch that reopens whenever \
         another gate blocks is a permanent bypass wearing a bound, and it opens \
         exactly when enforcement is still needed"
    );

    cleanup(&dir);
}

/// The full lifecycle in one place, in the order the four-way race produces it.
///
/// concede → a peer gate blocks, so the concession is still owed on the re-entry
/// → every gate then allows, the stop completes and the concession is collected
/// → a later re-entry (a new chain, a new block) is NOT owed and the gate
/// enforces again from attempt 1.
///
/// Stated as one sequence because the defect and its naive repair each break a
/// different edge of it; a test per edge can pass while the walk through them
/// does not.
#[test]
fn concession_lifecycle_survives_a_block_then_is_spent_by_a_completed_stop() {
    let dir = state_dir("lifecycle");
    let sess = "sess-lifecycle";

    concede(&dir, sess);

    assert!(
        concession_owed(&dir, sess, true),
        "step 1: a peer gate blocked the conceded stop, so the give-up is still \
         owed — otherwise the agent re-earns max_attempts on every re-entry"
    );
    assert!(
        !concession_owed(&dir, sess, false),
        "step 2: the stop completed (a fresh chain begins), so the give-up has been \
         collected and is spent — a give-up that outlives the stop it bought is a \
         standing exemption"
    );
    assert!(
        !concession_owed(&dir, sess, true),
        "step 3: a later block starts a new enforcement round from attempt 1 — the \
         collected give-up must not be handed out a second time, or the gate never \
         enforces again for the rest of the session"
    );

    cleanup(&dir);
}

/// Concessions are per session.
///
/// The state dir is shared by every session on the machine. A give-up earned by
/// session A says nothing about session B, which has paid nothing: leaking it
/// across sessions hands the hatch to an agent that never earned it, and one
/// stuck session would disable the gate for every concurrent one.
#[test]
fn a_concession_does_not_leak_to_another_session() {
    let dir = state_dir("session-scope");
    let a = "sess-alpha";
    let b = "sess-beta";

    concede(&dir, a);

    assert!(
        !concession_owed(&dir, b, true),
        "session B never exhausted its budget, so it is owed nothing: leaking A's \
         earned give-up lets B skip the gate for free, and one stuck session would \
         disable the gate for every concurrent session sharing the state dir"
    );
    assert!(
        !concession_owed(&dir, b, false),
        "and the same on a fresh stop for B — the leak must not exist for either \
         value of `stop_hook_active`"
    );
    assert!(
        concession_owed(&dir, a, true),
        "while A's own concession is untouched: reading B's state must not consume \
         or clear the give-up that A actually paid for"
    );

    cleanup(&dir);
}

/// A gate may legitimately give up again later in the same session.
///
/// After the first concession is collected, work continues and the gate can
/// exhaust `max_attempts` a second time. That second give-up is a new, separately
/// earned one: it must survive a blocked stop like the first, and be bounded like
/// the first. An implementation that let the spent state stick would trap the
/// agent from then on; one that let the second concession be permanent would
/// re-open the bypass.
#[test]
fn a_second_concession_is_freshly_earned_and_again_bounded() {
    let dir = state_dir("re-concede");
    let sess = "sess-re-concede";

    // First give-up: earned, survives a block, collected by a completed stop.
    concede(&dir, sess);
    assert!(
        concession_owed(&dir, sess, true),
        "the first give-up must survive the peer gate's block"
    );
    assert!(
        !concession_owed(&dir, sess, false),
        "the first give-up is spent once the stop it authorised completes"
    );

    // The gate enforced again, and exhausted its budget again.
    concede(&dir, sess);
    assert!(
        concession_owed(&dir, sess, true),
        "the second give-up was earned exactly like the first and must survive a \
         blocked stop too: a spent first concession must not permanently disable \
         the hatch, which would trap an agent that is genuinely stuck"
    );
    assert!(
        !concession_owed(&dir, sess, false),
        "and it is bounded exactly like the first — re-conceding must not bank an \
         unbounded exemption"
    );
    assert!(
        !concession_owed(&dir, sess, true),
        "including on a later re-entry: the second hatch must close as firmly as \
         the first, or repeated give-ups accumulate into a permanent bypass"
    );

    cleanup(&dir);
}

/// An unusable session id resolves to the restrictive side.
///
/// An empty id is 判定不能 about *whose* concession this is (CLAUDE.md 第3節):
/// it cannot identify a session, so it cannot witness that any session paid the
/// `max_attempts` price. Honouring it would make the hatch collectable by every
/// caller whose session id failed to arrive — a bypass keyed on an IO/plumbing
/// failure rather than on anything earned. `concede` with it must also not
/// panic: a gate that crashes here is 判定不能 in the loudest possible way.
#[test]
fn an_empty_session_id_is_never_owed_a_concession() {
    let dir = state_dir("empty-session");
    let empty = "";

    assert!(
        !concession_owed(&dir, empty, false),
        "an id that identifies no session cannot witness an earned give-up, so the \
         restrictive answer is 'not owed': anything else hands the hatch to every \
         caller whose session id went missing"
    );
    assert!(
        !concession_owed(&dir, empty, true),
        "and the same on a re-entry, where that free pass would be handed out at \
         precisely the moment a peer gate is blocking"
    );

    // Must not panic — a panicking gate is the loudest form of 判定不能.
    concede(&dir, empty);

    assert!(
        !concession_owed(&dir, empty, true),
        "and conceding under an unusable id must not manufacture a collectable \
         concession out of it"
    );
    assert!(
        !concession_owed(&dir, empty, false),
        "on either kind of stop: an unidentifiable session must never be able to \
         collect a give-up it cannot have earned"
    );

    cleanup(&dir);
}
