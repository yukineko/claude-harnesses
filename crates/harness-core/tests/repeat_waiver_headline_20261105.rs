#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-05: the second-occurrence waiver notice of
//! `repeat::emit_stop_block_keyed` stays ONE line, but carries a short headline
//! of the finding so it is identifiable at a glance (not a log):
//!
//!   `<gate>: 同じ指摘の2度目のため通しました（未解消）— <headline>`
//!
//! `<headline>` = first non-empty line of the DISPLAYED `reason` (not the
//! fingerprint), trimmed; a leading status emoji ("🔴 ") and a "<gate>: " prefix
//! may be stripped; bounded to 80 chars (chars, not bytes) with "…" when
//! truncated; never contains a newline. No headline formable (empty /
//! whitespace reason) => exactly today's message, never a dangling "— ".
//!
//! `emit_stop_block_keyed` prints, so it is observed out of process via a
//! re-exec'd child test fn with isolated HOME and a controlled session id
//! (same style as `repeat_quiet_waiver.rs`).

use std::path::PathBuf;
use std::process::{Command, Output};

const CHILD_ENV: &str = "REPEAT_HEADLINE_CHILD";
const REASON_ENV: &str = "REPEAT_HEADLINE_REASON";
const FP_ENV: &str = "REPEAT_HEADLINE_FP";
const GATE: &str = "tdd";
const BASE: &str = "tdd: 同じ指摘の2度目のため通しました（未解消）";
const BOUND: usize = 80;

const TDD_FIRST_LINE: &str =
    "🔴 tdd: write a test first — 85 new implementation line(s) added, 0 test line(s)";

fn tdd_reason() -> String {
    format!(
        "{TDD_FIRST_LINE}\n  implementation changed: crates/foo/src/lib.rs (+85)\n  \
         tests changed: (none)\n  run `tdd red` after writing a failing test"
    )
}

#[test]
fn child_emit_keyed() {
    let Ok(n) = std::env::var(CHILD_ENV) else {
        return;
    };
    let reason = std::env::var(REASON_ENV).unwrap();
    let fp = std::env::var(FP_ENV).unwrap();
    for _ in 0..n.parse::<usize>().unwrap() {
        println!();
        harness_core::repeat::emit_stop_block_keyed(GATE, &reason, &fp);
    }
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("hc-rwh-{tag}-{}-{nanos}", std::process::id()));
    for sub in ["home", "state", "data", "config", "cache", "cwd"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

/// Run the child `times` times in one fresh session; returns stdout JSON lines
/// and raw stdout.
fn run(tag: &str, reason: &str, fp: &str, times: usize) -> (Vec<serde_json::Value>, Output) {
    let root = scratch(tag);
    let o = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_emit_keyed",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, times.to_string())
        .env(REASON_ENV, reason)
        .env(FP_ENV, fp)
        .env("CLAUDE_CODE_SESSION_ID", format!("s-rwh-{tag}"))
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root.join("cwd"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let lines = String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let s = l.find('{')?;
            serde_json::from_str(l[s..].trim()).ok()
        })
        .collect();
    (lines, o)
}

/// Second-occurrence waiver message (asserts the block-then-waiver shape).
fn waiver_msg(tag: &str, reason: &str, fp: &str) -> String {
    let (lines, o) = run(tag, reason, fp, 2);
    assert_eq!(lines.len(), 2, "block then waiver line expected: {o:?}");
    assert_eq!(lines[0]["decision"], "block", "1st must block: {o:?}");
    let w = &lines[1];
    assert!(
        w.get("decision").is_none(),
        "waiver must not be a decision: {w}"
    );
    w["systemMessage"]
        .as_str()
        .expect("waiver systemMessage string")
        .to_string()
}

/// Split the waiver message into its headline; fails if the "— " suffix is absent.
fn headline_of(msg: &str) -> String {
    let prefix = format!("{BASE}— ");
    let Some(h) = msg.strip_prefix(&prefix) else {
        panic!("waiver must be `{prefix}<headline>`, got: {msg:?}");
    };
    h.to_string()
}

/// (a) Realistic tdd reason: waiver carries "— " + the first line's headline,
/// and nothing from the second line.
#[test]
fn a_second_occurrence_carries_headline_of_first_line() {
    let reason = tdd_reason();
    let msg = waiver_msg("a", &reason, &reason);
    let h = headline_of(&msg);
    assert!(!h.trim().is_empty(), "headline must not be empty: {msg:?}");
    assert_eq!(h, h.trim(), "headline must be trimmed: {h:?}");
    assert!(
        TDD_FIRST_LINE.ends_with(&h),
        "headline must be the first line (optionally stripped of `🔴 ` / `tdd: `), \
         got {h:?}"
    );
    assert!(
        h.contains("write a test first — 85 new implementation line(s) added, 0 test line(s)"),
        "first line (79 chars, under the bound) must not be truncated: {h:?}"
    );
    assert!(
        !h.ends_with('…'),
        "under-bound headline must not get `…`: {h:?}"
    );
    assert!(
        !msg.contains("implementation changed"),
        "only the headline, never later lines of the reason: {msg:?}"
    );
}

/// (a') The headline comes from the displayed reason, not the fingerprint.
#[test]
fn a2_headline_comes_from_reason_not_fingerprint() {
    let reason = "tdd: DISPLAYED-HEADLINE-MARKER\nmore detail";
    let msg = waiver_msg("a2", reason, "FINGERPRINT-ONLY-MARKER");
    let h = headline_of(&msg);
    assert!(h.contains("DISPLAYED-HEADLINE-MARKER"), "{msg:?}");
    assert!(!msg.contains("FINGERPRINT-ONLY-MARKER"), "{msg:?}");
}

/// (b) Long first line is truncated to <= 80 chars (counted in chars, using a
/// multi-byte line so a byte bound would cut far shorter) and ends with "…".
#[test]
fn b_long_first_line_is_truncated_in_chars_with_ellipsis() {
    let long: String = "長い指摘".repeat(50); // 200 chars, 600 bytes
    let reason = format!("{long}\nsecond line");
    let msg = waiver_msg("b", &reason, &reason);
    let h = headline_of(&msg);
    assert!(
        h.ends_with('…'),
        "truncated headline must end with `…`: {h:?}"
    );
    let n = h.chars().count();
    assert!(
        n <= BOUND + 1,
        "headline must be bounded to {BOUND} chars (+`…`), got {n}: {h:?}"
    );
    assert!(
        n >= BOUND / 2,
        "bound must be counted in chars, not bytes (80 bytes = ~26 CJK chars); got {n}: {h:?}"
    );
    let body = h.trim_end_matches('…');
    assert!(
        long.starts_with(body),
        "truncated headline must be a prefix of the first line: {h:?}"
    );
}

/// (c) Leading blank lines are skipped, the headline is trimmed, and the whole
/// notice is exactly one line with no newline / CR.
#[test]
fn c_headline_is_first_nonempty_line_and_single_line() {
    let reason = "\n   \n\t  first real line here  \r\nsecond line\nthird";
    let msg = waiver_msg("c", reason, reason);
    let h = headline_of(&msg);
    assert_eq!(h, "first real line here", "{msg:?}");
    assert!(!msg.contains('\n') && !msg.contains('\r'), "{msg:?}");
}

/// (d) No headline formable => exactly today's message; never a dangling "— ".
/// (A separate non-empty fingerprint is used so the waiver is reachable.)
#[test]
fn d_empty_or_whitespace_reason_keeps_exact_old_message() {
    for (tag, reason) in [("d-empty", ""), ("d-ws", "  \n\t \n   ")] {
        let msg = waiver_msg(tag, reason, "fp-for-empty-reason");
        assert_eq!(
            msg, BASE,
            "reason {reason:?} must give today's exact message"
        );
        assert!(!msg.contains('—'), "no dangling `— `: {msg:?}");
    }
}

/// (e) The first occurrence still prints the full block decision unchanged.
#[test]
fn e_first_occurrence_prints_full_block_unchanged() {
    let reason = tdd_reason();
    let (lines, o) = run("e", &reason, &reason, 1);
    assert_eq!(lines.len(), 1, "one JSON line expected: {o:?}");
    assert_eq!(
        lines[0],
        serde_json::json!({ "decision": "block", "reason": reason }),
        "first block must carry the full reason, unchanged"
    );
}
