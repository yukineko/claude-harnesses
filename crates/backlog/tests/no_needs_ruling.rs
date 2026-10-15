//! User ruling 2026-10-15: a backlog item with no RED evidence is a false
//! detection, so the `needs-ruling` status and the `backlog ruling ...` verbs
//! must not exist. A stray live `status = "needs-ruling"` row is FAIL-CLOSED
//! (store-reading commands exit non-zero naming its id and `needs-ruling`).
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

fn raw_row(f: &Fixture, id: &str, status: &str, project: &str, extra: &str) -> String {
    let _ = f;
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"row {id}\"\nproject = \"{project}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n{extra}\n"
    )
}

fn assert_named_needs_ruling_refusal(o: &Out, id: &str, what: &str) {
    assert_ne!(o.code, 0, "{what}: must exit non-zero: {}", o.both());
    let b = o.both();
    assert!(b.contains(id), "{what}: must name the row id {id}: {b}");
    assert!(
        b.contains("needs-ruling"),
        "{what}: must name `needs-ruling`: {b}"
    );
}

// ---- (a) the ruling verbs are gone ----

#[test]
fn ruling_request_judgment_is_rejected_and_row_unchanged() {
    let f = Fixture::new("nr-rj");
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&[
        "ruling",
        "request",
        &id,
        "--kind",
        "judgment",
        "--rationale",
        "x",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "ruling request judgment");
    assert_eq!(f.store_bytes(), before, "store must be byte-identical");
}

#[test]
fn ruling_request_untestable_is_rejected_and_row_unchanged() {
    let f = Fixture::new("nr-ru");
    let id = f.add("t");
    let before = f.store_bytes();
    let o = f.run(&[
        "ruling",
        "request",
        &id,
        "--kind",
        "untestable",
        "--untestable-reason",
        "x",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "ruling request untestable");
    assert_eq!(f.store_bytes(), before, "store must be byte-identical");
}

#[test]
fn ruling_approve_withdraw_list_are_rejected() {
    let f = Fixture::new("nr-rawl");
    let id = f.add("t");
    for args in [
        vec!["ruling", "approve", id.as_str()],
        vec!["ruling", "withdraw", id.as_str()],
        vec!["ruling", "list"],
    ] {
        let o = f.run(&args);
        assert_ne!(o.code, 0, "{args:?} must be rejected: {}", o.both());
    }
    assert_eq!(f.status(&id), "pending");
}

// ---- (c) a live needs-ruling row is fail-closed ----

fn plant_nr(f: &Fixture, id: &str, file: &std::path::Path) {
    let p = f.project();
    append(
        file,
        &raw_row(
            f,
            id,
            "needs-ruling",
            &p,
            "ruling_kind = \"judgment\"\nrationale = \"x\"",
        ),
    );
}

#[test]
fn list_fails_closed_on_a_needs_ruling_row_in_tasks_toml() {
    let f = Fixture::new("nr-live-list");
    let _ok = f.add("normal pending");
    plant_nr(&f, "nrow0001", &f.tasks_path());
    let o = f.run(&["list"]);
    assert_named_needs_ruling_refusal(&o, "nrow0001", "list");
    let o = f.run(&["list", "--all", "--json"]);
    assert_named_needs_ruling_refusal(&o, "nrow0001", "list --all --json");
}

#[test]
fn next_fails_closed_on_a_needs_ruling_row_in_tasks_toml() {
    let f = Fixture::new("nr-live-next");
    let _ok = f.add("normal pending");
    plant_nr(&f, "nrow0001", &f.tasks_path());
    let o = f.run(&["next"]);
    assert_named_needs_ruling_refusal(&o, "nrow0001", "next");
    assert!(
        !o.stdout.contains("nrow0001"),
        "next must not hand out the needs-ruling row: {}",
        o.both()
    );
}

#[test]
fn needs_ruling_row_only_in_done_file_without_twin_fails_closed() {
    let f = Fixture::new("nr-donefile");
    let _ok = f.add("normal pending");
    plant_nr(&f, "nrow0002", &f.done_path());
    let o = f.run(&["list"]);
    assert_named_needs_ruling_refusal(&o, "nrow0002", "list (done-file row)");
    let o = f.run(&["next"]);
    assert_named_needs_ruling_refusal(&o, "nrow0002", "next (done-file row)");
}

// ---- (d) no guidance toward the removed verbs ----

#[test]
fn bare_done_refusal_does_not_point_at_ruling_request() {
    let f = Fixture::new("nr-guid");
    let id = f.add("t");
    let o = f.run(&["done", &id]);
    assert_ne!(o.code, 0, "bare done must be refused: {}", o.both());
    assert!(
        !o.both().contains("ruling request"),
        "refusal still advertises `ruling request`: {}",
        o.both()
    );
    let h = f.run(&["done", "--help"]);
    assert!(
        !h.both().contains("ruling request"),
        "`done --help` still advertises `ruling request`: {}",
        h.both()
    );
    let h = f.run(&["--help"]);
    assert!(
        !h.both().contains("ruling"),
        "top-level --help still lists a ruling verb: {}",
        h.both()
    );
}

#[test]
fn session_start_context_does_not_mention_ruling_verbs() {
    let f = Fixture::new("nr-ss");
    let _id = f.add("pending for context");
    let input = format!(
        "{{\"session_id\":\"s\",\"cwd\":{},\"hook_event_name\":\"SessionStart\"}}",
        serde_json::to_string(&f.project()).unwrap()
    );
    let o = f.run_stdin(&["session-start"], &[], input.as_bytes());
    assert_eq!(o.code, 0, "{}", o.both());
    // Positive control so a silent hook cannot pass vacuously.
    assert!(
        o.stdout.contains("backlog done"),
        "session-start produced no task context: {}",
        o.both()
    );
    let b = o.both();
    assert!(
        !b.contains("ruling request") && !b.contains("ruling approve"),
        "session-start context still advertises ruling verbs: {b}"
    );
}
