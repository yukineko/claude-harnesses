//! `condukt state check-oracle` — deterministically ask `tdd oracle` whether a
//! fix/feature task's RED→GREEN proofs form a valid Fail→Pass reproduction
//! oracle. A thin wrapper that never panics and never exits nonzero, but it is
//! **not uniformly fail-soft** — two different failures mean two different
//! things:
//!
//! - **The check does not apply, or ran but had nothing to report**: not a
//!   fix/feature task, or `tdd` ran and produced missing/corrupt stdout.
//!   `fallback:true`, which `state::enforce_fp_gate` Allows — the legacy
//!   `done_criteria` gate takes over. (A fix/feature task that merely did not
//!   *declare* `reproduction_tests` is NOT in this bucket: it still consults
//!   `tdd`. The only exemption is `!requires_oracle`.)
//! - **The oracle could not be determined**: `tdd` exited non-zero, or `tdd`
//!   could not be spawned at all (not installed, not executable, a gone
//!   worktree). Nothing was established, and that is *undetermined*, not
//!   *fine*: `fallback:false` with `required`/`!valid`, which
//!   `enforce_fp_gate` Rejects. See [`verdict_from_oracle_output`] and
//!   [`check_oracle`]'s spawn-failure branch.
//!
//! Collapsing the second case into the first is how a crashed — or entirely
//! absent — checker passes for a checker that had nothing to say. An
//! environment without `tdd` installed must be told to install it, not quietly
//! granted a pass on every fix/feature task.

use std::path::Path;
use std::process::Command;

/// Parse `tdd oracle`'s stdout JSON, returning `(valid_fp_oracle, transition)`.
/// Corrupt/non-object stdout is reported as `(false, None)` rather than
/// panicking — this is a pure helper so it is fully unit-testable without
/// spawning a real `tdd` process.
pub fn interpret_oracle_stdout(stdout: &str) -> (bool, Option<String>) {
    match serde_json::from_str::<serde_json::Value>(stdout) {
        Ok(v) => {
            let valid = v
                .get("valid_fp_oracle")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let transition = v
                .get("transition")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            (valid, transition)
        }
        Err(_) => (false, None),
    }
}

/// Build the `check_oracle` verdict from a parsed `(valid, transition)` pair.
///
/// The crux of the Fail→Pass gate's fallback contract: a `tdd oracle` run that
/// completes and parses is *not* automatically a trustworthy reject signal.
/// `transition == "unknown"` means the proof pair was incomplete (missing RED
/// or GREEN artifact — `has_red`/`has_green` false), i.e. an oracle *could not
/// be generated* for this task. Per the charter DoD ("オラクル生成不能時の
/// fallback") that is a *can't-generate* condition and must degrade to the
/// legacy `done_criteria` gate (`fallback: true` → `enforce_fp_gate` Allows),
/// NOT hard-reject. Only a *complete* proof pair that ran the wrong direction
/// (`fail_to_fail` / `pass_to_pass` / `pass_to_fail` — a real, trustworthy
/// verdict) keeps `fallback: false` so the gate rejects it.
///
/// Split out as a pure function so the unknown→fallback vs wrong-direction→
/// reject distinction is unit-testable without spawning `tdd`.
pub fn verdict_from_oracle(valid: bool, transition: Option<&str>) -> serde_json::Value {
    // "unknown" == incomplete proofs == oracle could not be generated. Degrade
    // to the legacy gate rather than treating "no proofs" as a definitive
    // "invalid oracle" reject. (A valid FailToPass is never "unknown", so the
    // `!valid` guard is belt-and-suspenders.)
    let is_unknown = transition == Some("unknown");
    if is_unknown && !valid {
        return serde_json::json!({
            "required": true,
            "valid_fp_oracle": false,
            "fallback": true,
            "transition": "unknown",
            "reason": "no valid Fail→Pass proofs recorded (oracle could not be generated) — degrade to legacy done_criteria gate",
        });
    }
    serde_json::json!({
        "required": true,
        "valid_fp_oracle": valid,
        "fallback": false,
        "transition": transition,
        "reason": if valid {
            "fail-to-pass oracle confirmed"
        } else {
            "tdd oracle reported a complete but non-fail-to-pass transition"
        },
    })
}

/// Decide whether `task_id` (whose worktree is `run_dir`) carries a valid
/// Fail→Pass reproduction oracle, deferring to `tdd oracle --task <id>` run in
/// `run_dir`. Pure aside from the one external process spawn; never panics.
///
/// The only exemption from the F→P gate is `!requires_oracle` (not a
/// fix/feature task). A fix/feature task is ALWAYS consulted against `tdd`,
/// whether or not it declared `reproduction_tests` — missing
/// `reproduction_tests` no longer short-circuits to an exempt verdict, because
/// real tdd proofs can exist for a task that never declared them.
///
/// Always returns a JSON object with a `fallback` bool. `true` means "the
/// oracle check does not apply, or `tdd` ran but had nothing usable to say —
/// degrade to the legacy gate".
///
/// `false` means the gate must decide on this verdict rather than defer, and it
/// arises three ways that are **not** interchangeable:
///
/// - the run produced a real verdict (`valid_fp_oracle`/`transition` reflect
///   what `tdd` actually observed),
/// - the run exited non-zero, so there is no verdict to reflect and
///   `valid_fp_oracle` is `false` because nothing was established — see
///   [`verdict_from_oracle_output`], or
/// - `tdd` could not be spawned at all (not installed / not executable). Also
///   nothing established; the `reason` says so and asks for `tdd` to be
///   installed.
///
/// The `reason` field is what distinguishes the three; do not read
/// `valid_fp_oracle: false` here as "tdd looked and found the proofs invalid".
pub fn check_oracle(
    requires_oracle: bool,
    // Retained for signature stability (callers in `main.rs` and the tests pass
    // it positionally). It is NO LONGER a gate switch: a fix/feature task is
    // consulted against `tdd` regardless of whether it declared
    // `reproduction_tests`. See the `!requires_oracle`-only guard below.
    _reproduction_tests: Option<&str>,
    task_id: &str,
    run_dir: &Path,
) -> serde_json::Value {
    if !requires_oracle {
        return serde_json::json!({
            "required": false,
            "valid_fp_oracle": false,
            "fallback": true,
            "reason": "not a fix/feature task",
        });
    }

    match Command::new("tdd")
        .args(["oracle", "--task", task_id])
        .current_dir(run_dir)
        .output()
    {
        Ok(out) => {
            verdict_from_oracle_output(&String::from_utf8_lossy(&out.stdout), out.status.success())
        }
        // A spawn failure is `cannot determine`, NOT `unavailable`. The most
        // common cause is that `tdd` is not installed at all, and that used to
        // yield `fallback:true` → `enforce_fp_gate` Allow — i.e. an environment
        // simply missing the checker passed the F→P gate for every fix/feature
        // task, silently, forever. A missing checker is not a passing checker.
        //
        // Deliberately NOT branched on `e.kind()`: NotFound, EACCES and ENOMEM
        // are all "no verdict was obtained", and splitting them would create a
        // permissive path no test covers.
        Err(e) => serde_json::json!({
            "required": true,
            "valid_fp_oracle": false,
            "fallback": false,
            "reason": format!(
                "failed to spawn tdd ({e}) — the F→P oracle could not be determined. \
                 Install/provide the `tdd` binary on PATH; a missing checker is not a \
                 passing checker, so this blocks rather than degrading to the legacy gate"
            ),
        }),
    }
}

/// Derive the F→P verdict from a finished `tdd oracle` run: its stdout plus
/// whether it exited 0.
///
/// Split out as a pure function so the exit-status branch is testable without a
/// subprocess — `check_oracle` above does nothing but spawn and hand both
/// signals here.
///
/// **A non-zero exit is `cannot determine`, not `fallback`.** The distinction
/// matters because `fallback: true` makes `state::enforce_fp_gate` *Allow*: a
/// crashed checker would otherwise be indistinguishable from a checker that
/// legitimately could not generate an oracle, and the gate would pass. A
/// checker that fell over is not a checker that passed, so a non-zero exit
/// produces `required/!fallback/!valid` — the exact shape `enforce_fp_gate`
/// turns into `Reject`.
///
/// This deliberately ignores whatever the failed process printed. A run that
/// exited non-zero while claiming `valid_fp_oracle: true` on stdout is claiming
/// something it did not finish establishing; trusting that self-report would
/// hand the gate back to the party being gated.
pub fn verdict_from_oracle_output(stdout: &str, exit_ok: bool) -> serde_json::Value {
    if !exit_ok {
        return serde_json::json!({
            "required": true,
            "valid_fp_oracle": false,
            "fallback": false,
            "reason": "tdd oracle exited non-zero — the F→P oracle could not be \
                       determined, which is not the same as it being unavailable",
        });
    }
    if stdout.trim().is_empty() {
        return serde_json::json!({
            "required": true,
            "valid_fp_oracle": false,
            "fallback": true,
            "reason": "tdd oracle produced no stdout",
        });
    }
    // Confirm the stdout is well-formed JSON before trusting the verdict;
    // `interpret_oracle_stdout` already defaults missing fields to false/None,
    // but corrupt/non-JSON stdout must degrade to fallback rather than silently
    // reporting `valid_fp_oracle:false`.
    if serde_json::from_str::<serde_json::Value>(stdout).is_err() {
        return serde_json::json!({
            "required": true,
            "valid_fp_oracle": false,
            "fallback": true,
            "reason": "could not parse tdd oracle stdout as JSON",
        });
    }
    let (valid, transition) = interpret_oracle_stdout(stdout);
    verdict_from_oracle(valid, transition.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_required_falls_back_immediately() {
        let out = check_oracle(false, Some("cargo test -p x"), "t1", Path::new("."));
        assert_eq!(out["required"], false);
        assert_eq!(out["fallback"], true);
        assert_eq!(out["valid_fp_oracle"], false);
    }

    /// Post-fix contract (backlog 22b69f6a): a fix/feature task
    /// (`requires_oracle: true`) that declared no `reproduction_tests` is NOT
    /// exempted — it must still consult `tdd`. Against a fresh empty run dir
    /// with no recorded proofs the result is `required: true` in both worlds
    /// (tdd present-but-no-proofs → unknown fallback, or tdd absent → spawn
    /// Err), never the old `required: false` exemption.
    #[test]
    fn no_reproduction_tests_falls_back_even_when_required() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = check_oracle(true, None, "t1", tmp.path());
        assert_eq!(
            out["required"], true,
            "a fix/feature task without reproduction_tests must still consult tdd — got {out}"
        );
        assert_eq!(out["valid_fp_oracle"], false, "{out}");
        assert_ne!(
            out["reason"], "not a fix/feature task",
            "must not resolve via the !requires_oracle exemption — got {out}"
        );
    }

    /// Spawning `tdd` with a nonexistent `current_dir` reliably fails the
    /// spawn regardless of whether a `tdd` binary happens to be on PATH in
    /// the test environment — this exercises the "tdd unreachable" path
    /// deterministically.
    ///
    /// A spawn failure is **cannot determine**, not **unavailable**: nothing
    /// looked at the proofs, so the gate must not be handed a `fallback:true`
    /// that `enforce_fp_gate` Allows. An environment with no `tdd` installed
    /// must not silently pass the F→P gate for every fix/feature task.
    #[test]
    fn spawn_failure_is_undetermined_and_rejects() {
        let bogus_dir = std::env::temp_dir().join("condukt-oracle-test-nonexistent-dir-zzz-987654");
        let _ = std::fs::remove_dir_all(&bogus_dir);
        assert!(!bogus_dir.exists());

        let out = check_oracle(true, Some("cargo test -p x"), "t1", &bogus_dir);
        assert_eq!(out["required"], true, "{out}");
        assert_eq!(
            out["fallback"], false,
            "a spawn failure is undetermined, not a could-not-generate fallback — got {out}"
        );
        assert_eq!(out["valid_fp_oracle"], false, "{out}");
        assert!(
            matches!(
                crate::state::enforce_fp_gate(&out),
                crate::state::FpGateDecision::Reject
            ),
            "an unspawnable tdd must Reject end-to-end, got {:?} for {out}",
            crate::state::enforce_fp_gate(&out)
        );
    }

    /// The spawn-failure `reason` is the only thing the operator sees. It must
    /// (a) name the undetermined condition — `tdd` could not be spawned — and
    /// (b) tell them to install/provide the `tdd` binary. Asserted on stable
    /// substrings, not the whole string.
    #[test]
    fn spawn_failure_reason_names_tdd_and_demands_it_be_installed() {
        let bogus_dir = std::env::temp_dir().join("condukt-oracle-test-nonexistent-dir-zzz-987655");
        let _ = std::fs::remove_dir_all(&bogus_dir);
        assert!(!bogus_dir.exists());

        let out = check_oracle(true, Some("cargo test -p x"), "t1", &bogus_dir);
        let reason = out["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("verdict has no string `reason`: {out}"))
            .to_ascii_lowercase();

        assert!(
            reason.contains("tdd"),
            "reason must name the `tdd` binary — got {reason:?}"
        );
        assert!(
            reason.contains("spawn"),
            "reason must say tdd could not be spawned — got {reason:?}"
        );
        assert!(
            reason.contains("could not be determined")
                || reason.contains("cannot be determined")
                || reason.contains("cannot determine")
                || reason.contains("undetermined"),
            "reason must say the oracle could not be DETERMINED (not that it is \
             merely unavailable) — got {reason:?}"
        );
        assert!(
            reason.contains("install")
                || reason.contains("available")
                || reason.contains("provide"),
            "reason must tell the operator to install/provide `tdd` — got {reason:?}"
        );
    }

    #[test]
    fn interpret_valid_fp_oracle_stdout() {
        let (valid, transition) =
            interpret_oracle_stdout(r#"{"valid_fp_oracle":true,"transition":"FailToPass"}"#);
        assert!(valid);
        assert_eq!(transition.as_deref(), Some("FailToPass"));
    }

    #[test]
    fn interpret_invalid_fp_oracle_stdout() {
        let (valid, transition) =
            interpret_oracle_stdout(r#"{"valid_fp_oracle":false,"transition":"fail_to_fail"}"#);
        assert!(!valid);
        assert_eq!(transition.as_deref(), Some("fail_to_fail"));
    }

    #[test]
    fn interpret_corrupt_stdout_defaults_to_invalid() {
        let (valid, transition) = interpret_oracle_stdout("not json");
        assert!(!valid);
        assert_eq!(transition, None);
    }

    // --- verdict_from_oracle: unknown (no proofs) → fallback, not reject ------

    /// THE FIX (b209f2d9): a `tdd oracle` verdict of `transition:"unknown"`
    /// (incomplete proofs = oracle could-not-be-generated) must degrade to the
    /// legacy gate (`fallback:true`), which `enforce_fp_gate` then Allows —
    /// rather than being treated as a definitive invalid-oracle reject.
    #[test]
    fn unknown_transition_degrades_to_fallback_not_reject() {
        let v = verdict_from_oracle(false, Some("unknown"));
        assert_eq!(v["required"], true);
        assert_eq!(v["valid_fp_oracle"], false);
        assert_eq!(
            v["fallback"], true,
            "no-proofs unknown must fall back, got: {v}"
        );
        // And the gate must Allow (degrade to done_criteria), never Reject.
        assert!(matches!(
            crate::state::enforce_fp_gate(&v),
            crate::state::FpGateDecision::Allow(None)
        ));
    }

    /// A *complete* proof pair that ran the wrong direction is a real,
    /// trustworthy verdict — it must stay non-fallback so the gate rejects it.
    #[test]
    fn wrong_direction_transition_still_rejects() {
        for name in ["fail_to_fail", "pass_to_pass", "pass_to_fail"] {
            let v = verdict_from_oracle(false, Some(name));
            assert_eq!(
                v["fallback"], false,
                "{name} is a real verdict, not fallback"
            );
            assert!(
                matches!(
                    crate::state::enforce_fp_gate(&v),
                    crate::state::FpGateDecision::Reject
                ),
                "{name} must still Reject"
            );
        }
    }

    /// A valid Fail→Pass verdict is non-fallback and Allows with the true flag.
    #[test]
    fn valid_fail_to_pass_allows_with_flag() {
        let v = verdict_from_oracle(true, Some("fail_to_pass"));
        assert_eq!(v["valid_fp_oracle"], true);
        assert_eq!(v["fallback"], false);
        assert!(matches!(
            crate::state::enforce_fp_gate(&v),
            crate::state::FpGateDecision::Allow(Some(true))
        ));
    }

    // --- exit status is part of the verdict: a crashed checker is not a pass ---
    //
    // Contract pinned below (do not weaken):
    //   `tdd oracle` exiting nonzero is CANNOT-DETERMINE, not "could not
    //   generate an oracle". It must NOT degrade to `fallback:true` (which
    //   `enforce_fp_gate` Allows). It must produce
    //   `required:true, fallback:false, valid_fp_oracle:false` so that
    //   `enforce_fp_gate` returns `Reject`.
    //
    // Assumed pure-function signature under test (implementation to follow):
    //   pub fn verdict_from_oracle_output(stdout: &str, exit_ok: bool) -> serde_json::Value

    /// Helper: assert a verdict is the "cannot determine → restrictive" shape
    /// AND that it actually reaches `FpGateDecision::Reject` end-to-end.
    fn assert_undetermined_rejects(v: &serde_json::Value, ctx: &str) {
        assert_eq!(
            v["required"], true,
            "{ctx}: required must stay true — got {v}"
        );
        assert_eq!(
            v["fallback"], false,
            "{ctx}: a nonzero-exit `tdd oracle` is undetermined, not fallback — got {v}"
        );
        assert_eq!(
            v["valid_fp_oracle"], false,
            "{ctx}: a crashed checker never proves a valid F→P oracle — got {v}"
        );
        assert!(
            matches!(
                crate::state::enforce_fp_gate(v),
                crate::state::FpGateDecision::Reject
            ),
            "{ctx}: must reach FpGateDecision::Reject, got {:?} for {v}",
            crate::state::enforce_fp_gate(v)
        );
    }

    /// Case 1: nonzero exit + empty stdout. Today this is indistinguishable
    /// from the "no stdout" fallback and Allows; it must Reject instead.
    #[test]
    fn nonzero_exit_with_empty_stdout_rejects() {
        assert_undetermined_rejects(
            &verdict_from_oracle_output("", false),
            "nonzero exit, empty stdout",
        );
        assert_undetermined_rejects(
            &verdict_from_oracle_output("   \n\t ", false),
            "nonzero exit, whitespace-only stdout",
        );
    }

    /// Case 2: nonzero exit + unparseable stdout. Must not degrade to the
    /// legacy gate.
    #[test]
    fn nonzero_exit_with_corrupt_stdout_rejects() {
        for s in [
            "not json",
            "{\"valid_fp_oracle\": tru",
            "<html>500</html>",
            "[1,2,3",
        ] {
            assert_undetermined_rejects(
                &verdict_from_oracle_output(s, false),
                &format!("nonzero exit, corrupt stdout {s:?}"),
            );
        }
    }

    /// Case 3 (the crux): nonzero exit + well-formed stdout that *claims* a
    /// valid Fail→Pass oracle. The self-report of a checker that crashed must
    /// not be trusted — "落ちたチェッカは合格したチェッカではない".
    #[test]
    fn nonzero_exit_does_not_trust_claimed_valid_oracle() {
        assert_undetermined_rejects(
            &verdict_from_oracle_output(
                r#"{"valid_fp_oracle":true,"transition":"fail_to_pass"}"#,
                false,
            ),
            "nonzero exit claiming valid fail_to_pass",
        );
        // …and the same for a claimed "unknown" (which on exit 0 is the
        // legitimate fallback path): a crash must not borrow that exemption.
        assert_undetermined_rejects(
            &verdict_from_oracle_output(
                r#"{"valid_fp_oracle":false,"transition":"unknown"}"#,
                false,
            ),
            "nonzero exit claiming unknown transition",
        );
        // A plain non-object JSON body on a crashed run is still undetermined.
        assert_undetermined_rejects(
            &verdict_from_oracle_output("null", false),
            "nonzero exit with JSON null stdout",
        );
    }

    // --- Case 4: non-regression — exit 0 behaviour is unchanged -------------

    /// exit 0 + empty/whitespace stdout keeps the existing "no stdout"
    /// fallback (Allow(None)).
    #[test]
    fn exit_ok_empty_stdout_still_falls_back() {
        for s in ["", "   \n"] {
            let v = verdict_from_oracle_output(s, true);
            assert_eq!(v["required"], true, "stdout {s:?} → {v}");
            assert_eq!(v["fallback"], true, "stdout {s:?} → {v}");
            assert_eq!(v["valid_fp_oracle"], false, "stdout {s:?} → {v}");
            assert!(matches!(
                crate::state::enforce_fp_gate(&v),
                crate::state::FpGateDecision::Allow(None)
            ));
        }
    }

    /// exit 0 + corrupt stdout keeps the existing parse-failure fallback.
    #[test]
    fn exit_ok_corrupt_stdout_still_falls_back() {
        let v = verdict_from_oracle_output("not json", true);
        assert_eq!(v["required"], true, "{v}");
        assert_eq!(v["fallback"], true, "{v}");
        assert_eq!(v["valid_fp_oracle"], false, "{v}");
        assert!(matches!(
            crate::state::enforce_fp_gate(&v),
            crate::state::FpGateDecision::Allow(None)
        ));
    }

    /// exit 0 + well-formed stdout still routes through `verdict_from_oracle`
    /// unchanged: valid F→P allows, wrong-direction rejects, unknown falls back.
    #[test]
    fn exit_ok_wellformed_stdout_matches_verdict_from_oracle() {
        let cases = [
            (
                r#"{"valid_fp_oracle":true,"transition":"fail_to_pass"}"#,
                true,
                Some("fail_to_pass"),
            ),
            (
                r#"{"valid_fp_oracle":false,"transition":"fail_to_fail"}"#,
                false,
                Some("fail_to_fail"),
            ),
            (
                r#"{"valid_fp_oracle":false,"transition":"pass_to_pass"}"#,
                false,
                Some("pass_to_pass"),
            ),
            (
                r#"{"valid_fp_oracle":false,"transition":"pass_to_fail"}"#,
                false,
                Some("pass_to_fail"),
            ),
            (
                r#"{"valid_fp_oracle":false,"transition":"unknown"}"#,
                false,
                Some("unknown"),
            ),
        ];
        for (stdout, valid, transition) in cases {
            let got = verdict_from_oracle_output(stdout, true);
            let want = verdict_from_oracle(valid, transition);
            assert_eq!(got, want, "exit-0 regression for stdout {stdout:?}");
            assert_eq!(
                crate::state::enforce_fp_gate(&got),
                crate::state::enforce_fp_gate(&want),
                "exit-0 gate decision regression for stdout {stdout:?}"
            );
        }
    }

    // --- CLI wiring: check_oracle must actually forward the exit status ------
    //
    // The pure-function tests above prove `verdict_from_oracle_output` treats a
    // non-zero exit as undetermined. They say nothing about whether
    // `check_oracle` *passes* the real exit status: a caller hardcoding `true`
    // would keep every unit test green while the live gate stayed fail-open.
    // These tests close that seam by spawning a fake `tdd` off a prepended PATH.

    /// Write an executable `tdd` into `dir` that prints `stdout` and exits with
    /// `code`. Returns nothing; the caller prepends `dir` to `PATH`.
    #[cfg(unix)]
    fn write_fake_tdd(dir: &Path, stdout: &str, code: i32) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!("#!/bin/sh\ncat <<'ORACLE_EOF'\n{stdout}\nORACLE_EOF\nexit {code}\n");
        let p = dir.join("tdd");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Run `check_oracle` with a fake `tdd` on PATH that exits `code` printing
    /// `stdout`. PATH is restored before returning.
    #[cfg(unix)]
    fn check_oracle_with_fake_tdd(stdout: &str, code: i32) -> serde_json::Value {
        let _guard = crate::env_lock::PATH_ENV_LOCK
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_fake_tdd(&bin, stdout, code);

        let old_path = std::env::var_os("PATH");
        let mut parts = vec![bin.clone()];
        if let Some(p) = &old_path {
            parts.extend(std::env::split_paths(p));
        }
        std::env::set_var("PATH", std::env::join_paths(parts).unwrap());

        let out = check_oracle(true, Some("cargo test -p x"), "t1", tmp.path());

        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        out
    }

    /// Sanity: the fake `tdd` really is what gets spawned. If PATH injection
    /// silently failed we would be measuring a spawn error / the real `tdd`
    /// instead of the exit-status wiring, and case A below would pass for the
    /// wrong reason.
    #[cfg(unix)]
    #[test]
    fn oracle_fake_tdd_is_actually_spawned() {
        let v = check_oracle_with_fake_tdd(
            r#"{"valid_fp_oracle":false,"transition":"pass_to_pass"}"#,
            0,
        );
        assert_eq!(
            v["transition"], "pass_to_pass",
            "fake tdd's stdout did not reach check_oracle — PATH injection is not working; got {v}"
        );
        assert_eq!(v["fallback"], false, "{v}");
    }

    /// CASE A (the wiring proof): a `tdd` that exits 1 while claiming a valid
    /// Fail→Pass oracle on stdout must still produce the undetermined verdict
    /// and Reject. Hardcoding `check_oracle`'s `exit_ok` argument to `true`
    /// makes this test fail — that is exactly what it is for.
    #[cfg(unix)]
    #[test]
    fn oracle_check_oracle_forwards_nonzero_exit_and_rejects() {
        let v = check_oracle_with_fake_tdd(
            r#"{"valid_fp_oracle":true,"transition":"fail_to_pass"}"#,
            1,
        );
        assert_eq!(v["required"], true, "{v}");
        assert_eq!(
            v["fallback"], false,
            "a crashed tdd must not degrade to the legacy gate — got {v}"
        );
        assert_eq!(
            v["valid_fp_oracle"], false,
            "check_oracle trusted a crashed checker's self-report — got {v}"
        );
        assert!(
            matches!(
                crate::state::enforce_fp_gate(&v),
                crate::state::FpGateDecision::Reject
            ),
            "must Reject end-to-end, got {:?} for {v}",
            crate::state::enforce_fp_gate(&v)
        );
    }

    /// Run `check_oracle` with a PATH that contains nothing at all (a single
    /// empty temp dir), so `tdd` is genuinely unreachable and the spawn fails.
    /// PATH is restored before returning. Returns the verdict plus the run dir's
    /// guard so it outlives the call.
    #[cfg(unix)]
    fn check_oracle_with_no_tdd_on_path() -> serde_json::Value {
        let _guard = crate::env_lock::PATH_ENV_LOCK
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        assert!(
            !empty_bin.join("tdd").exists(),
            "PATH dir must contain no tdd"
        );

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &empty_bin);

        let out = check_oracle(true, Some("cargo test -p x"), "t1", &run_dir);

        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        out
    }

    /// CASE C (the wiring proof for "no tdd installed"): the run dir is fine,
    /// the task is in scope, and the ONLY thing wrong is that no `tdd` exists
    /// anywhere on PATH. That is the exact shape of a machine that never
    /// installed `tdd` — and today it silently passes the F→P gate for every
    /// fix/feature task. It must Reject end-to-end, with a reason that names
    /// `tdd` and demands it be installed.
    #[cfg(unix)]
    #[test]
    fn oracle_no_tdd_anywhere_on_path_rejects_end_to_end() {
        let v = check_oracle_with_no_tdd_on_path();
        assert_eq!(v["required"], true, "{v}");
        assert_eq!(
            v["fallback"], false,
            "no `tdd` installed is cannot-determine, not could-not-generate — got {v}"
        );
        assert_eq!(v["valid_fp_oracle"], false, "{v}");
        assert!(
            matches!(
                crate::state::enforce_fp_gate(&v),
                crate::state::FpGateDecision::Reject
            ),
            "an environment with no tdd must not silently pass the F→P gate, got {:?} for {v}",
            crate::state::enforce_fp_gate(&v)
        );
        let reason = v["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("verdict has no string `reason`: {v}"))
            .to_ascii_lowercase();
        assert!(reason.contains("tdd"), "reason must name tdd — {reason:?}");
        assert!(
            reason.contains("install")
                || reason.contains("available")
                || reason.contains("provide"),
            "reason must tell the operator to install/provide tdd — {reason:?}"
        );
    }

    /// CASE B (non-regression): the same stdout on exit 0 still yields the
    /// trusted valid-oracle verdict and Allows.
    #[cfg(unix)]
    #[test]
    fn oracle_check_oracle_exit_zero_still_trusts_valid_verdict() {
        let v = check_oracle_with_fake_tdd(
            r#"{"valid_fp_oracle":true,"transition":"fail_to_pass"}"#,
            0,
        );
        assert_eq!(v["required"], true, "{v}");
        assert_eq!(v["fallback"], false, "{v}");
        assert_eq!(v["valid_fp_oracle"], true, "{v}");
        assert!(
            matches!(
                crate::state::enforce_fp_gate(&v),
                crate::state::FpGateDecision::Allow(Some(true))
            ),
            "exit-0 regression: {v}"
        );
    }

    // --- THE FIX under test (backlog 22b69f6a): the top-of-function
    // short-circuit `!requires_oracle || reproduction_tests.is_none()` must
    // narrow to `!requires_oracle` alone. A fix/feature task
    // (`requires_oracle: true`) that simply did not DECLARE
    // `reproduction_tests` must NOT be exempted from the F→P gate — it must
    // still consult `tdd`, exactly like a task that did declare
    // `reproduction_tests`.
    //
    // Deterministic, env-independent pivot: after the fix,
    // `check_oracle(true, None, <task>, <fresh dir with no recorded proofs>)`
    // returns `required == true` in BOTH possible worlds —
    //   (a) `tdd` is on PATH but finds no proofs for `<task>` in the dir →
    //       `verdict_from_oracle_output`'s "unknown transition" fallback,
    //       which is `required:true, fallback:true`, or
    //   (b) `tdd` is not on PATH / not spawnable → the spawn-`Err` branch,
    //       `required:true, fallback:false`.
    // Today's short-circuit returns `required:false` unconditionally in this
    // case, so this assertion is the RED→GREEN pivot for the bug fix and does
    // not depend on whether `tdd` happens to be installed in the test
    // environment.

    /// THE bug (backlog 22b69f6a): a fix/feature task with no declared
    /// `reproduction_tests` is currently exempted from the F→P gate. It must
    /// not be — `check_oracle` must still consult `tdd` and can only resolve
    /// to a non-required exemption via the `!requires_oracle` gate, never via
    /// missing `reproduction_tests` alone.
    #[test]
    fn check_oracle_fix_or_feature_without_reproduction_tests_is_not_exempted() {
        let tmp = tempfile::TempDir::new().unwrap();

        let out = check_oracle(true, None, "nonexistent-task-224466", tmp.path());
        assert_eq!(
            out["required"], true,
            "a fix/feature task with no declared reproduction_tests must still \
             consult tdd — it must not be exempted from the F\u{2192}P gate merely \
             for not declaring reproduction_tests — got {out}"
        );
    }

    /// Non-regression: a task that is NOT fix/feature (`requires_oracle:
    /// false`) must stay exempted. This guards against the implementer
    /// over-tightening the fix and deleting the legitimate `!requires_oracle`
    /// exemption along with the buggy `reproduction_tests.is_none()` one.
    #[test]
    fn check_oracle_non_fix_feature_task_is_still_exempted() {
        let tmp = tempfile::TempDir::new().unwrap();

        let out = check_oracle(false, None, "t1", tmp.path());
        assert_eq!(out["required"], false, "{out}");
        assert_eq!(out["fallback"], true, "{out}");
    }

    /// The exemption is governed by task `kind` (`requires_oracle`), not by
    /// whether `reproduction_tests` happens to be present. A non-fix/feature
    /// task stays exempt even when `reproduction_tests` IS declared.
    #[test]
    fn check_oracle_non_fix_feature_exempt_regardless_of_reproduction_tests() {
        let tmp = tempfile::TempDir::new().unwrap();

        let out = check_oracle(false, Some("cargo test"), "t1", tmp.path());
        assert_eq!(
            out["required"], false,
            "kind gates the exemption, not reproduction_tests — got {out}"
        );
    }

    /// The old exempt shape's `reason` string
    /// ("not a fix/feature task or no reproduction_tests") conflated the two
    /// gates. Once the fix lands, a fix/feature task with no
    /// reproduction_tests must no longer produce that exact reason, since it
    /// is no longer exempt on that basis.
    #[test]
    fn check_oracle_fix_feature_no_reproduction_tests_reason_is_not_old_exempt_shape() {
        let tmp = tempfile::TempDir::new().unwrap();

        let out = check_oracle(true, None, "nonexistent-task-224469", tmp.path());
        let reason = out["reason"].as_str().unwrap_or("").to_string();
        assert_ne!(
            reason, "not a fix/feature task or no reproduction_tests",
            "reason must no longer claim a fix/feature task is exempt for lacking \
             reproduction_tests — got {out}"
        );
    }
}
