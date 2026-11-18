// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Mutation-survivor tests for `classify::mask_file_path_tokens` (private),
//! observed through the public `riskgrade::classify::classify` whose deploy
//! tier reads the masked text. Written 2026-11-17 by an independent test
//! author after `scripts/mutation-gate.sh` (PILOT=riskgrade, classify.rs)
//! reported for `mask_file_path_tokens`:
//!   MISSED  classify.rs:255:14  `%` -> `/`, `%` -> `+`  (backtick parity)
//!   TIMEOUT classify.rs:262:66  delete `!`
//!   TIMEOUT classify.rs:263:32  `+` -> `*`
//!   TIMEOUT classify.rs:280:28  `+` -> `*`
//!
//! Every case runs in a CHILD PROCESS (this test binary re-executed with an
//! env var selecting `bounded_child_entry`) under a wall-clock deadline, and
//! the child is killed on timeout. A thread + `recv_timeout` is not enough:
//! the `280:28` mutant loops while appending to the output `String`, so a
//! leaked thread would keep allocating until the test process exits.
//!
//! Contract source: the doc comment of `mask_file_path_tokens` — inside a
//! backtick span and after an unbalanced backtick everything stays visible;
//! outside, a bare file-path token that is not in command position (first
//! token of the text / of the text after a span, after a runner word, ...)
//! is blanked. `rollout` is a tier-1 deploy needle, so `scripts/rollout.py`
//! yields `High`/irreversible when kept visible and `Low`/reversible when
//! blanked (no other needle or history signal occurs in these inputs).
//!
//! Repro: cargo test -p riskgrade --test mask_file_path_tokens_mutation_20261117

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use riskgrade::classify::{classify, Risk, RiskAssessment};

const CHILD_ENV: &str = "RISKGRADE_MASK_MUT_CHILD_INPUT";
const RESULT_TAG: &str = "RGMASK_RESULT";
const DEADLINE: Duration = Duration::from_secs(5);

/// Child side: a no-op unless the parent set `CHILD_ENV`.
#[test]
fn bounded_child_entry() {
    let Ok(input) = std::env::var(CHILD_ENV) else {
        return;
    };
    let a = classify(&input);
    println!("{RESULT_TAG} {:?} {}", a.risk, a.reversible);
}

/// Run `classify(input)` in a child process; panic if it does not finish
/// within `DEADLINE`, exits non-zero, or prints no result line.
fn classify_bounded(input: &str) -> RiskAssessment {
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(exe)
        .args([
            "bounded_child_entry",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, input)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child test process");
    let started = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        if started.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("classify({input:?}) did not terminate within {DEADLINE:?} (child killed)");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read child stdout");
    assert!(
        status.success(),
        "child for {input:?} exited with {status} (panic?); stdout:\n{out}"
    );
    let line = out
        .lines()
        // libtest prints `test bounded_child_entry ... ` on the same line.
        .find_map(|l| l.find(RESULT_TAG).map(|i| &l[i + RESULT_TAG.len()..]))
        .unwrap_or_else(|| panic!("no result line for {input:?}; stdout:\n{out}"));
    let mut parts = line.split_whitespace();
    let risk = match parts.next() {
        Some("Low") => Risk::Low,
        Some("Medium") => Risk::Medium,
        Some("High") => Risk::High,
        other => panic!("unparseable risk {other:?} for {input:?}"),
    };
    let reversible = match parts.next() {
        Some("true") => true,
        Some("false") => false,
        other => panic!("unparseable reversible {other:?} for {input:?}"),
    };
    RiskAssessment { risk, reversible }
}

fn high() -> RiskAssessment {
    RiskAssessment {
        risk: Risk::High,
        reversible: false,
    }
}

fn low() -> RiskAssessment {
    RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    }
}

// ---------------------------------------------------------------------------
// 1. Termination + behaviour (targets 262:66, 263:32, 280:28)
// ---------------------------------------------------------------------------

/// Multi-token segment, no leading whitespace: the path is the 2nd token,
/// not in command position -> blanked -> `Low`.
#[test]
fn multi_token_segment_terminates_and_blanks_named_path() {
    assert_eq!(classify_bounded("edit scripts/rollout.py now"), low());
}

/// Leading whitespace before the first token (the cursor must advance past
/// a non-zero `start_rel` on the very first iteration).
#[test]
fn leading_whitespace_segment_terminates_and_blanks_named_path() {
    assert_eq!(classify_bounded("   edit scripts/rollout.py"), low());
}

/// Leading whitespace, single token in command position -> kept -> `High`.
#[test]
fn leading_whitespace_single_command_token_terminates_and_stays_visible() {
    assert_eq!(classify_bounded("  scripts/rollout.py"), high());
}

/// Runner word puts the path in command position -> kept -> `High`.
#[test]
fn runner_word_path_terminates_and_stays_visible() {
    assert_eq!(classify_bounded("run scripts/rollout.py now"), high());
}

/// Whitespace in the segment after a span (segment index 2).
#[test]
fn segment_after_span_terminates_and_blanks_named_path() {
    assert_eq!(
        classify_bounded("see `x` and edit scripts/rollout.py"),
        low()
    );
}

// ---------------------------------------------------------------------------
// 2. Backtick parity (targets 255:14 `%`->`/`, `%`->`+`)
// ---------------------------------------------------------------------------

/// Control: the same words without backticks are blanked.
#[test]
fn parity_control_no_span_blanks() {
    assert_eq!(classify_bounded("edit scripts/rollout.py"), low());
}

/// 1 span, the path is a non-first token INSIDE it (segment 1, odd) -> kept.
#[test]
fn parity_one_span_inside_is_kept() {
    assert_eq!(classify_bounded("`edit scripts/rollout.py`"), high());
}

/// 1 span, the path is outside AFTER it (segment 2, even) -> blanked.
#[test]
fn parity_one_span_after_is_blanked() {
    assert_eq!(classify_bounded("x `y` edit scripts/rollout.py"), low());
}

/// 2 spans, the path inside the second (segment 3, odd) -> kept.
#[test]
fn parity_two_spans_inside_second_is_kept() {
    assert_eq!(
        classify_bounded("`a` and `edit scripts/rollout.py`"),
        high()
    );
}

/// 2 spans, the path outside after both (segment 4, even) -> blanked.
#[test]
fn parity_two_spans_after_is_blanked() {
    assert_eq!(
        classify_bounded("`a` b `c` and edit scripts/rollout.py"),
        low()
    );
}

/// 3 spans, the path inside the third (segment 5, odd) -> kept.
#[test]
fn parity_three_spans_inside_third_is_kept() {
    assert_eq!(
        classify_bounded("`a` b `c` d `edit scripts/rollout.py`"),
        high()
    );
}

/// 3 spans, the path outside after all (segment 6, even) -> blanked.
#[test]
fn parity_three_spans_after_is_blanked() {
    assert_eq!(
        classify_bounded("`a` b `c` d `e` and edit scripts/rollout.py"),
        low()
    );
}

/// Unbalanced: 5 backticks, the path after the last one (segment 5) is
/// "everything after an unbalanced backtick" -> kept.
#[test]
fn parity_unbalanced_trailing_is_kept() {
    assert_eq!(
        classify_bounded("`a` b `c` d `edit scripts/rollout.py"),
        high()
    );
}
