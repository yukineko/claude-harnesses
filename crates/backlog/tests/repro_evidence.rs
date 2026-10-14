//! Findings side. Since the intake contract v2 (user rulings 2026-10-04) only a
//! REPRODUCED outcome is filed (as `pending`); every other outcome files NO
//! row, exits 2 and is kept in `.backlog/unproven.toml` (pinned in
//! `intake_requires_repro.rs`). Legacy `unconfirmed` rows still exist in old
//! stores, so they are PLANTED directly here: they stay excluded from
//! next/claim and the pending count, and `confirm ID --repro-test CMD`
//! promotes only on reproduced. Polarity: a repro script that FAILS (exit 1)
//! reproduces the bug (same as a RED test); exit 0 = not-reproduced; anything
//! that cannot be run under the runner allowlist/timeout = undetermined.
mod common;
use common::*;

/// The outcome label is recorded somewhere on the row as a quoted JSON string.
fn has_outcome(row: &serde_json::Value, outcome: &str) -> bool {
    row.to_string().contains(&format!("\"{outcome}\""))
}

fn unproven_records(f: &Fixture) -> Vec<toml::Value> {
    let p = f.tasks_path().with_file_name("unproven.toml");
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    let v: toml::Value =
        toml::from_str(&s).unwrap_or_else(|e| panic!("unproven.toml unparseable ({e}):\n{s}"));
    v.get("record")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
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
/// shell composition, timeout) files NO row, exits 2 and is recorded as an
/// `undetermined` unproven record — never pending.
#[test]
fn undetermined_repro_files_no_row_and_is_recorded_undetermined() {
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
        let recs = unproven_records(&f);
        let rec = recs
            .iter()
            .find(|r| r.get("repro_test").and_then(|v| v.as_str()) == Some(cmd))
            .unwrap_or_else(|| panic!("{cmd:?} not kept in unproven.toml: {recs:?}"));
        assert_eq!(
            rec.get("outcome").and_then(|v| v.as_str()),
            Some("undetermined"),
            "{cmd:?}: {rec:?}"
        );
    }
}

#[test]
fn unconfirmed_is_excluded_from_next_claim_and_pending_list() {
    let f = Fixture::new("excl");
    let (pending, label) = f.add_with_label("verified");
    let unconf = "0e0c0f01".to_string();
    f.plant_open(&unconf, "unverified", "unconfirmed", &label);
    assert_eq!(f.status(&unconf), "unconfirmed", "planted row is visible");
    let p = f.project();
    // Checked BEFORE the claim below: once `next --claim` leases `pending`,
    // `list` derives it as `claimed` (backlog f09db5ce), so a `--status
    // pending` listing after the claim would drop the positive control for a
    // reason unrelated to unconfirmed exclusion.
    let l = f.run(&["list", "--status", "pending", "--project", &p]);
    assert!(
        !l.stdout.contains(&unconf) && l.stdout.contains(&pending),
        "{}",
        l.stdout
    );
    for args in [
        vec!["next", "--project", &p],
        vec!["next", "--claim", "--project", &p],
    ] {
        let o = f.run(&args);
        assert!(
            !o.stdout.contains(&unconf),
            "{args:?} must not return an unconfirmed row: {}",
            o.stdout
        );
        assert!(
            o.stdout.contains(&pending),
            "{args:?} positive control: {}",
            o.both()
        );
    }
}

#[test]
fn list_shows_unconfirmed_separately_with_observed_vs_suspicion_labels() {
    let f = Fixture::new("labels");
    let (observed, label) = f.add_with_label("verified one");
    let unconf = "0e0c0f02".to_string();
    f.plant_open(&unconf, "unverified one", "unconfirmed", &label);
    let l = f.run(&["list", "--all"]);
    assert_eq!(l.code, 0, "{}", l.both());
    let line_of = |id: &str| {
        l.stdout
            .lines()
            .find(|x| x.contains(id))
            .unwrap_or("")
            .to_lowercase()
    };
    assert!(
        line_of(&unconf).contains("suspicion"),
        "unconfirmed row labelled suspicion: {}",
        l.stdout
    );
    assert!(
        line_of(&observed).contains("observed"),
        "reproduced row labelled observed: {}",
        l.stdout
    );
    assert!(
        l.stdout.contains("unconfirmed"),
        "own section/count for unconfirmed: {}",
        l.stdout
    );
}

#[test]
fn confirm_promotes_to_pending_only_on_reproduced() {
    let f = Fixture::new("confirm");
    let (_, label) = f.add_with_label("reference row for the project label");
    let id = "0e0c0f03".to_string();
    f.plant_open(&id, "finding", "unconfirmed", &label);
    let o = f.run(&["confirm", &id, "--repro-test", REPRO_NO]);
    assert_eq!(
        f.status(&id),
        "unconfirmed",
        "not-reproduced leaves it unconfirmed: {}",
        o.both()
    );
    assert!(
        has_outcome(&f.row(&id), "not-reproduced"),
        "the attempt is recorded: {}",
        f.row(&id)
    );
    let o = f.run(&["confirm", &id, "--repro-test", "grep -q x README.txt"]);
    assert_eq!(
        f.status(&id),
        "unconfirmed",
        "undetermined leaves it unconfirmed: {}",
        o.both()
    );
    let o = f.run(&["confirm", &id, "--repro-test", REPRO_YES]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending");
    assert!(has_outcome(&f.row(&id), "reproduced"));
}

#[test]
fn confirm_without_a_repro_test_is_refused() {
    let f = Fixture::new("confnotest");
    let (_, label) = f.add_with_label("reference row for the project label");
    let id = "0e0c0f04".to_string();
    f.plant_open(&id, "finding", "unconfirmed", &label);
    let o = f.run(&["confirm", &id]);
    assert_refused_unchanged(&f, &id, "unconfirmed", &o, "confirm needs --repro-test");
}
