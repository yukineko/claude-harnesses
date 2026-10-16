//! Scope extension of the 2026-10-15 ruling: the `unconfirmed` status is
//! removed too. A stray live `status = "unconfirmed"` row is FAIL-CLOSED;
//! intake never files a row without a reproduced repro; historical rows that
//! were unconfirmed and then discard-cancelled still LOAD.
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

fn raw_row(id: &str, status: &str, project: &str, extra: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"row {id}\"\nproject = \"{project}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n{extra}\n"
    )
}

fn assert_named_refusal(o: &Out, id: &str, word: &str, what: &str) {
    assert_ne!(o.code, 0, "{what}: must exit non-zero: {}", o.both());
    let b = o.both();
    assert!(b.contains(id), "{what}: must name the row id {id}: {b}");
    assert!(b.contains(word), "{what}: must name `{word}`: {b}");
}

const UNCONF_REPRO: &str = "[task.repro]\noutcome = \"not-reproduced\"\ncmd = \"bash tests/repro_no.sh\"\nobserved_at = 1\ndetail = \"planted\"\n";

// ---- (e) live unconfirmed row is fail-closed ----

#[test]
fn list_fails_closed_on_an_unconfirmed_row_in_tasks_toml() {
    let f = Fixture::new("uc-list");
    let _ok = f.add("normal pending");
    append(
        &f.tasks_path(),
        &raw_row("ucrow001", "unconfirmed", &f.project(), UNCONF_REPRO),
    );
    let o = f.run(&["list"]);
    assert_named_refusal(&o, "ucrow001", "unconfirmed", "list");
}

#[test]
fn next_fails_closed_on_an_unconfirmed_row_in_tasks_toml() {
    let f = Fixture::new("uc-next");
    let _ok = f.add("normal pending");
    append(
        &f.tasks_path(),
        &raw_row("ucrow001", "unconfirmed", &f.project(), UNCONF_REPRO),
    );
    let o = f.run(&["next"]);
    assert_named_refusal(&o, "ucrow001", "unconfirmed", "next");
    assert!(
        !o.stdout.contains("ucrow001"),
        "next must not hand out the unconfirmed row: {}",
        o.both()
    );
}

#[test]
fn unconfirmed_row_only_in_done_file_without_twin_fails_closed() {
    let f = Fixture::new("uc-done");
    let _ok = f.add("normal pending");
    append(
        &f.done_path(),
        &raw_row("ucrow002", "unconfirmed", &f.project(), UNCONF_REPRO),
    );
    let o = f.run(&["list"]);
    assert_named_refusal(&o, "ucrow002", "unconfirmed", "list (done-file row)");
}

#[test]
fn next_fails_closed_on_unconfirmed_row_only_in_done_file() {
    let f = Fixture::new("uc-done-next");
    let _ok = f.add("normal pending");
    append(
        &f.done_path(),
        &raw_row("ucrow002", "unconfirmed", &f.project(), UNCONF_REPRO),
    );
    let o = f.run(&["next"]);
    assert_named_refusal(&o, "ucrow002", "unconfirmed", "next (done-file row)");
    assert!(!o.stdout.contains("ucrow002"), "{}", o.both());
}

// ---- (f) intake never files a row without a reproduced repro ----

fn add_args<'a>(p: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut a = vec!["add", "--title", "t", "--project", p];
    a.extend_from_slice(extra);
    a
}

#[test]
fn add_without_repro_test_exits_nonzero_and_files_no_row() {
    let f = Fixture::new("uc-add-none");
    let p = f.project();
    let before = std::fs::read(f.tasks_path()).unwrap_or_default();
    let o = f.run(&add_args(&p, &[]));
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(
        std::fs::read(f.tasks_path()).unwrap_or_default(),
        before,
        "tasks.toml must gain no row: {}",
        o.both()
    );
}

#[test]
fn add_with_non_reproducing_repro_exits_nonzero_and_files_no_row() {
    let f = Fixture::new("uc-add-no");
    let p = f.project();
    let before = std::fs::read(f.tasks_path()).unwrap_or_default();
    let o = f.run(&add_args(&p, &["--repro-test", REPRO_NO]));
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(
        std::fs::read(f.tasks_path()).unwrap_or_default(),
        before,
        "tasks.toml must gain no row: {}",
        o.both()
    );
}

// ---- (g) the removed statuses are unknown filters ----

#[test]
fn status_filters_for_removed_statuses_warn_unknown_status() {
    let f = Fixture::new("uc-filter");
    let _ok = f.add("normal pending");
    for s in ["unconfirmed", "needs-ruling"] {
        let o = f.run(&["list", "--status", s]);
        assert!(
            o.both().contains(&format!("unknown status '{s}'")),
            "`list --status {s}` must warn unknown status: {}",
            o.both()
        );
    }
}

// ---- (h) history: unconfirmed then discard-cancelled still loads ----

#[test]
fn historical_unconfirmed_then_discard_cancelled_row_still_loads() {
    let f = Fixture::new("uc-hist");
    let closure = format!(
        "[task.closure]\nreason = \"discard\"\ndiscard_reason = \"unproven: x\"\n{UNCONF_REPRO}"
    );
    append(
        &f.done_path(),
        &raw_row("uchist01", "cancelled", &f.project(), &closure),
    );
    let o = f.run(&["list", "--all", "--status", "cancelled"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(o.stdout.contains("uchist01"), "{}", o.both());
    let o = f.run(&["list", "--all", "--json"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(o.stdout.contains("uchist01"), "{}", o.both());
}
