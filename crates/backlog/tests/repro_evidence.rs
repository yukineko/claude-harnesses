//! Findings side: `add` never loses a finding, but only a REPRODUCED outcome
//! lands `pending`; anything else lands `unconfirmed` (excluded from next/claim
//! and from the pending count). `confirm ID --repro-test CMD` promotes only on
//! reproduced. Polarity assumed: a repro script that FAILS (exit 1) reproduces
//! the bug (same as a RED test); exit 0 = not-reproduced; anything that cannot
//! be run under the runner allowlist/timeout = undetermined.
mod common;
use common::*;

/// The outcome label is recorded somewhere on the row as a quoted JSON string.
fn has_outcome(row: &serde_json::Value, outcome: &str) -> bool {
    row.to_string().contains(&format!("\"{outcome}\""))
}

#[test]
fn add_without_repro_test_lands_unconfirmed_but_succeeds() {
    let f = Fixture::new("norepro");
    let (o, id) = f.add_extra("a finding", &[]);
    assert_eq!(o.code, 0, "add always succeeds: {}", o.both());
    let id = id.expect("added: <id>");
    assert_eq!(f.status(&id), "unconfirmed");
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

#[test]
fn not_reproduced_lands_unconfirmed_and_is_never_lost() {
    let f = Fixture::new("notrepro");
    let (o, id) = f.add_extra("phantom", &["--repro-test", REPRO_NO]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.unwrap();
    assert_eq!(f.status(&id), "unconfirmed");
    assert!(has_outcome(&f.row(&id), "not-reproduced"), "{}", f.row(&id));
}

#[test]
fn undetermined_repro_lands_unconfirmed_never_pending() {
    let f = Fixture::new("undet");
    f.write_script("tests/slow.sh", "sleep 30\nexit 1");
    f.commit_paths(&["tests/slow.sh"], "slow repro");
    f.write_script("tests/untracked_repro.sh", "exit 1");
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("grep -q x README.txt", vec![]),
        ("bash tests/untracked_repro.sh", vec![]),
        ("bash tests/does_not_exist.sh", vec![]),
        ("bash tests/repro_yes.sh; true", vec![]),
        (
            "bash tests/slow.sh",
            vec![("BACKLOG_TEST_TIMEOUT_SECS", "1")],
        ),
    ];
    for (cmd, env) in cases {
        let p = f.project();
        let o = f.run_env(
            &["add", "--title", cmd, "--project", &p, "--repro-test", cmd],
            &env,
        );
        assert_eq!(o.code, 0, "add must succeed for {cmd:?}: {}", o.both());
        let id = parse_added(&o.stdout);
        assert_eq!(
            f.status(&id),
            "unconfirmed",
            "{cmd:?} is Undetermined, never pending"
        );
        assert!(
            has_outcome(&f.row(&id), "undetermined"),
            "{cmd:?}: {}",
            f.row(&id)
        );
    }
}

#[test]
fn unconfirmed_is_excluded_from_next_claim_and_pending_list() {
    let f = Fixture::new("excl");
    let (_, unconf) = f.add_extra("unverified", &[]);
    let unconf = unconf.unwrap();
    let pending = f.add("verified");
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
    let (_, unconf) = f.add_extra("unverified one", &[]);
    let unconf = unconf.unwrap();
    let observed = f.add("verified one");
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
    let (_, id) = f.add_extra("finding", &[]);
    let id = id.unwrap();
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
    let (_, id) = f.add_extra("finding", &[]);
    let id = id.unwrap();
    let o = f.run(&["confirm", &id]);
    assert_refused_unchanged(&f, &id, "unconfirmed", &o, "confirm needs --repro-test");
}
