// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backslash-over-escaped nested `sh -c` fail-OPEN (backlog 99b506b7).
//!
//! blastguard is a PreToolUse gate: a destructive shell command must be BLOCKED
//! (Deny, or an Ask that hardens to Deny when no human is present) — never a
//! silent Allow. This suite pins a fail-OPEN regression where it did the opposite.
//!
//! ## The leak
//!
//! When a human nests `sh -c "..."` by hand, the naive escaping is to escape ONLY
//! the inner double-quote (`"` -> `\"`) and leave any pre-existing backslashes
//! untouched. Applied repeatedly this produces backslash-over-escaped layers.
//! blastguard's payload extractor does a FAITHFUL POSIX unquote, which — on these
//! over-escaped strings — truncated the payload *before* the destructive command
//! word, dropped it, and returned `Allow`. From nesting depth 4 downward the whole
//! line was ALLOWED. A hand-nested `rm -rf /` therefore sailed straight through.
//!
//! ## What is pinned
//!
//! 1. The malicious payload (`rm -rf /`), wrapped by the exact human rule
//!    (`wrap_quote_only`), must BLOCK at every depth 1..=8 — the core regression,
//!    since depths 4..=8 were Allow before the fix.
//! 2. The SAME nesting shape around a benign payload (`echo hi`) must stay Allow.
//!    This proves the fix discriminates by finding the destructive command word,
//!    not by blanket-blocking the over-escaped shape (which would be a useless,
//!    noisy gate).
//! 3. A hard-coded literal depth-4 leaking string is asserted blocking, so the
//!    regression stays pinned even if the helper below is ever changed.
//!
//! ## Predicate choice
//!
//! `detect()` is pure and reads no environment, so the honest, env-independent
//! assertion is `is_blocking()` (Deny OR Ask): an `Ask` is never weaker than the
//! `Allow` it replaces because it `hardened()`s to `Deny` when no human can
//! answer. Where the current code additionally returns a full `Deny`, a separate
//! test records that stronger fact — without weakening the core invariant.

use blastguard::detect::detect;
use blastguard::model::Decision;
use serde_json::json;

/// The public entry point, exactly as the PreToolUse hook calls it.
fn analyse(cmd: &str) -> Decision {
    detect("Bash", Some(&json!({ "command": cmd })))
}

/// The exact human-nesting rule from the vulnerability report (backlog
/// 99b506b7): wrap `p` in one `sh -c "..."` layer, escaping ONLY the
/// double-quote and leaving any existing backslashes as-is. This is the naive,
/// faithful way a person nests shells by hand, and it is what over-escapes.
fn wrap_quote_only(p: &str) -> String {
    format!("sh -c \"{}\"", p.replace('"', "\\\""))
}

/// Apply [`wrap_quote_only`] `depth` times to `payload`.
fn nest_quote_only(depth: usize, payload: &str) -> String {
    let mut cur = payload.to_string();
    for _ in 0..depth {
        cur = wrap_quote_only(&cur);
    }
    cur
}

const MALICIOUS: &str = "rm -rf /";
const BENIGN: &str = "echo hi";

/// Depths the regression spans. Before the fix, the malicious payload was
/// ALLOWED from depth 4 downward, so the range must cover both the still-blocked
/// shallow depths (1..=3) and the previously-leaking deep ones (4..=8).
const DEPTHS: std::ops::RangeInclusive<usize> = 1..=8;

// ---------------------------------------------------------------------------
// 1. Core regression — the malicious payload must BLOCK at every depth 1..=8.
//    Before the fix, depths 4..=8 were a silent ALLOW.
// ---------------------------------------------------------------------------

#[test]
fn quote_only_nested_rm_rf_blocks_at_every_depth() {
    for depth in DEPTHS {
        let cmd = nest_quote_only(depth, MALICIOUS);
        let d = analyse(&cmd);
        assert!(
            d.is_blocking(),
            "FAIL-OPEN (backlog 99b506b7): `{MALICIOUS}` wrapped in {depth} \
             quote-only `sh -c` layers must BLOCK (Deny or Ask), but got {d:?}. \
             Before the fix, depth >= 4 here was a silent ALLOW — a hand-nested \
             destructive command sailing through the gate.\n  command: {cmd}"
        );
    }
}

/// Stronger fact recorded separately so the core invariant above stays honest
/// even if a future change downgrades a layer to `Ask`: with the current code
/// every depth resolves to a full `Deny` (the destructive command word is
/// recovered from under the over-escaping). A downgrade to `Ask` would trip this
/// test — a signal to investigate — without pretending the payload became safe.
#[test]
fn quote_only_nested_rm_rf_is_a_full_deny_at_every_depth() {
    for depth in DEPTHS {
        let cmd = nest_quote_only(depth, MALICIOUS);
        let d = analyse(&cmd);
        assert!(
            d.is_deny(),
            "at depth {depth} the over-escaped `{MALICIOUS}` currently resolves \
             to a full Deny; got {d:?}. If this is now an Ask the core invariant \
             still holds (Ask blocks), but the extractor stopped recovering the \
             destructive word verbatim — investigate before accepting.\n  command: {cmd}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Discrimination — the SAME over-escaped shape around a benign payload must
//    stay ALLOW. Proves the fix keys on the destructive command word, not on
//    blanket-blocking the escape shape (which would make the gate useless).
// ---------------------------------------------------------------------------

#[test]
fn quote_only_nested_benign_payload_stays_allow_at_every_depth() {
    for depth in DEPTHS {
        let cmd = nest_quote_only(depth, BENIGN);
        let d = analyse(&cmd);
        assert_eq!(
            d,
            Decision::Allow,
            "over-block: `{BENIGN}` wrapped in {depth} quote-only `sh -c` layers \
             is harmless and must stay ALLOW. A block here would mean the fix is \
             blanket-blocking the over-escaped SHAPE instead of discriminating on \
             the destructive command word — a false-positive gate.\n  command: {cmd}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Literal leaking string — hard-coded so the regression stays pinned even if
//    the helper above is ever changed. This is the depth-4 example from backlog
//    99b506b7 verbatim, and it was ALLOWED before the fix.
// ---------------------------------------------------------------------------

#[test]
fn literal_depth4_leaking_string_blocks() {
    // The exact depth-4 example string from the backlog. Verified equal to
    // `nest_quote_only(4, "rm -rf /")` (asserted below), and asserted blocking
    // independently of the helper.
    let literal = r#"sh -c "sh -c \"sh -c \\"sh -c \\\"rm -rf /\\\"\\"\"""#;
    assert_eq!(
        literal,
        nest_quote_only(4, MALICIOUS),
        "the hard-coded literal must match the generator at depth 4; if this \
         fails, the pinned string drifted from the documented vulnerability"
    );
    let d = analyse(literal);
    assert!(
        d.is_blocking(),
        "FAIL-OPEN (backlog 99b506b7): the literal depth-4 leaking string must \
         BLOCK, but got {d:?}. This exact line was ALLOWED before the fix.\n  \
         command: {literal}"
    );
}
