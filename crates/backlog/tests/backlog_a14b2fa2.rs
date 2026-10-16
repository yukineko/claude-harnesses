//! backlog a14b2fa2: `backlog done --test CMD --red-rev R` runs CMD (at R and
//! at HEAD) with the CALLER's environment, but the closure record stores only
//! `cmd`. A closure whose RED depended on an env var (9b64f427 needed
//! `AUDIT_RUN_OPEN=1` to unskip at the red rev) is therefore not reproducible
//! from the record: re-running the stored command without that env skips at
//! the red rev.
//!
//! Property under test (either resolution satisfies it): a closure that
//! `done` ACCEPTS must be reproducible from the record — so either the record
//! names the env the RED depended on, or `done` refuses the env-dependent
//! close.
mod common;
use common::*;

const CMD: &str = "bash tests/check.sh";

/// rev A has the bug; rev B fixes it and adds a test that SKIPS (exit 0,
/// nothing checked) unless `AUDIT_RUN_OPEN` is set.
fn env_gated_bug_then_fix(f: &Fixture) -> (String, String) {
    f.write("src/impl.txt", "broken\n");
    let a = f.commit_paths(&["src/impl.txt"], "buggy impl");
    f.write("src/impl.txt", "fixed\n");
    f.write_script(
        "tests/check.sh",
        "if [ -z \"$AUDIT_RUN_OPEN\" ]; then echo '1 skipped'; exit 0; fi\n\
         grep -q '^fixed$' src/impl.txt || { echo 'FAIL: impl not fixed'; exit 1; }\n\
         echo '1 passed'",
    );
    let b = f.commit_paths(&["src/impl.txt", "tests/check.sh"], "fix + env-gated test");
    (a, b)
}

/// Control: without the env var the red rev does not go RED, so `done`
/// refuses — i.e. the stored command alone does NOT reproduce the RED.
#[test]
fn control_without_env_the_red_rev_is_not_red() {
    let f = Fixture::new("a14b-ctl");
    let (red, _head) = env_gated_bug_then_fix(&f);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--test", CMD, "--red-rev", &red]);
    assert_ne!(
        o.code,
        0,
        "control: without AUDIT_RUN_OPEN the red rev skips, so done must refuse: {}",
        o.both()
    );
    assert_eq!(f.status(&id), "pending");
}

#[test]
#[ignore = "backlog a14b2fa2: RED repro"]
fn accepted_env_dependent_closure_records_the_env_or_is_refused() {
    let f = Fixture::new("a14b");
    let (red, _head) = env_gated_bug_then_fix(&f);
    let id = f.add("t");
    let o = f.run_env(
        &["done", &id, "--test", CMD, "--red-rev", &red],
        &[("AUDIT_RUN_OPEN", "1")],
    );
    if o.code != 0 {
        // Refusing the env-dependent close is an acceptable resolution.
        assert_eq!(f.status(&id), "pending", "refused close must not mutate");
        return;
    }
    assert_eq!(f.status(&id), "done");
    let closure = f.row(&id)["closure"].to_string();
    assert!(
        closure.contains("AUDIT_RUN_OPEN"),
        "done ACCEPTED a closure whose RED only appears with AUDIT_RUN_OPEN=1 in the \
         caller env, but the closure record does not name that env, so re-running the \
         stored command reproduces a skip, not the RED: {closure}"
    );
}
