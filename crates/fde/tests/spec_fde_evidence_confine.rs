// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spec `fde-evidence-confine` (specs/fde-evidence-confine.toml), requirements
//! R2-R8 that are observable through the real `fde` binary. R1 is covered by
//! `backlog_scout_20261112_s2_evidence_cmd_escapes_root.rs`.
//!
//! Every CMD reports what it saw by writing into an in-root file (the CMD's
//! stdout is not surfaced by fde). Tests marked RED fail at HEAD (no
//! confinement, no private TMPDIR); the rest are controls that pass at HEAD.
//!
//! Not testable through the binary (see the report): R3 refusal paths, R5
//! "ledger unchanged after refusal", R7 removal-failure warning, R8 cfg(test)
//! override placement, R9 docs.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{code, run_fde_with_env, show, Engagement, TempPlace};

fn red(e: &Engagement, cmd: &str, env: &[(&str, &Path)]) -> Output {
    run_fde_with_env(
        e.root(),
        &e.state.path,
        &["evidence", "red", "--req", "R1", "--cmd", cmd],
        env,
    )
}

/// Every `*.json` file under the state dir that parses to an object with a
/// `records` array: the ledger(s).
fn ledgers(e: &Engagement) -> Vec<serde_json::Value> {
    fn walk(d: &Path, out: &mut Vec<serde_json::Value>) {
        for ent in fs::read_dir(d).unwrap() {
            let p = ent.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(s) = fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if v.get("records").and_then(|r| r.as_array()).is_some() {
                        out.push(v);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&e.state.path, &mut out);
    out
}

fn records(e: &Engagement) -> Vec<serde_json::Value> {
    ledgers(e)
        .iter()
        .flat_map(|l| l["records"].as_array().unwrap().clone())
        .collect()
}

/// R2 control: reads outside the root and the allow-set are not restricted.
#[test]
fn r2_cmd_can_read_outside_root() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let other = TempPlace::new("fde-readable-");
    let src = other.path.join("secret.txt");
    fs::write(&src, "outside-content").unwrap();
    let seen = e.root().join("seen");
    let out = red(
        &e,
        &format!("cat {} > {} && false", src.display(), seen.display()),
        &[],
    );
    assert_eq!(
        code(&out),
        Some(0),
        "red with a failing CMD must record: {}",
        show(&out)
    );
    assert_eq!(
        fs::read_to_string(&seen).unwrap_or_default(),
        "outside-content",
        "CMD must read a file outside the root: {}",
        show(&out)
    );
}

/// R3 (observable half): the CMD sees a private per-invocation TMPDIR, not
/// the parent's.
#[test]
#[ignore = "RED: spec fde-evidence-confine R3 — remove the ignore when fixed"]
fn r3_cmd_tmpdir_is_not_parent_tmpdir() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let parent = TempPlace::new("fde-parent-tmp-");
    let seen = e.root().join("seen-tmpdir");
    let out = red(
        &e,
        &format!("printf %s \"$TMPDIR\" > {} && false", seen.display()),
        &[("TMPDIR", &parent.path)],
    );
    let got = fs::read_to_string(&seen).unwrap_or_default();
    assert!(
        !got.is_empty(),
        "CMD must have run and reported TMPDIR: {}",
        show(&out)
    );
    let got = PathBuf::from(got);
    assert_ne!(
        got.canonicalize().unwrap_or(got.clone()),
        parent.path,
        "CMD saw the parent TMPDIR, not a private per-invocation dir: {}",
        show(&out)
    );
}

/// R4 control: in-root write and a write into the CMD's own $TMPDIR succeed.
#[test]
fn r4_cmd_can_write_root_and_its_tmpdir() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let out = red(
        &e,
        "touch \"$TMPDIR/x\" && test -f \"$TMPDIR/x\" && mktemp >/dev/null && touch in-root && false",
        &[],
    );
    assert_eq!(code(&out), Some(0), "{}", show(&out));
    assert!(
        e.root().join("in-root").exists(),
        "CMD must reach its end (TMPDIR write/mktemp + in-root write): {}",
        show(&out)
    );
}

/// R4: $TMPDIR inside the CMD is a directory that exists while it runs and is
/// not the parent's.
#[test]
#[ignore = "RED: spec fde-evidence-confine R4 — remove the ignore when fixed"]
fn r4_private_tmpdir_exists_during_cmd_and_differs_from_parent() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let parent = TempPlace::new("fde-parent-tmp-");
    let out = red(
        &e,
        "test -d \"$TMPDIR\" && printf %s \"$TMPDIR\" > tmpdir-seen && false",
        &[("TMPDIR", &parent.path)],
    );
    let seen = fs::read_to_string(e.root().join("tmpdir-seen")).unwrap_or_default();
    assert!(
        !seen.is_empty(),
        "$TMPDIR must be an existing dir while the CMD runs: {}",
        show(&out)
    );
    assert_ne!(
        PathBuf::from(&seen)
            .canonicalize()
            .unwrap_or(PathBuf::from(&seen)),
        parent.path,
        "$TMPDIR in the CMD must differ from the parent's: {}",
        show(&out)
    );
}

/// R5 control: a successful record gains no confinement key and SCHEMA stays 2.
#[test]
fn r5_record_has_no_confinement_key_and_schema_is_2() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let out = red(&e, "false", &[]);
    assert_eq!(code(&out), Some(0), "{}", show(&out));
    let ls = ledgers(&e);
    assert_eq!(ls.len(), 1, "exactly one ledger expected: {}", show(&out));
    assert_eq!(ls[0]["schema"], 2, "SCHEMA must stay 2");
    let recs = records(&e);
    assert_eq!(recs.len(), 1, "one record expected");
    let keys: Vec<&String> = recs[0].as_object().unwrap().keys().collect();
    for k in &keys {
        assert!(
            !k.contains("sandbox") && !k.contains("confine"),
            "record gained a confinement key {k:?}: {keys:?}"
        );
    }
}

/// R6: a write denied by the confinement is just a non-zero exit, so
/// `evidence red` records RED with a non-zero exit_code.
#[test]
#[ignore = "RED: spec fde-evidence-confine R6 — remove the ignore when fixed"]
fn r6_denied_outside_write_records_red_nonzero_exit() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let home = TempPlace::new("fde-home-");
    let outside = home.path.join("escaped-marker");
    let out = red(&e, &format!("touch {}", outside.display()), &[]);
    assert_eq!(
        code(&out),
        Some(0),
        "red with a denied (non-zero) write must exit 0: {}",
        show(&out)
    );
    assert!(!outside.exists(), "outside file must not exist");
    let recs = records(&e);
    assert_eq!(recs.len(), 1, "one RED record expected: {}", show(&out));
    assert_ne!(
        recs[0]["exit_code"].as_i64(),
        Some(0),
        "exit_code must be non-zero: {}",
        recs[0]
    );
}

/// R7: the private TMPDIR is gone once `fde evidence red` returns.
#[test]
#[ignore = "RED: spec fde-evidence-confine R7 — remove the ignore when fixed"]
fn r7_private_tmpdir_removed_after_cmd() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let parent = TempPlace::new("fde-parent-tmp-");
    let out = red(
        &e,
        "printf %s \"$TMPDIR\" > tmpdir-seen && false",
        &[("TMPDIR", &parent.path)],
    );
    let seen = PathBuf::from(fs::read_to_string(e.root().join("tmpdir-seen")).unwrap_or_default());
    assert!(
        seen.is_absolute(),
        "CMD must have reported its TMPDIR: {}",
        show(&out)
    );
    assert_ne!(
        seen.canonicalize().unwrap_or(seen.clone()),
        parent.path,
        "TMPDIR seen was the parent's, not a private dir: {}",
        show(&out)
    );
    assert!(
        !seen.exists(),
        "private TMPDIR must be removed after the CMD: {} still exists",
        seen.display()
    );
}

/// R8: no env var can switch confinement off in the real binary. The names
/// below are plausible guesses; the property is that an outside write is
/// still denied with them all set.
#[test]
#[ignore = "RED: spec fde-evidence-confine R8 — remove the ignore when fixed"]
fn r8_plausible_env_overrides_do_not_disable_confinement() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let home = TempPlace::new("fde-home-");
    let outside = home.path.join("escaped-marker");
    let missing = PathBuf::from("/nonexistent/sandbox-exec");
    let allow_all = e.spec_dir.path.join("allow-all.sb");
    fs::write(&allow_all, "(version 1)(allow default)").unwrap();
    let out = red(
        &e,
        &format!("touch {}", outside.display()),
        &[
            ("FDE_SANDBOX_EXEC", &missing),
            ("FDE_SANDBOX_EXEC_PATH", &missing),
            ("FDE_SANDBOX_PROFILE", &allow_all),
            ("FDE_NO_SANDBOX", &allow_all),
            ("FDE_SANDBOX", &allow_all),
            ("FDE_UNCONFINED", &allow_all),
        ],
    );
    assert!(
        !outside.exists(),
        "confinement was disabled by an env override: {}",
        show(&out)
    );
}
