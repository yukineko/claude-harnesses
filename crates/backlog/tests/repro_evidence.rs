//! Findings side (user rulings 2026-10-04 / 2026-10-15): only a REPRODUCED
//! repro outcome is filed (as `pending`); every other outcome files NO row,
//! exits 2 and writes nothing anywhere. Polarity: a repro script that FAILS
//! (exit 1) reproduces the bug (same as a RED test); exit 0 = not-reproduced;
//! anything that cannot be run under the runner allowlist/timeout =
//! undetermined.
mod common;
use common::*;

/// The outcome label is recorded somewhere on the row as a quoted JSON string.
fn has_outcome(row: &serde_json::Value, outcome: &str) -> bool {
    row.to_string().contains(&format!("\"{outcome}\""))
}

#[test]
fn reproduced_lands_pending_and_records_outcome() {
    let f = Fixture::new("repro");
    let (o, id) = f.add_extra("real bug", &["--repro-test", REPRO_YES]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.unwrap();
    assert_eq!(f.status(&id), "pending");
    assert!(has_outcome(&f.row(&id), "reproduced"), "{}", f.row(&id));
}

/// Every undetermined runner case (not allowlisted, untracked, missing,
/// shell composition, timeout) files NO row, exits 2 and writes no
/// `.backlog/unproven.toml` — never pending, never recorded.
#[test]
fn undetermined_repro_files_no_row_and_writes_nothing() {
    let f = Fixture::new("undet");
    f.write_script("tests/slow.sh", "sleep 30\nexit 1");
    f.commit_paths(&["tests/slow.sh"], "slow repro");
    // The untracked script dirties the tree, which on its own makes every
    // later run undetermined; it is therefore created only for the LAST case,
    // so each earlier case is undetermined for its own reason.
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("grep -q x README.txt", vec![]),
        ("bash tests/does_not_exist.sh", vec![]),
        ("bash tests/repro_yes.sh; true", vec![]),
        (
            "bash tests/slow.sh",
            vec![("BACKLOG_TEST_TIMEOUT_SECS", "1")],
        ),
        ("bash tests/untracked_repro.sh", vec![]),
    ];
    for (cmd, env) in cases {
        if cmd == "bash tests/untracked_repro.sh" {
            f.write_script("tests/untracked_repro.sh", "exit 1");
        }
        let p = f.project();
        let o = f.run_env(
            &["add", "--title", cmd, "--project", &p, "--repro-test", cmd],
            &env,
        );
        assert_eq!(o.code, 2, "{cmd:?} is not filed (exit 2): {}", o.both());
        assert!(
            !o.stdout.lines().any(|l| l.starts_with("added: ")),
            "{cmd:?} must not be filed: {}",
            o.stdout
        );
        assert!(
            f.list_all().is_empty(),
            "{cmd:?} is Undetermined, never a row: {:?}",
            f.list_all()
        );
        assert!(
            !f.tasks_path().with_file_name("unproven.toml").exists(),
            "{cmd:?}: nothing may be recorded for an unfiled finding"
        );
    }
}
