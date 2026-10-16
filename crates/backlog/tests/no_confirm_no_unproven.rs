//! User rulings 2026-10-15: `confirm` and `unproven` are removed; `add` without
//! a reproduced repro writes NOTHING; `next` never hands out a row lacking a
//! `reproduced` repro (fail closed).
//! Written by an independent test writer (CLAUDE.md §2(a)).
mod common;
use common::*;

fn append(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cur = std::fs::read_to_string(path).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(body);
    std::fs::write(path, cur).unwrap();
}

fn row(id: &str, status: &str, project: &str, repro: Option<&str>) -> String {
    let r = repro
        .map(|o| format!("[task.repro]\noutcome = \"{o}\"\ncmd = \"bash tests/repro_yes.sh\"\nobserved_at = 1\ndetail = \"planted\"\n"))
        .unwrap_or_default();
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"row {id}\"\nproject = \"{project}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n{r}\n"
    )
}

// ---- i. confirm is gone ----

#[test]
fn confirm_is_rejected_and_row_byte_unchanged() {
    let f = Fixture::new("nc-confirm");
    append(
        &f.tasks_path(),
        &row("legacy01", "pending", &f.project(), None),
    );
    let before = f.store_bytes();
    let o = f.run(&["confirm", "legacy01", "--repro-test", REPRO_YES]);
    assert_ne!(o.code, 0, "confirm must not exist: {}", o.both());
    assert_eq!(
        f.store_bytes(),
        before,
        "store must be byte-unchanged: {}",
        o.both()
    );
}

// ---- j. next never hands out a row without a reproduced repro ----

fn only_unreproduced(tag: &str, repro: Option<&str>, claim: bool) {
    let f = Fixture::new(tag);
    append(
        &f.tasks_path(),
        &row("norepro1", "pending", &f.project(), repro),
    );
    let args: Vec<&str> = if claim {
        vec!["next", "--claim"]
    } else {
        vec!["next"]
    };
    let o = f.run(&args);
    assert!(
        !o.stdout.contains("norepro1"),
        "{args:?} handed out a row with repro {repro:?}: {}",
        o.both()
    );
}

#[test]
fn next_skips_pending_row_without_repro() {
    only_unreproduced("nc-next-none", None, false);
}
#[test]
fn next_claim_skips_pending_row_without_repro() {
    only_unreproduced("nc-claim-none", None, true);
}
#[test]
fn next_skips_not_reproduced_row() {
    only_unreproduced("nc-next-nr", Some("not-reproduced"), false);
}
#[test]
fn next_skips_undetermined_row() {
    only_unreproduced("nc-next-ud", Some("undetermined"), false);
}
#[test]
fn next_skips_failed_row_without_repro() {
    let f = Fixture::new("nc-next-failed");
    append(
        &f.tasks_path(),
        &row("norepro1", "failed", &f.project(), None),
    );
    let o = f.run(&["next"]);
    assert!(!o.stdout.contains("norepro1"), "{}", o.both());
}

#[test]
fn next_exit_shape_for_only_unreproduced_row_matches_empty_queue() {
    let empty = Fixture::new("nc-shape-empty");
    let e = empty.run(&["next"]);
    let f = Fixture::new("nc-shape-row");
    append(
        &f.tasks_path(),
        &row("norepro1", "pending", &f.project(), None),
    );
    let o = f.run(&["next"]);
    assert_eq!(
        o.code,
        e.code,
        "exit code must match the empty-queue shape: empty={} row={}",
        e.both(),
        o.both()
    );
}

#[test]
fn next_hands_out_the_reproduced_row_over_a_repro_less_one() {
    let f = Fixture::new("nc-next-mixed");
    append(
        &f.tasks_path(),
        &row("norepro1", "pending", &f.project(), None),
    );
    append(
        &f.tasks_path(),
        &row("repro001", "pending", &f.project(), Some("reproduced")),
    );
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("repro001"),
        "must hand out the reproduced row: {}",
        o.both()
    );
    assert!(!o.stdout.contains("norepro1"), "{}", o.both());
}

// ---- k. add without a reproduced repro writes NOTHING ----

fn assert_add_writes_nothing(tag: &str, extra: &[&str], pre_unproven: Option<&str>) {
    let f = Fixture::new(tag);
    let up = f.repo.join(".backlog").join("unproven.toml");
    if let Some(body) = pre_unproven {
        append(&up, body);
    }
    let tasks_before = std::fs::read(f.tasks_path()).ok();
    let up_before = std::fs::read(&up).ok();
    let p = f.project();
    let mut a = vec!["add", "--title", "t", "--project", &p];
    a.extend_from_slice(extra);
    let o = f.run(&a);
    assert_ne!(o.code, 0, "add must be refused: {}", o.both());
    assert_eq!(
        std::fs::read(f.tasks_path()).ok(),
        tasks_before,
        "tasks.toml changed: {}",
        o.both()
    );
    assert_eq!(
        std::fs::read(&up).ok(),
        up_before,
        "unproven.toml written/changed: {}",
        o.both()
    );
    let b = o.both().to_lowercase();
    assert!(
        !b.contains("recorded") && !b.contains("unproven.toml"),
        "stderr claims the finding was recorded: {}",
        o.both()
    );
}

#[test]
fn add_without_repro_writes_nothing_anywhere() {
    assert_add_writes_nothing("nc-add-none", &[], None);
}
#[test]
fn add_with_non_reproducing_repro_writes_nothing_anywhere() {
    assert_add_writes_nothing("nc-add-no", &["--repro-test", REPRO_NO], None);
}
#[test]
fn add_leaves_a_preexisting_unproven_file_byte_unchanged() {
    assert_add_writes_nothing("nc-add-pre", &[], Some("[[record]]\ntitle = \"old\"\n"));
}

// ---- l / m. unproven and confirm are gone from the CLI ----

#[test]
fn unproven_list_and_search_are_rejected() {
    let f = Fixture::new("nc-unproven");
    for args in [vec!["unproven", "list"], vec!["unproven", "search", "x"]] {
        let o = f.run(&args);
        assert_ne!(o.code, 0, "{args:?} must not exist: {}", o.both());
    }
}

#[test]
fn help_lists_neither_confirm_nor_unproven() {
    let f = Fixture::new("nc-help");
    let o = f.run(&["--help"]);
    let cmds: Vec<&str> = o
        .stdout
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(
        !cmds.contains(&"confirm"),
        "--help still lists confirm: {}",
        o.stdout
    );
    assert!(
        !cmds.contains(&"unproven"),
        "--help still lists unproven: {}",
        o.stdout
    );
}
