// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![cfg(feature = "fault-injection")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED against the STUB: `boundary::fault` (backlog 8696dd7e, slice 1) is
//! meant to let a caller force each of the 4 boundary IO entries to
//! `Determination::Undetermined` on demand, and `degrade::assert_fails_closed`
//! is meant to run a closure fully blind and assert it did not come back
//! permissive.
//!
//! The current `FaultPlan`/`with_fault_plan` never actually installs anything
//! (`faults()` always returns `false`, `with_fault_plan` never forces a
//! boundary call to `Undetermined`, `injected` is always `0`), and
//! `assert_fails_closed` is a bare `f()` with no assertion at all. Every test
//! here that actually exercises injection is expected to fail against that
//! stub — that is the RED this file exists to produce.
//!
//! Anti-vacuity is load-bearing throughout: every "this entry is faulted"
//! assertion is paired with a "this OTHER entry, or the same call outside any
//! scope, stays real" assertion on input that genuinely succeeds (an existing
//! readable temp file, an existing directory, `sh -c 'exit 0'`). Without the
//! pairing, a `FaultPlan` that faulted *everything* unconditionally would pass
//! the same tests.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harness_core::boundary::fault::{with_fault_plan, Entry, FaultPlan};
use harness_core::boundary::{self, CommandOutput};
use harness_core::degrade::{self, Permissiveness};
use harness_core::verdict::{Determination, Verdict};

// ---- shared fixtures ----------------------------------------------------

/// A directory containing one file, so `read_dir_entries` has something real
/// to observe.
fn dir_with_one_entry() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("present.txt"), b"x").unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// An existing, readable file with known contents.
fn readable_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readable.txt");
    std::fs::write(&path, b"hello").unwrap();
    (dir, path)
}

/// A command that genuinely succeeds (`sh -c 'exit 0'`), exactly the pattern
/// `boundary.rs`'s own unit tests use.
fn succeeding_command() -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("exit 0");
    cmd
}

#[track_caller]
fn assert_known<T: std::fmt::Debug>(d: &Determination<T>, msg: &str) {
    assert!(
        matches!(d, Determination::Known(_)),
        "{msg}: expected Known, got {d:?}"
    );
}

#[track_caller]
fn assert_undetermined<T: std::fmt::Debug>(d: &Determination<T>, msg: &str) {
    assert!(
        matches!(d, Determination::Undetermined(_)),
        "{msg}: expected Undetermined, got {d:?}"
    );
}

// ---- baseline: no injection outside any scope ----------------------------

/// Anti-vacuity control for the whole file: outside `with_fault_plan`, every
/// input that genuinely succeeds is `Known`. If this test does not pass, the
/// faulted-arm tests below prove nothing (they could be observing a plan-
/// independent default rather than actual injection).
#[test]
fn outside_any_scope_boundary_calls_are_real() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    assert_known(&boundary::read_dir_entries(&dir), "read_dir_entries");
    assert_known(&boundary::read_to_string(&file), "read_to_string");
    assert_known(&boundary::run(&mut succeeding_command()), "run");
    assert_known(
        &boundary::run_with_timeout(&mut succeeding_command(), Duration::from_secs(5)),
        "run_with_timeout",
    );
    assert_known(
        &boundary::run_with_timeout_and_stdin(
            &mut succeeding_command(),
            Duration::from_secs(5),
            None,
        ),
        "run_with_timeout_and_stdin",
    );
}

/// `FaultPlan::none()` must behave identically to no scope at all: it faults
/// nothing.
#[test]
fn none_plan_faults_nothing() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::none(), || {
        assert_known(&boundary::read_dir_entries(&dir), "read_dir_entries");
        assert_known(&boundary::read_to_string(&file), "read_to_string");
        assert_known(&boundary::run(&mut succeeding_command()), "run");
        assert_known(
            &boundary::run_with_timeout(&mut succeeding_command(), Duration::from_secs(5)),
            "run_with_timeout",
        );
    });
    assert_eq!(faulted.injected, 0, "FaultPlan::none() must inject nothing");
}

// ---- each of the 4 entries, faulted individually --------------------------

#[test]
fn only_read_dir_faults_read_dir_and_nothing_else() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::only(Entry::ReadDir), || {
        assert_undetermined(
            &boundary::read_dir_entries(&dir),
            "read_dir_entries faulted",
        );
        // Anti-vacuity: a plan that faults ONLY ReadDir must leave ReadFile
        // and Run observing reality.
        assert_known(
            &boundary::read_to_string(&file),
            "read_to_string unaffected",
        );
        assert_known(&boundary::run(&mut succeeding_command()), "run unaffected");
    });
    assert_eq!(faulted.injected, 1, "exactly the one faulted call counted");
}

#[test]
fn only_read_file_faults_read_file_and_nothing_else() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        assert_undetermined(&boundary::read_to_string(&file), "read_to_string faulted");
        assert_known(
            &boundary::read_dir_entries(&dir),
            "read_dir_entries unaffected",
        );
        assert_known(&boundary::run(&mut succeeding_command()), "run unaffected");
    });
    assert_eq!(faulted.injected, 1);
}

#[test]
fn only_run_faults_run_and_nothing_else() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::only(Entry::Run), || {
        let out = boundary::run(&mut succeeding_command());
        assert_undetermined(&out, "run faulted");
        assert_known(
            &boundary::read_dir_entries(&dir),
            "read_dir_entries unaffected",
        );
        assert_known(
            &boundary::read_to_string(&file),
            "read_to_string unaffected",
        );
        assert_known(
            &boundary::run_with_timeout(&mut succeeding_command(), Duration::from_secs(5)),
            "run_with_timeout is a DIFFERENT entry, must be unaffected",
        );
    });
    assert_eq!(faulted.injected, 1);
}

#[test]
fn only_run_with_timeout_faults_both_of_its_functions_but_not_plain_run() {
    let faulted = with_fault_plan(FaultPlan::only(Entry::RunWithTimeout), || {
        assert_undetermined(
            &boundary::run_with_timeout(&mut succeeding_command(), Duration::from_secs(5)),
            "run_with_timeout faulted",
        );
        assert_undetermined(
            &boundary::run_with_timeout_and_stdin(
                &mut succeeding_command(),
                Duration::from_secs(5),
                None,
            ),
            "run_with_timeout_and_stdin faulted (same Entry::RunWithTimeout)",
        );
        // `run` is a separate entry (`Entry::Run`), must stay real.
        assert_known(
            &boundary::run(&mut succeeding_command()),
            "plain run unaffected",
        );
    });
    assert_eq!(
        faulted.injected, 2,
        "both run_with_timeout and run_with_timeout_and_stdin calls counted"
    );
}

/// Covers `run_with_timeout_and_stdin` with an actual stdin payload, not just
/// `None`, so a fix that only special-cases the `None` path is still caught.
#[test]
fn run_with_timeout_and_stdin_with_payload_is_faulted_under_blind() {
    let faulted = with_fault_plan(FaultPlan::blind(), || {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("cat");
        cmd.stdin(std::process::Stdio::piped());
        boundary::run_with_timeout_and_stdin(
            &mut cmd,
            Duration::from_secs(5),
            Some(b"payload".to_vec()),
        )
    });
    assert_undetermined(
        &faulted.value,
        "run_with_timeout_and_stdin with stdin faulted",
    );
    assert_eq!(faulted.injected, 1);
}

// ---- `only` / `with` / `faults` construction ------------------------------

#[test]
fn faults_reports_plan_membership() {
    let plan = FaultPlan::only(Entry::ReadFile);
    assert!(plan.faults(Entry::ReadFile));
    assert!(!plan.faults(Entry::ReadDir));
    assert!(!plan.faults(Entry::Run));
    assert!(!plan.faults(Entry::RunWithTimeout));

    let none = FaultPlan::none();
    for entry in Entry::ALL {
        assert!(
            !none.faults(*entry),
            "FaultPlan::none() must fault nothing: {entry:?}"
        );
    }

    let blind = FaultPlan::blind();
    for entry in Entry::ALL {
        assert!(
            blind.faults(*entry),
            "FaultPlan::blind() must fault everything: {entry:?}"
        );
    }
}

#[test]
fn with_adds_an_entry_without_disturbing_others() {
    let plan = FaultPlan::only(Entry::ReadFile).with(Entry::Run);
    assert!(
        plan.faults(Entry::ReadFile),
        "the original entry must survive `with`"
    );
    assert!(plan.faults(Entry::Run), "the added entry must be present");
    assert!(
        !plan.faults(Entry::ReadDir),
        "an untouched entry must stay unfaulted"
    );
    assert!(!plan.faults(Entry::RunWithTimeout));
}

#[test]
fn with_on_none_builds_up_a_plan_incrementally() {
    let plan = FaultPlan::none()
        .with(Entry::ReadDir)
        .with(Entry::RunWithTimeout);
    assert!(plan.faults(Entry::ReadDir));
    assert!(plan.faults(Entry::RunWithTimeout));
    assert!(!plan.faults(Entry::ReadFile));
    assert!(!plan.faults(Entry::Run));
}

// ---- `blind` covers every entry when actually run --------------------------

#[test]
fn blind_faults_all_four_entries_when_actually_exercised() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::blind(), || {
        assert_undetermined(&boundary::read_dir_entries(&dir), "read_dir_entries");
        assert_undetermined(&boundary::read_to_string(&file), "read_to_string");
        assert_undetermined(&boundary::run(&mut succeeding_command()), "run");
        assert_undetermined(
            &boundary::run_with_timeout(&mut succeeding_command(), Duration::from_secs(5)),
            "run_with_timeout",
        );
    });
    assert_eq!(faulted.injected, 4);
}

// ---- the `injected` count --------------------------------------------------

#[test]
fn injected_counts_every_forced_call_not_just_distinct_entries() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    let faulted = with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        // Three separate calls into the SAME faulted entry.
        let _ = boundary::read_to_string(&file);
        let _ = boundary::read_to_string(&file);
        let _ = boundary::read_to_string(&file);
        // An unfaulted entry call must not add to the count.
        let _ = boundary::read_dir_entries(&dir);
    });
    assert_eq!(
        faulted.injected, 3,
        "injected must count the faulted calls only, one per call"
    );
}

#[test]
fn injected_is_zero_when_the_plan_faults_nothing_even_with_real_calls() {
    let (_file_guard, file) = readable_file();
    let faulted = with_fault_plan(FaultPlan::none(), || {
        let _ = boundary::read_to_string(&file);
        let _ = boundary::read_to_string(&file);
    });
    assert_eq!(faulted.injected, 0);
}

// ---- scoping: restore on normal return, on panic, and when nested ---------

/// After the scope returns normally, boundary calls outside it must go back
/// to being real — the plan must not leak past `with_fault_plan`.
#[test]
fn plan_is_restored_after_normal_return() {
    let (_file_guard, file) = readable_file();

    let _ = with_fault_plan(FaultPlan::blind(), || {
        assert_undetermined(&boundary::read_to_string(&file), "faulted inside the scope");
    });

    assert_known(
        &boundary::read_to_string(&file),
        "must be real again once the scope has returned",
    );
}

/// If `f` panics, the panic propagates out of `with_fault_plan` (spec: "the
/// panic propagates"), but the thread-local plan must still be restored to
/// whatever it was before — otherwise one panicking gate would leave every
/// later gate on the same thread permanently blind.
#[test]
fn plan_is_restored_even_when_f_panics() {
    let (_file_guard, file) = readable_file();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        with_fault_plan::<()>(FaultPlan::blind(), || {
            panic!("boom: simulating a gate that panics mid-check");
        })
    }));
    assert!(
        result.is_err(),
        "the panic must propagate out of with_fault_plan"
    );

    // The thread-local plan must be back to "no fault" — not stuck on the
    // blind plan the panicking scope installed.
    assert_known(
        &boundary::read_to_string(&file),
        "the panic must not leave the thread permanently faulted",
    );
}

/// Nested scopes restore the OUTER plan when the inner one exits, not the
/// no-fault default.
#[test]
fn nested_scope_restores_the_outer_plan_not_the_default() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let (_file_guard, file) = readable_file();

    with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        assert_undetermined(
            &boundary::read_to_string(&file),
            "outer plan faults ReadFile",
        );
        assert_known(
            &boundary::read_dir_entries(&dir),
            "outer plan leaves ReadDir alone",
        );

        with_fault_plan(FaultPlan::blind(), || {
            assert_undetermined(
                &boundary::read_to_string(&file),
                "inner blind plan faults ReadFile too",
            );
            assert_undetermined(
                &boundary::read_dir_entries(&dir),
                "inner blind plan faults ReadDir",
            );
        });

        // Back to the OUTER plan, not the top-level no-fault default.
        assert_undetermined(
            &boundary::read_to_string(&file),
            "outer plan must still be faulting ReadFile after the inner scope exits",
        );
        assert_known(
            &boundary::read_dir_entries(&dir),
            "outer plan must still be leaving ReadDir alone after the inner scope exits",
        );
    });

    assert_known(
        &boundary::read_to_string(&file),
        "top-level default restored",
    );
}

/// Nested scope also restores correctly when the INNER closure panics: the
/// outer plan must survive, not fall back to no-fault.
#[test]
fn nested_scope_restores_outer_plan_after_inner_panic() {
    let (_file_guard, file) = readable_file();

    with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            with_fault_plan::<()>(FaultPlan::blind(), || {
                panic!("boom: inner scope panics");
            })
        }));
        assert!(result.is_err());

        assert_undetermined(
            &boundary::read_to_string(&file),
            "outer plan must survive an inner panic, not reset to no-fault",
        );
    });
}

// ---- thread-locality -------------------------------------------------------

/// A thread spawned INSIDE `f` does not inherit the plan: its boundary calls
/// are real, and they must not be counted in the parent's `injected`.
#[test]
fn plan_is_thread_local_spawned_thread_is_unaffected_and_uncounted() {
    let (_file_guard, file) = readable_file();
    let file_for_thread = file.clone();

    let faulted = with_fault_plan(FaultPlan::blind(), || {
        // On a fresh thread, no plan is installed: this must observe reality.
        let handle = std::thread::spawn(move || boundary::read_to_string(&file_for_thread));
        let from_other_thread = handle.join().unwrap();
        assert_known(
            &from_other_thread,
            "a spawned thread must not inherit the plan",
        );

        // Back on this (the installing) thread, the same file IS faulted.
        assert_undetermined(
            &boundary::read_to_string(&file),
            "the installing thread stays faulted",
        );
    });

    assert_eq!(
        faulted.injected, 1,
        "only the call made on the plan's own thread is counted"
    );
}

// ---- `assert_fails_closed` -------------------------------------------------

/// A buggy gate that maps `Undetermined` to `Clean` (the exact fail-open
/// class CLAUDE.md names) must be caught: `assert_fails_closed` panics.
#[test]
fn assert_fails_closed_panics_on_a_gate_that_maps_undetermined_to_clean() {
    let (_file_guard, file) = readable_file();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        degrade::assert_fails_closed(|| {
            let _ = boundary::read_to_string(&file);
            // BUG: ignores whether the read was Known/Undetermined and always
            // reports Clean, the fail-open assert_fails_closed exists to catch.
            Verdict::from_findings(vec![])
        })
    }));
    assert!(
        result.is_err(),
        "a Clean verdict under a fully blind fault plan must panic assert_fails_closed"
    );
}

/// A closure that never reaches a boundary entry proves nothing about
/// fail-closed behaviour, even if it happens to return a restrictive verdict.
/// `assert_fails_closed` must panic on this vacuous case too (spec: "PANICS if
/// zero faults were injected").
#[test]
fn assert_fails_closed_panics_on_a_vacuous_closure_even_if_restrictive() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        degrade::assert_fails_closed(|| Verdict::violation("never touched the boundary at all"))
    }));
    assert!(
        result.is_err(),
        "zero injected faults must panic even though the verdict itself was restrictive"
    );
}

/// A correct gate that reads through the boundary and correctly forwards
/// `Undetermined` passes, and `assert_fails_closed` hands back the value.
#[test]
fn assert_fails_closed_passes_and_returns_the_value_for_a_correct_gate() {
    let (_file_guard, file) = readable_file();

    let verdict = degrade::assert_fails_closed(|| match boundary::read_to_string(&file) {
        Determination::Known(_) => Verdict::from_findings(vec![]),
        Determination::Undetermined(why) => Verdict::Undetermined(why),
    });
    assert!(
        matches!(verdict, Verdict::Undetermined(_)),
        "the correct gate must have observed Undetermined under the blind plan: {verdict:?}"
    );
}

/// If `f` itself panics (a crashing gate, not a faulty verdict), the panic
/// must propagate out of `assert_fails_closed` rather than being swallowed
/// into a pass/fail verdict.
#[test]
fn assert_fails_closed_propagates_a_panic_from_f() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        degrade::assert_fails_closed(|| -> Verdict {
            panic!("boom: the gate itself crashed");
        })
    }));
    assert!(
        result.is_err(),
        "a panic inside f must propagate, not be absorbed"
    );
}

// ---- `Permissiveness for Verdict` ------------------------------------------

#[test]
fn verdict_permissiveness_clean_is_permissive() {
    assert_eq!(Verdict::from_findings(vec![]).permissiveness(), 1);
}

#[test]
fn verdict_permissiveness_violation_and_undetermined_are_restrictive() {
    assert_eq!(Verdict::violation("x").permissiveness(), 0);
    assert_eq!(Verdict::undetermined("x").permissiveness(), 0);
}

/// Anti-vacuity: `Undetermined` and `Violation` must be ranked equal (both
/// 0), not accidentally distinguished — `degrade`'s own module docs say the
/// two are deliberately unordered against each other, and a `permissiveness`
/// that gave `Undetermined` a nonzero score would break `assert_fails_closed`
/// (a blind run legitimately produces `Undetermined`, and that must count as
/// fully restrictive, not partially permissive).
#[test]
fn verdict_permissiveness_undetermined_is_as_restrictive_as_violation() {
    assert_eq!(
        Verdict::undetermined("x").permissiveness(),
        Verdict::violation("x").permissiveness()
    );
}

// ---- sanity on the produced values, not just Known/Undetermined -----------

/// A faulted `run` really does produce an `Undetermined`, not e.g. a
/// `Known(CommandOutput)` with a sentinel/zeroed exit code — the reason this
/// asserts on the variant via a typed match rather than any `unwrap_or`-style
/// shortcut that could mask the payload's type changing under it.
#[test]
fn faulted_run_result_has_no_command_output_accessible() {
    let faulted = with_fault_plan(FaultPlan::only(Entry::Run), || {
        boundary::run(&mut succeeding_command())
    });
    match faulted.value {
        Determination::Undetermined(_) => {}
        Determination::Known(out) => {
            panic!(
                "expected Undetermined under Entry::Run fault, got Known({:?})",
                out.code()
            )
        }
    }
}

/// Same shape for `read_dir_entries`: the faulted result is not a `Known`
/// carrying an empty `Vec` (which would be indistinguishable from a
/// legitimately empty directory — the exact ambiguity this whole module
/// exists to prevent).
#[test]
fn faulted_read_dir_result_is_not_a_known_empty_vec() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let faulted = with_fault_plan(FaultPlan::only(Entry::ReadDir), || {
        boundary::read_dir_entries(&dir)
    });
    match faulted.value {
        Determination::Undetermined(_) => {}
        Determination::Known(entries) => panic!(
            "expected Undetermined under Entry::ReadDir fault, got Known({entries:?}) \
             — a faulted directory read must never look like a legitimately empty one"
        ),
    }
}

/// Silence an unused-import-ish concern in case a future edit narrows what
/// this file references: `CommandOutput` and `Path` are exercised through the
/// type-level assertions above, not constructed directly, so this keeps the
/// imports honest without an `#[allow(unused_imports)]`.
#[allow(dead_code)]
fn _type_witnesses(_: &CommandOutput, _: &Path) {}

// ---- read_dir_entries_checked honours the ReadDir entry --------------------

/// `read_dir_entries_checked` is the same directory-listing observation as
/// `read_dir_entries`, so it shares `Entry::ReadDir`. Faulted, it must be
/// `Undetermined` even for a path that does not exist: a blind boundary has
/// not observed absence, so it must not report `Known(None)`.
#[test]
fn read_dir_entries_checked_is_faulted_by_read_dir_and_by_blind() {
    let (_dir_guard, dir) = dir_with_one_entry();
    let absent = dir.join("does-not-exist");

    // Control: outside a plan both inputs are Known.
    assert_known(
        &boundary::read_dir_entries_checked(&dir),
        "read_dir_entries_checked control (present)",
    );
    assert_known(
        &boundary::read_dir_entries_checked(&absent),
        "read_dir_entries_checked control (absent)",
    );

    for plan in [FaultPlan::only(Entry::ReadDir), FaultPlan::blind()] {
        let faulted = with_fault_plan(plan.clone(), || {
            assert_undetermined(
                &boundary::read_dir_entries_checked(&dir),
                "read_dir_entries_checked faulted (present)",
            );
            assert_undetermined(
                &boundary::read_dir_entries_checked(&absent),
                "read_dir_entries_checked faulted (absent)",
            );
        });
        assert_eq!(faulted.injected, 2, "{plan:?} must inject on both calls");
    }

    // Anti-vacuity: a plan that faults something else leaves it observing.
    let faulted = with_fault_plan(FaultPlan::only(Entry::ReadFile), || {
        assert_known(
            &boundary::read_dir_entries_checked(&absent),
            "read_dir_entries_checked under ReadFile-only plan",
        );
    });
    assert_eq!(faulted.injected, 0);
}
