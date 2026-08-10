// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verdict monotonicity for overwatch's Continuous-Audit convergence report
//! (backlog a7d41587).
//!
//! The property, from `harness_core::degrade`:
//!
//! ```text
//!     permissiveness(f(d(x))) <= permissiveness(f(x))
//! ```
//!
//! Here `f` is "read the audit-round ledger and report whether the audit is
//! converging", and `d` degrades the ledger BYTES — truncated, corrupted,
//! emptied. The audit history those bytes record is unchanged in every case;
//! only overwatch's ability to read it drops. So the report may go undetermined,
//! and it may stay `false`, but it must never become `true`.
//!
//! This is not hypothetical. Against the shipped 0.2.15 binary all three
//! degradations flipped `converging` from `false` to `true` at exit 0:
//!
//! ```text
//!   corrupt     before="converging": false   after="converging": true   exit=0
//!   truncate    before="converging": false   after="converging": true   exit=0
//!   unreadable  before="converging": false   after="converging": true   exit=0
//! ```
//!
//! Three separate mechanisms conspired: `store::read_audit_rounds` mapped a read
//! error to `Ok(Vec::new())`, its line loop dropped unparseable records
//! silently, and `is_converging` returns `true` for anything under two rounds.
//! Each is individually defensible-sounding; composed, damaging the evidence
//! improved the verdict.
//!
//! # Why the parse layer and not the CLI
//!
//! `store::read_audit_rounds` resolves its path through `HOME`, which is
//! process-global — a proptest that swapped it would race its own cases. So the
//! property drives the pure `parse_rounds` + `compute_metrics` pair, and the IO
//! arm (unreadable file) is pinned by a separate serial test in
//! `audit_round_cli`. Between them every arm of the tri-state is covered.
//!
//! # What this property does NOT cover
//!
//! Blinding, not substitution. If a degradation happens to leave a VALID ledger
//! (a truncation landing on a record boundary), the gate is not blind — it is
//! reading well-formed evidence that is no longer the whole evidence. No
//! content-level check can tell that from a genuinely shorter history; it needs
//! integrity state the format does not carry. That residual is pinned by
//! `known_gap_a_boundary_truncation_still_reads_as_a_shorter_history` below and
//! filed, not swept up.

use harness_core::degrade::{explain_break, Degradation};
use harness_core::verdict::{Determination, Required};
use overwatch::audit_round::{self, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
use proptest::prelude::*;

/// The gate under test: ledger bytes in, convergence verdict out.
///
/// `Some(converging)` when the ledger was read to a conclusion, `None` when it
/// could not be. `Option<bool>` is exactly the ordering
/// `harness_core::degrade` needs: `None` and `Some(false)` are both restrictive,
/// `Some(true)` is the permissive one.
fn converging_verdict(ledger: &str) -> Option<bool> {
    // `require()`'s `Required` has no `unwrap_err`/`.ok()`, so both arms are
    // matched explicitly here. `Blocked` is NOT a "should never happen"
    // corner this test would rather crash on: it is the exact case this
    // file's property drives on purpose — a non-boundary truncate/corrupt
    // degradation yields bytes `parse_rounds` cannot read (see
    // `an_unparseable_record_is_undetermined_not_a_shorter_history` below),
    // and the module docs above name this outcome explicitly ("the report
    // may go undetermined ... but it must never become true"). Panicking on
    // `Blocked` would abort the monotonicity property on that documented,
    // expected path instead of exercising it, and — per this file's own
    // header comment — resolving "could not read" to anything other than the
    // restrictive `None` is exactly the fail-open this test exists to catch.
    // So `Blocked` maps to `None`, the same restrictive answer the old
    // `.require().ok()?` produced; nothing about the oracle's meaning changes.
    let rounds = match audit_round::parse_rounds(ledger).require() {
        Required::Determined(rounds) => rounds,
        Required::Blocked(_verdict) => return None,
    };
    // `converging` is itself `Option<bool>` — `None` when there are too few
    // rounds to read a trend — so the two ways of failing to reach a verdict
    // (unreadable ledger, unanswerable question) flatten to the same
    // restrictive `None`.
    audit_round::compute_metrics(&rounds, DEFAULT_CONVERGENCE_WINDOW).converging
}

fn ledger_text(rounds: &[AuditRound]) -> String {
    rounds
        .iter()
        .map(|r| serde_json::to_string(r).expect("AuditRound serializes"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Are `degraded` bytes a prefix of `intact` that ends on a record boundary?
///
/// Purely lexical: a prefix whose next byte in the original is a newline (or
/// which consumed the whole file) leaves only whole records behind. This is the
/// integrity carve-out described in the module docs, and keeping it structural
/// is what stops it from becoming a self-issued pardon — see the comment at the
/// call site.
fn is_clean_prefix(intact: &str, degraded: &str) -> bool {
    let (i, d) = (intact.as_bytes(), degraded.as_bytes());
    if !i.starts_with(d) {
        return false;
    }
    // Both sides of the newline are boundaries: the cut may land just BEFORE it
    // (the next byte in the original is `\n`) or just AFTER it (the prefix ends
    // with `\n`). Checking only the first was itself a bug — the property found
    // it, via a ledger whose last record had a two-digit count and so shifted
    // the midpoint by one byte.
    d.is_empty() || d.len() == i.len() || i.get(d.len()) == Some(&b'\n') || d.last() == Some(&b'\n')
}

/// A ledger of `n` rounds whose new-findings counts are arbitrary — so the
/// generator produces both converging and non-converging histories rather than
/// only the shape that happens to expose the bug.
fn arb_ledger() -> impl Strategy<Value = Vec<AuditRound>> {
    prop::collection::vec(0u64..40, 2..8).prop_map(|counts| {
        counts
            .into_iter()
            .enumerate()
            .map(|(i, new_findings)| {
                AuditRound::new(
                    format!("r{i}"),
                    &["overwatch".to_string()],
                    new_findings,
                    0,
                    0,
                    1_700_000_000 + i as i64,
                )
            })
            .collect()
    })
}

proptest! {
    /// The property itself.
    #[test]
    fn degrading_the_ledger_never_improves_the_convergence_verdict(
        rounds in arb_ledger(),
        which in prop::sample::select(Degradation::ALL),
    ) {
        let intact = ledger_text(&rounds);
        let intact_verdict = converging_verdict(&intact);

        // Unreadable has no in-memory form; apply_bytes reports the skip rather
        // than silently returning the input unchanged (which would pass
        // vacuously). The IO arm is covered by the serial test in
        // audit_round_cli instead.
        let Some(degraded_bytes) = which.apply_bytes(intact.as_bytes()) else {
            return Ok(());
        };
        let degraded = String::from_utf8_lossy(&degraded_bytes).into_owned();

        // BLINDING vs SUBSTITUTION. A cut that lands mid-record garbles it and
        // the gate goes blind — that is what this property governs. But a cut
        // that lands exactly on a record boundary yields a VALID, self-
        // consistent ledger of fewer rounds, and nothing in the bytes
        // distinguishes it from a genuinely shorter history. That is evidence
        // substitution, and catching it needs integrity (a count sidecar, a
        // chain), not parsing. See the known-gap test below.
        //
        // The test is STRUCTURAL, on the bytes — deliberately not "did it still
        // parse". Asking the parser would let a parser that silently drops
        // garbled records excuse itself: it would report a short clean history,
        // the carve-out would call that a prefix, and the very bug this file
        // exists to catch would pass. Only a cut landing on a real newline
        // boundary is excused.
        if is_clean_prefix(&intact, &degraded) {
            return Ok(());
        }

        let degraded_verdict = converging_verdict(&degraded);
        if let Some(msg) = explain_break(which, &intact_verdict, &degraded_verdict) {
            prop_assert!(false, "{msg}\n  ledger was {} rounds", rounds.len());
        }
    }
}

/// The residual, pinned as an OPEN GAP rather than left silent (CLAUDE.md §5).
///
/// Truncating `audit_rounds.jsonl` at a record boundary produces a ledger that
/// parses cleanly and reports a better verdict than the intact one. This is not
/// a parsing bug and the tri-state above does not fix it: a tail-truncated
/// append-only file is indistinguishable from a shorter one using the file's
/// contents alone. Detecting it requires out-of-band state — a recorded round
/// count, or a chain — which changes the on-disk format and would make every
/// pre-existing ledger unverifiable until its next append. That trade is a
/// design decision, so it is filed rather than decided here.
///
/// This test asserts the GAP, deliberately. If someone closes it, this test
/// fails and points them here — which is the intent. It must NOT be read as a
/// specification that truncation is acceptable.
#[test]
fn known_gap_a_boundary_truncation_still_reads_as_a_shorter_history() {
    let rounds: Vec<AuditRound> = [0u64, 0, 0, 1]
        .iter()
        .enumerate()
        .map(|(i, n)| {
            AuditRound::new(
                format!("r{i}"),
                &["overwatch".to_string()],
                *n,
                0,
                0,
                1_700_000_000 + i as i64,
            )
        })
        .collect();
    let intact = ledger_text(&rounds);
    assert_eq!(
        converging_verdict(&intact),
        Some(false),
        "0,0,0,1 ends on an increase"
    );

    // Equal-width records mean the halfway byte IS a record boundary.
    let cut = &intact[..intact.len() / 2];
    let parsed = audit_round::parse_rounds(cut)
        .require()
        .expect("a boundary cut leaves whole records, so it parses");
    assert_eq!(parsed.len(), 2, "the cut kept exactly the first two rounds");
    assert_eq!(
        converging_verdict(cut),
        Some(true),
        "OPEN GAP: a truncated ledger reports a better verdict than the intact \
         one. Content-level parsing cannot see this; closing it needs ledger \
         integrity. If this assertion starts failing because integrity landed, \
         delete this test and drop the prefix carve-out in the property above."
    );
}

/// Anti-vacuity control. If the generator only ever produced already-converging
/// ledgers (`Some(true)` intact), the property above could not fail no matter
/// what the degradation did — every verdict is <= permissive-true. This pins
/// that the search space actually contains the case that can break.
#[test]
fn the_property_has_a_case_that_could_break_it() {
    let rounds: Vec<AuditRound> = [1u64, 9]
        .iter()
        .enumerate()
        .map(|(i, n)| {
            AuditRound::new(
                format!("r{i}"),
                &["overwatch".to_string()],
                *n,
                0,
                0,
                1_700_000_000 + i as i64,
            )
        })
        .collect();
    // 1 -> 9 is INCREASING, so the intact verdict is the restrictive one.
    assert_eq!(
        converging_verdict(&ledger_text(&rounds)),
        Some(false),
        "the fixture must start non-converging or the property is vacuous"
    );
    // And the permissive value must be REACHABLE, or the property could not
    // fail no matter what a degradation did: every verdict is <= Some(true).
    let decreasing: Vec<AuditRound> = [9u64, 1]
        .iter()
        .enumerate()
        .map(|(i, n)| {
            AuditRound::new(
                format!("r{i}"),
                &["overwatch".to_string()],
                *n,
                0,
                0,
                1_700_000_000 + i as i64,
            )
        })
        .collect();
    assert_eq!(
        converging_verdict(&ledger_text(&decreasing)),
        Some(true),
        "Some(true) must be reachable or the property is vacuous"
    );

    // The specific shrink proptest found: dropping the last record leaves one
    // round, which used to read as vacuously converging. That is the permissive
    // value a degraded read must never reach — now it is undetermined.
    assert_eq!(
        converging_verdict(&ledger_text(&rounds[..1])),
        None,
        "a one-round ledger has no trend; it must be undetermined, not converging"
    );
}

/// The specific mechanism, stated as a unit test so a regression names itself
/// instead of surfacing as an opaque proptest shrink.
#[test]
fn an_unparseable_record_is_undetermined_not_a_shorter_history() {
    let good = r#"{"round":"r0","targets":["overwatch"],"new_findings":1,"confirmed":0,"unverified":0,"regression_tests_added":0,"ts":1700000000}"#;
    let ledger = format!("{good}\n{{\"round\":\"r1\",\"new_fin");

    match audit_round::parse_rounds(&ledger) {
        Determination::Undetermined(why) => {
            assert!(
                why.as_str().contains("line 2"),
                "the reason should name the offending line: {}",
                why.as_str()
            );
        }
        Determination::Known(rounds) => panic!(
            "a truncated record was silently dropped, yielding a {}-round \
             history that reads as converging",
            rounds.len()
        ),
    }
}

/// The other side of the same contract: an empty or absent ledger is a real,
/// determined answer ("no rounds yet"), not an undetermined one. Without this,
/// the fix would fail closed on every fresh checkout and the audit loop would
/// be unusable — which is how a correct fail-closed change gets reverted.
#[test]
fn an_empty_ledger_is_known_empty_not_undetermined() {
    match audit_round::parse_rounds("") {
        Determination::Known(rounds) => assert!(rounds.is_empty()),
        Determination::Undetermined(why) => {
            panic!("empty ledger must be Known(vec![]), got: {}", why.as_str())
        }
    }
    // Trailing newline from the append path must not read as a bad record.
    match audit_round::parse_rounds("\n") {
        Determination::Known(rounds) => assert!(rounds.is_empty()),
        Determination::Undetermined(why) => {
            panic!("blank line must be skipped, got: {}", why.as_str())
        }
    }
}
