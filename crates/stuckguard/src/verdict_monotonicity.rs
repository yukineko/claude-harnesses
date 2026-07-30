//! Verdict monotonicity for stuckguard's stuck-loop verdict (backlog a7d41587).
//!
//! The property, from `harness_core::degrade`:
//!
//! ```text
//!     permissiveness(f(d(x))) <= permissiveness(f(x))
//! ```
//!
//! Here `f` is "load this session's history, add the call that just happened,
//! and decide whether the agent is stuck", and `d` degrades the persisted
//! session-state file — truncated, corrupted, emptied, unreadable. What the
//! agent actually DID is unchanged in every case; only stuckguard's record of it
//! degrades. So the gate may go undetermined and it may keep nudging, but it
//! must never fall silent.
//!
//! # Why stuckguard, when it is already correct here
//!
//! Unlike the overwatch instance, this is not a fix — `main::watch` already
//! resolves an `Undetermined` load to a nudge, and `tests/faultinject_state.rs`
//! records the incident that got it there. The property is a RATCHET: it states
//! the invariant once so that a future edit which quietly reintroduces
//! `SessionState::default()` on the error path is caught by a test that was
//! never written about that edit.
//!
//! That it passes on first run is therefore expected and is NOT evidence that it
//! works. What is evidence: reverting the fail-closed arm to
//! `Determination::Known(st) | Undetermined(_) => SessionState::default()` makes
//! it fail — see `the_property_dies_when_the_failclosed_arm_is_reverted`, which
//! performs exactly that substitution inline rather than asking the reader to
//! take it on trust.
//!
//! # Polarity
//!
//! `detect` returns `Option<Trip>` where `None` means "nothing wrong" — the
//! PERMISSIVE answer. That is the inverse of `harness_core::degrade`'s
//! `Option<T>` ordering, where `None` is restrictive. The verdict is therefore
//! adapted at the call site to `Option<bool>` meaning "allowed to carry on",
//! exactly as `Permissiveness for bool` documents. Getting this backwards would
//! make the property assert the reverse of what is meant, so
//! `the_verdict_adapter_has_the_polarity_it_claims` pins it.

use std::path::Path;

use harness_core::degrade::{explain_break, Degradation};
use harness_core::verdict::Determination;
use proptest::prelude::*;

use crate::config::Config;
use crate::detect;
use crate::sig::Event;
use crate::state::{self, SessionState};

/// A deterministic detector config: three identical calls in the window trip a
/// repeat. `state_dir` is per-case, so nothing here touches process-global
/// state and the cases may run in parallel.
fn cfg_for(state_dir: &Path) -> Config {
    Config {
        window: 20,
        repeat_threshold: 3,
        cooldown_events: 0,
        escalate_after: 2,
        state_dir: state_dir.to_path_buf(),
        ..Config::default()
    }
}

fn event(tool: &str, sig: &str) -> Event {
    Event {
        seq: 0,
        tool: tool.to_string(),
        sig: sig.to_string(),
        tokens: Default::default(),
        file: None,
        old_h: None,
        new_h: None,
        error: false,
        failed_test_digest: None,
    }
}

/// The gate under test, adapted to `harness_core::degrade`'s ordering.
///
/// `Some(true)` = allowed to carry on (permissive), `Some(false)` = nudged,
/// `None` = the gate could not reach a verdict. Note the inversion of `detect`'s
/// own `Option<Trip>`: a `Trip` is the RESTRICTIVE outcome.
fn allowed_to_carry_on(state_dir: &Path, session: &str, next: Event, cfg: &Config) -> Option<bool> {
    match state::load(state_dir, session) {
        // Mirrors `main::watch`: an unreadable history nudges and stops. The
        // window is never rebuilt from a default.
        Determination::Undetermined(_) => None,
        Determination::Known(mut st) => {
            st.push(next, cfg.window);
            Some(detect::detect(&st.events, cfg).is_none())
        }
    }
}

/// The same gate with the fail-closed arm REMOVED, for the mutation test below.
/// Kept beside the real one so the two cannot drift apart unnoticed.
fn allowed_to_carry_on_failing_open(
    state_dir: &Path,
    session: &str,
    next: Event,
    cfg: &Config,
) -> Option<bool> {
    let mut st = match state::load(state_dir, session) {
        Determination::Known(st) => st,
        // THE DEFECT: "I could not read the history" becomes "there is no
        // history", which hands the detector a one-event window.
        Determination::Undetermined(_) => SessionState::default(),
    };
    st.push(next, cfg.window);
    Some(detect::detect(&st.events, cfg).is_none())
}

/// Seed a session already deep in a repeat loop, and return its state path.
fn seed_stuck_session(state_dir: &Path, session: &str, repeats: usize) -> std::path::PathBuf {
    let cfg = cfg_for(state_dir);
    let mut st = SessionState::default();
    for _ in 0..repeats {
        st.push(event("Bash", "same-command"), cfg.window);
    }
    state::save(state_dir, session, &st);
    state::path(state_dir, session)
}

proptest! {
    /// The property itself.
    #[test]
    fn degrading_the_session_history_never_silences_the_nudge(
        which in prop::sample::select(Degradation::ALL),
        repeats in 3usize..12,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_for(dir.path());
        let session = "s";
        let path = seed_stuck_session(dir.path(), session, repeats);

        let next = event("Bash", "same-command");
        let intact = allowed_to_carry_on(dir.path(), session, next.clone(), &cfg);
        // Control: the seeded session really is stuck, or the case proves
        // nothing. A gate that was already nudging cannot become MORE
        // restrictive, so this is what makes the property able to fail.
        prop_assert_eq!(
            intact,
            Some(false),
            "seeded session must trip the detector before we degrade anything"
        );

        // `apply_to_file` reports a no-op rather than silently returning the
        // input unchanged, so a degradation that cannot bite is skipped, not
        // counted as a pass.
        let bit = which.apply_to_file(&path).expect("degrade the state file");
        if !bit {
            return Ok(());
        }

        let degraded = allowed_to_carry_on(dir.path(), session, next, &cfg);
        if let Some(msg) = explain_break(which, &intact, &degraded) {
            prop_assert!(false, "{msg}\n  session had {} repeats", repeats);
        }
    }
}

/// Mutation test, inline. Proves the property above can actually fail — that it
/// is a ratchet and not decoration.
///
/// If this ever passes, the property has stopped testing anything.
#[test]
fn the_property_dies_when_the_failclosed_arm_is_reverted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = cfg_for(dir.path());
    let session = "s";
    let path = seed_stuck_session(dir.path(), session, 5);
    let next = event("Bash", "same-command");

    let intact = allowed_to_carry_on(dir.path(), session, next.clone(), &cfg);
    assert_eq!(intact, Some(false), "control: the session is stuck");

    // Corrupt the history so the load goes undetermined.
    assert!(Degradation::Corrupt
        .apply_to_file(&path)
        .expect("corrupt the state file"));

    // The shipped gate: undetermined, i.e. nudge and stop. Monotone.
    let real = allowed_to_carry_on(dir.path(), session, next.clone(), &cfg);
    assert_eq!(real, None, "the shipped gate must not reach a verdict here");
    assert!(explain_break(Degradation::Corrupt, &intact, &real).is_none());

    // The reverted gate: an empty window, a one-event history, "all clear".
    let mutated = allowed_to_carry_on_failing_open(dir.path(), session, next, &cfg);
    assert_eq!(
        mutated,
        Some(true),
        "the reverted arm should report the stuck loop as fine"
    );
    assert!(
        explain_break(Degradation::Corrupt, &intact, &mutated).is_some(),
        "the property must FLAG the reverted arm; if it does not, it is not \
         testing the thing it claims to test"
    );
}

/// Coverage guard: every degradation must actually BITE on a session-state
/// file, and every one must drive the verdict to the restrictive side.
///
/// Without this, a degradation that quietly became a no-op (`apply_to_file`
/// returning `Ok(false)`) would make the property SKIP its cases forever while
/// still reporting `ok`. The property cannot notice its own missing coverage —
/// silent truncation of a test matrix reads exactly like a passing one.
#[test]
fn every_degradation_bites_and_drives_the_verdict_restrictive() {
    for which in Degradation::ALL {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_for(dir.path());
        let session = "s";
        let path = seed_stuck_session(dir.path(), session, 5);
        let next = event("Bash", "same-command");

        assert_eq!(
            allowed_to_carry_on(dir.path(), session, next.clone(), &cfg),
            Some(false),
            "{which:?}: control — the seeded session must be stuck"
        );

        // `assert!(matches!(..))` rather than `panic!` in an `unwrap_or_else`:
        // `clippy::panic` is deny for this target, and widening the crate-root
        // allow to satisfy one test would loosen the lint for every test in the
        // binary.
        let bit = which.apply_to_file(&path);
        assert!(
            matches!(bit, Ok(true)),
            "{which:?} did not change the state file, so the property has been \
             silently skipping this degradation"
        );

        let after = allowed_to_carry_on(dir.path(), session, next, &cfg);
        assert_ne!(
            after,
            Some(true),
            "{which:?}: degrading the history reported the stuck session as fine"
        );
    }
}

/// Polarity guard for the adapter. `detect` returning `None` (no trip) is the
/// PERMISSIVE outcome, which is the inverse of `Option`'s own ordering in
/// `harness_core::degrade`. If the adapter is ever flipped, every case above
/// silently asserts the opposite of what it reads as.
#[test]
fn the_verdict_adapter_has_the_polarity_it_claims() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = cfg_for(dir.path());

    // A quiet session: one call, nothing repeated -> allowed.
    state::save(dir.path(), "quiet", &SessionState::default());
    assert_eq!(
        allowed_to_carry_on(dir.path(), "quiet", event("Bash", "one-off"), &cfg),
        Some(true),
        "a non-stuck session must be the PERMISSIVE value"
    );

    // A stuck session -> not allowed.
    seed_stuck_session(dir.path(), "stuck", 5);
    assert_eq!(
        allowed_to_carry_on(dir.path(), "stuck", event("Bash", "same-command"), &cfg),
        Some(false),
        "a stuck session must be the RESTRICTIVE value"
    );

    // And the ordering the property relies on.
    use harness_core::degrade::Permissiveness;
    assert!(Some(true).permissiveness() > Some(false).permissiveness());
    assert_eq!(None::<bool>.permissiveness(), Some(false).permissiveness());
}
