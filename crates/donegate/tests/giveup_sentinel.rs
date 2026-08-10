//! End-to-end: donegate's attempt cap must leave a DURABLE SENTINEL
//! (backlog 5151605e part 3).
//!
//! Once `attempt > max_attempts` the gate prints "Allowing stop" and exits 0.
//! That is a deliberate anti-trap escape and this test does NOT change it — it
//! pins it. What it does require is that the give-up stop being
//! *indistinguishable from a pass* ends: a human or a script must be able to
//! ask afterwards whether the gate enforced or gave up.
//!
//! Everything runs against the real built binary with `$HOME` set per child
//! process, so no process-global env is mutated and neither the real
//! `~/.harness/trust.toml` nor the real `~/.overwatch` store is touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("donegate-giveup-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// A trusted project rooted under an isolated `$HOME`, carrying exactly one
/// required check running `cmd`. Returns (home, project root).
///
/// The project MUST be trusted or `Config::load` ignores `donegate.toml`
/// outright and the gate has no checks at all — the test would then pass
/// vacuously on a gate that never ran anything.
fn project(tag: &str, cmd: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");

    std::fs::write(
        root.join("donegate.toml"),
        format!("max_attempts = 3\n\n[[check]]\nname = \"typecheck\"\ncmd = \"{cmd}\"\n"),
    )
    .expect("write donegate.toml");

    // `trust::is_trusted` canonicalizes before comparing, so the seeded entry
    // must be the canonical form (on macOS $TMPDIR resolves through /private).
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");

    (home, root)
}

struct GateRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl GateRun {
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\"") && self.stdout.contains("block")
    }
}

fn run_gate(home: &Path, root: &Path, session: &str) -> GateRun {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{}","hook_event_name":"Stop"}}"#,
        root.display()
    );
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(root)
        .env("HOME", home)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("donegate runs");
    GateRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Concatenated contents of every `violations.jsonl` under the isolated
/// `$HOME`. Read straight off disk rather than through `overwatch::store` so
/// the observation does not depend on the code under test, and located by
/// walking rather than by recomputing the project-key hash.
fn violation_ledger(home: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "violations.jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(home, &mut out);
    out
}

/// THE FINDING: a persistent failure that exhausts `max_attempts` is allowed
/// through, and (before this change) left nothing durable saying so. The
/// give-up must become queryable — and the two controls below make sure the
/// sentinel is not simply written unconditionally.
#[test]
fn reaching_the_attempt_cap_leaves_a_durable_giveup_sentinel() {
    let (home, root) = project("cap", "exit 1");
    let session = "sess-cap";

    // Attempts 1..=3 are under the cap: the gate ENFORCES.
    for i in 1..=3 {
        let r = run_gate(&home, &root, session);
        assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
        assert!(
            r.blocked(),
            "attempt {i} is under max_attempts=3 and must block; stdout={:?} stderr={:?}",
            r.stdout,
            r.stderr
        );
    }

    // CONTROL 1 (no false positive): under the cap the gate has judged and
    // enforced, so the ledger must already carry ordinary block events but NOT
    // a give-up marker. Without this, a sentinel written on every block would
    // satisfy the assertion further down while proving nothing.
    let under_cap = violation_ledger(&home);
    assert!(
        under_cap.contains("donegate:typecheck"),
        "apparatus: blocks must reach the violation ledger, else the give-up assertion below \
         would be comparing two empty ledgers. ledger={under_cap:?}"
    );
    assert!(
        !under_cap.contains("giveup"),
        "a give-up sentinel must NOT appear while the gate is still enforcing. ledger={under_cap:?}"
    );

    // The 4th Stop exceeds the cap: the gate gives up and ALLOWS the stop.
    let r = run_gate(&home, &root, session);
    assert_eq!(r.code, 0, "the give-up path exits 0");
    assert!(
        !r.blocked(),
        "IN SCOPE-CHECK: this change must not alter whether the cap allows the stop. \
         stdout={:?}",
        r.stdout
    );
    assert!(
        r.stderr.contains("still failing"),
        "apparatus: the give-up branch must be the one that ran; stderr={:?}",
        r.stderr
    );

    let after_cap = violation_ledger(&home);
    assert!(
        after_cap.contains("donegate:giveup:typecheck"),
        "the gate gave up on a still-red check and left no durable trace: 'the gate gave up' is \
         then indistinguishable from 'the gate passed' for every downstream reader. Expected a \
         `donegate:giveup:typecheck` event in the overwatch violation ledger. ledger={after_cap:?}"
    );
}

/// CONTROL 2 (no unconditional write): a green check allows the stop on the
/// first Stop, and that ordinary allow must leave NO sentinel — otherwise the
/// marker means "donegate ran", not "donegate gave up".
#[test]
fn a_normal_green_allow_leaves_no_sentinel() {
    let (home, root) = project("green", "exit 0");

    let r = run_gate(&home, &root, "sess-green");
    assert_eq!(r.code, 0);
    assert!(
        !r.blocked(),
        "a green check must allow the stop; stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );

    let ledger = violation_ledger(&home);
    assert!(
        !ledger.contains("giveup"),
        "an ordinary green allow must leave no give-up sentinel. ledger={ledger:?}"
    );
    assert!(
        ledger.trim().is_empty(),
        "a green run records no violations at all. ledger={ledger:?}"
    );
}

/// Drive `max_attempts + 1` Stops so the cap is exhausted exactly once.
/// The give-up branch resets the attempt counter, so calling this N times
/// produces N give-ups for the same session.
/// Returns the give-up run itself.
fn one_giveup_cycle(home: &Path, root: &Path, session: &str) -> GateRun {
    for _ in 0..3 {
        let r = run_gate(home, root, session);
        assert!(r.blocked(), "apparatus: under the cap donegate must block");
    }
    let r = run_gate(home, root, session);
    assert!(
        r.stderr.contains("still failing"),
        "apparatus: the give-up branch must have run; stderr={:?}",
        r.stderr
    );
    r
}

/// Every `donegate:giveup:*` event in the isolated `$HOME`'s ledger.
fn giveup_events(home: &Path) -> Vec<overwatch::violation::ViolationEvent> {
    violation_ledger(home)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<overwatch::violation::ViolationEvent>(l).ok())
        .filter(|e| e.signature.starts_with("donegate:giveup:"))
        .collect()
}

/// WHICH SURFACE SHOWS A GIVE-UP — the prose in `main.rs`'s give-up branch is
/// a claim about `overwatch review-queue`, so it gets measured rather than
/// asserted in a comment (CLAUDE.md §2; the earlier version of that comment
/// said review-queue "starts carrying" a repeatedly-capping check "once
/// recurrence crosses the threshold", which is false).
///
/// review-queue's systemic stream is exactly `is_systemic`
/// (`crates/overwatch/src/review_queue.rs`: `.filter(|r| r.is_systemic)`), and
/// `is_systemic` requires `distinct_tasks > 1 || distinct_sessions > 1` on top
/// of the occurrence threshold. donegate passes its session key as BOTH fields,
/// so a single session can never satisfy the second conjunct — no matter how
/// many times it gives up.
#[test]
fn repeated_giveups_in_one_session_never_become_systemic() {
    use overwatch::violation::{systemic_issues, RecurrencePolicy};

    let (home, root) = project("systemic-one", "exit 1");
    for _ in 0..3 {
        one_giveup_cycle(&home, &root, "sess-only-one");
    }

    let events = giveup_events(&home);
    assert_eq!(
        events.len(),
        3,
        "apparatus: three give-up cycles must have recorded three give-up events, else the \
         emptiness below would be vacuous. ledger={:?}",
        violation_ledger(&home)
    );
    let policy = RecurrencePolicy::default();
    assert!(
        events.len() >= policy.threshold,
        "apparatus: the occurrence threshold ({}) must be MET, so that the only thing keeping \
         this out of review-queue is the task/session conjunct",
        policy.threshold
    );

    let systemic = systemic_issues(&events, overwatch::store::now(), policy);
    assert!(
        systemic.is_empty(),
        "a single session's repeated give-ups escalated to SYSTEMIC — the surface documented in \
         main.rs would then be a first-recurrence channel after all. got: {systemic:?}"
    );
}

/// ANTI-VACUITY CONTROL for the test above: the give-up signature is not
/// permanently invisible to review-queue. The SAME check giving up in two
/// distinct sessions does escalate — which is the (narrower) property the
/// comment in `main.rs` is now allowed to claim.
#[test]
fn giveups_across_two_sessions_do_become_systemic() {
    use overwatch::violation::{systemic_issues, RecurrencePolicy};

    let (home, root) = project("systemic-two", "exit 1");
    one_giveup_cycle(&home, &root, "sess-a");
    one_giveup_cycle(&home, &root, "sess-a");
    one_giveup_cycle(&home, &root, "sess-b");

    let events = giveup_events(&home);
    assert_eq!(events.len(), 3, "apparatus: three give-up events");

    let systemic = systemic_issues(
        &events,
        overwatch::store::now(),
        RecurrencePolicy::default(),
    );
    assert_eq!(
        systemic.len(),
        1,
        "the same check giving up in two distinct sessions must escalate to systemic (that is \
         the channel main.rs documents); got: {systemic:?}"
    );
    assert_eq!(systemic[0].signature, "donegate:giveup:typecheck");
}

/// THE LEDGER WRITE CAN FAIL, AND THAT FAILURE MUST BE VISIBLE.
///
/// `emit_violations` is the only thing that makes a give-up durable, so an
/// `append_violation` error is the case where the gate gives up AND leaves no
/// trace — precisely the state the sentinel exists to rule out. Swallowing the
/// error (`let _ = …`) still compiles and still passes every other test in this
/// file, because they all run against a writable store: the diagnostic was
/// unpinned. This test pins it by injecting a real fault — `~/.overwatch` is a
/// regular FILE, so the store's `create_dir_all` cannot succeed.
///
/// It asserts on stderr because stderr is where the diagnostic goes today. That
/// channel is transcript-bound — the same weakness `main.rs`'s give-up comment
/// names as the reason the sentinel had to be durable in the first place — so
/// this test pins the CURRENT contract, not an endorsement of it: if the
/// warning ever moves to a durable channel, this test should be updated to
/// follow it, not deleted.
#[test]
fn a_giveup_that_cannot_be_recorded_says_so() {
    let (home, root) = project("unwritable", "exit 1");

    // Fault injection: a file where the store needs a directory.
    std::fs::write(home.join(".overwatch"), b"not a directory").expect("inject fault");

    for _ in 0..3 {
        let _ = run_gate(&home, &root, "sess-unwritable");
    }
    let r = run_gate(&home, &root, "sess-unwritable");

    assert_eq!(r.code, 0, "the give-up path still exits 0");
    assert!(
        r.stderr.contains("still failing"),
        "apparatus: the give-up branch must have run; stderr={:?}",
        r.stderr
    );
    // Apparatus: the fault really landed — nothing durable was written, so the
    // stderr line below is the ONLY trace that exists.
    assert!(
        giveup_events(&home).is_empty(),
        "apparatus: the injected fault must have prevented the ledger write; ledger={:?}",
        violation_ledger(&home)
    );

    assert!(
        r.stderr.contains("could not record"),
        "the give-up could not be recorded and donegate said nothing about it: the stop is then \
         indistinguishable from a pass with NO trace anywhere. stderr={:?}",
        r.stderr
    );
    assert!(
        r.stderr.contains("donegate:giveup:typecheck"),
        "the warning must name the signature that was lost; stderr={:?}",
        r.stderr
    );
    assert!(
        r.stderr.contains("NOT represented"),
        "the warning must say the run is absent from the ledger; stderr={:?}",
        r.stderr
    );
}

/// ANTI-VACUITY CONTROL for the test above: the warning is conditional on an
/// actual failure, not printed on every give-up (which would make the assertion
/// above pass against a gate that always cries wolf).
#[test]
fn a_recordable_giveup_prints_no_failure_warning() {
    let (home, root) = project("recordable", "exit 1");
    let r = one_giveup_cycle(&home, &root, "sess-ok");
    assert!(
        !r.stderr.contains("could not record"),
        "a give-up that WAS recorded must not warn about a failed record; stderr={:?}",
        r.stderr
    );
    assert!(
        !giveup_events(&home).is_empty(),
        "apparatus: the give-up must actually be in the ledger"
    );
}
