//! `backlog audit-open [--json]` — check-once audit of OPEN rows for a
//! recorded reproducing test (spec6, user ruling 2026-10-04:
//! 「完了のテストはみなくていい。pendingにテストがあるかだけ確認すればいいし2度確認しなくていい」).
//!
//! Written by an independent test author (CLAUDE.md §2(a)). Drives the REAL
//! binary through the shared [`Fixture`] (isolated HOME + linked git worktree).
//!
//! Contract pinned here:
//! - Scope: stored status `pending` / `failed` only. `done` and `cancelled`
//!   (terminal, in tasks.done.toml) are NOT audited. (`unconfirmed` and
//!   `needs-ruling` were removed 2026-10-15; a store holding one is refused
//!   by `list` and `next` — see tests/no_needs_ruling.rs, tests/no_unconfirmed.rs.)
//! - Classification is READ from the recorded `repro` (no test is executed):
//!   `reproduced` -> has-test; none / `not-reproduced` -> no-test; anything
//!   else -> undetermined.
//! - Ledger `$HOME/.backlog/audit-open/<slug>.json`
//!   `{"checked": {"<id>": {"result": "has-test"|"no-test", "checked_at": <int>}}}`;
//!   undetermined is NOT recorded; a recorded id is skipped forever (even if
//!   its repro changes later).
//! - Unparseable ledger -> non-zero, EMPTY stdout, stderr `UNDETERMINED`,
//!   ledger bytes untouched.
//! - The store files are byte-identical before/after every run.
mod common;
use common::*;

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

// ---- fixture helpers (local; common/mod.rs is not modified) -----------------

/// Append a row to `file` (tasks.toml or tasks.done.toml) with an optional
/// `[task.repro]` table `(outcome, cmd)`.
fn plant(
    f: &Fixture,
    file: &PathBuf,
    id: &str,
    title: &str,
    status: &str,
    project: &str,
    repro: Option<(&str, &str)>,
) {
    let mut body = format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{project}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n"
    );
    if let Some((outcome, cmd)) = repro {
        body.push_str(&repro_table(outcome, cmd));
    }
    append(file, &body);
    let _ = f;
}

fn repro_table(outcome: &str, cmd: &str) -> String {
    format!(
        "\n[task.repro]\noutcome = \"{outcome}\"\ncmd = \"{cmd}\"\nobserved_at = 1\ndetail = \"planted\"\n"
    )
}

fn append(file: &PathBuf, body: &str) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mut cur = std::fs::read_to_string(file).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(body);
    std::fs::write(file, cur).unwrap();
}

fn ledger_dir(f: &Fixture) -> PathBuf {
    f.home.join(".backlog").join("audit-open")
}

/// The single ledger json file (exactly one expected).
fn ledger_file(f: &Fixture) -> PathBuf {
    let dir = ledger_dir(f);
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("ledger dir {} not readable: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(
        files.len(),
        1,
        "exactly one ledger json expected under {}: {files:?}",
        dir.display()
    );
    files.into_iter().next().unwrap()
}

fn ledger(f: &Fixture) -> Value {
    let p = ledger_file(f);
    let s = std::fs::read_to_string(&p).unwrap();
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("ledger not json ({e}): {s:?}"))
}

/// Run `audit-open` and assert the store is byte-identical across the run.
fn audit(f: &Fixture, json: bool) -> Out {
    let before = f.store_bytes();
    let o = if json {
        f.run(&["audit-open", "--json"])
    } else {
        f.run(&["audit-open"])
    };
    assert_eq!(
        f.store_bytes(),
        before,
        "audit-open must not modify tasks.toml / tasks.done.toml: {}",
        o.both()
    );
    o
}

fn audit_json(f: &Fixture) -> Value {
    let o = audit(f, true);
    assert_eq!(o.code, 0, "audit-open --json must succeed: {}", o.both());
    serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("audit-open --json stdout not JSON ({e}): {:?}", o.stdout))
}

fn id_set(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected an array of ids, got {v}"))
        .iter()
        .map(|x| {
            x.as_str()
                .unwrap_or_else(|| panic!("id not a string: {x}"))
                .to_string()
        })
        .collect()
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// `checked_now` as id -> (status, result, title).
fn checked_now(v: &Value) -> std::collections::BTreeMap<String, (String, String, String)> {
    v["checked_now"]
        .as_array()
        .unwrap_or_else(|| panic!("checked_now must be an array: {v}"))
        .iter()
        .map(|e| {
            let s = |k: &str| {
                e[k].as_str()
                    .unwrap_or_else(|| panic!("checked_now entry lacks string {k:?}: {e}"))
                    .to_string()
            };
            (s("id"), (s("status"), s("result"), s("title")))
        })
        .collect()
}

const PEND_YES: &str = "a0e00001";
const PEND_NONE: &str = "a0e00002";
const FAIL_NOTREP: &str = "a0e00003";
const UNCONF_WEIRD: &str = "a0e00004";
const DONE_ROW: &str = "a0e00006";
const CANCELLED_ROW: &str = "a0e00007";

/// The standard scenario. Returns (filed id, project label).
/// Open rows: the filed task (pending, reproduced) + PEND_YES (has-test),
/// PEND_NONE + FAIL_NOTREP (no-test), UNCONF_WEIRD (a pending row whose repro
/// outcome is unrecognised: undetermined).
/// Excluded: DONE_ROW / CANCELLED_ROW (done file).
fn scenario(f: &Fixture, with_undetermined: bool) -> (String, String) {
    let (filed, label) = f.add_with_label("filed reproduced finding");
    let live = f.tasks_path();
    let done = f.done_path();
    plant(
        f,
        &live,
        PEND_YES,
        "pending with test",
        "pending",
        &label,
        Some(("reproduced", REPRO_YES)),
    );
    plant(
        f,
        &live,
        PEND_NONE,
        "pending without repro",
        "pending",
        &label,
        None,
    );
    plant(
        f,
        &live,
        FAIL_NOTREP,
        "failed not reproduced",
        "failed",
        &label,
        Some(("not-reproduced", REPRO_NO)),
    );
    if with_undetermined {
        plant(
            f,
            &live,
            UNCONF_WEIRD,
            "pending weird",
            "pending",
            &label,
            Some(("weird", REPRO_YES)),
        );
    }
    plant(f, &done, DONE_ROW, "done row", "done", &label, None);
    plant(
        f,
        &done,
        CANCELLED_ROW,
        "cancelled row",
        "cancelled",
        &label,
        None,
    );
    // Sanity: the planted store parses and every row is visible to the CLI.
    for id in [PEND_YES, PEND_NONE, FAIL_NOTREP, DONE_ROW, CANCELLED_ROW] {
        let _ = f.row(id);
    }
    (filed, label)
}

// ---- tests ------------------------------------------------------------------

#[test]
fn first_run_classifies_open_rows_and_excludes_terminal() {
    let f = Fixture::new("ao-first");
    let (filed, _) = scenario(&f, true);
    let v = audit_json(&f);

    for k in [
        "checked_now",
        "no_test",
        "undetermined",
        "skipped_already_checked",
        "recorded_no_test_still_open",
    ] {
        assert!(v.get(k).is_some(), "JSON lacks key {k:?}: {v}");
    }

    let now = checked_now(&v);
    let got: BTreeSet<String> = now.keys().cloned().collect();
    assert_eq!(
        got,
        set(&[&filed, PEND_YES, PEND_NONE, FAIL_NOTREP, UNCONF_WEIRD]),
        "first run checks exactly the open rows (no done/cancelled): {v}"
    );
    let expect = [
        (filed.as_str(), "pending", "has-test"),
        (PEND_YES, "pending", "has-test"),
        (PEND_NONE, "pending", "no-test"),
        (FAIL_NOTREP, "failed", "no-test"),
        (UNCONF_WEIRD, "pending", "undetermined"),
    ];
    for (id, status, result) in expect {
        let (s, r, _) = &now[id];
        assert_eq!(s, status, "status of {id}: {v}");
        assert_eq!(r, result, "result of {id}: {v}");
    }
    assert_eq!(
        now[PEND_NONE].2, "pending without repro",
        "title carried: {v}"
    );

    assert_eq!(id_set(&v["no_test"]), set(&[PEND_NONE, FAIL_NOTREP]), "{v}");
    assert_eq!(id_set(&v["undetermined"]), set(&[UNCONF_WEIRD]), "{v}");
    assert_eq!(v["skipped_already_checked"], 0, "{v}");
    let recorded = id_set(&v["recorded_no_test_still_open"]);
    assert!(
        recorded.is_subset(&set(&[PEND_NONE, FAIL_NOTREP])),
        "recorded_no_test_still_open may only hold no-test ids: {v}"
    );
}

#[test]
fn first_run_ledger_records_has_and_no_test_but_not_undetermined() {
    let f = Fixture::new("ao-ledger");
    let (filed, _) = scenario(&f, true);
    let o = audit(&f, true);
    assert_eq!(o.code, 0, "audit-open must succeed: {}", o.both());

    let l = ledger(&f);
    let checked = l["checked"]
        .as_object()
        .unwrap_or_else(|| panic!("ledger lacks object `checked`: {l}"));
    let ids: BTreeSet<String> = checked.keys().cloned().collect();
    assert_eq!(
        ids,
        set(&[&filed, PEND_YES, PEND_NONE, FAIL_NOTREP]),
        "ledger records has-test/no-test rows only (undetermined is re-examined): {l}"
    );
    for (id, want) in [
        (filed.as_str(), "has-test"),
        (PEND_YES, "has-test"),
        (PEND_NONE, "no-test"),
        (FAIL_NOTREP, "no-test"),
    ] {
        assert_eq!(checked[id]["result"], want, "ledger result for {id}: {l}");
        assert!(
            checked[id]["checked_at"].is_i64() || checked[id]["checked_at"].is_u64(),
            "checked_at must be an integer for {id}: {l}"
        );
    }
    for id in [UNCONF_WEIRD, DONE_ROW, CANCELLED_ROW] {
        assert!(
            !checked.contains_key(id),
            "{id} must not be in the ledger: {l}"
        );
    }
}

#[test]
fn second_run_skips_checked_and_reexamines_undetermined() {
    let f = Fixture::new("ao-second");
    let _ = scenario(&f, true);
    let first = audit(&f, true);
    assert_eq!(first.code, 0, "first run: {}", first.both());

    let v = audit_json(&f);
    let now = checked_now(&v);
    assert_eq!(
        now.keys().cloned().collect::<BTreeSet<_>>(),
        set(&[UNCONF_WEIRD]),
        "second run re-examines only the undetermined row: {v}"
    );
    assert_eq!(now[UNCONF_WEIRD].1, "undetermined", "{v}");
    assert_eq!(
        v["skipped_already_checked"], 4,
        "4 rows were recorded on run 1: {v}"
    );
    assert_eq!(
        id_set(&v["no_test"]),
        set(&[]),
        "nothing newly no-test: {v}"
    );
    assert_eq!(id_set(&v["undetermined"]), set(&[UNCONF_WEIRD]), "{v}");
    assert_eq!(
        id_set(&v["recorded_no_test_still_open"]),
        set(&[PEND_NONE, FAIL_NOTREP]),
        "earlier no-test results stay visible while the rows are open: {v}"
    );
}

#[test]
fn human_output_lines_summary_and_nothing_new() {
    let f = Fixture::new("ao-human");
    let _ = scenario(&f, false);
    let o = audit(&f, false);
    assert_eq!(o.code, 0, "human first run: {}", o.both());
    assert!(
        o.stdout.lines().any(|l| l.contains(PEND_NONE)
            && l.contains("no-test")
            && l.contains("pending without repro")),
        "one line per checked row (id, result, title): {}",
        o.stdout
    );
    assert!(
        o.stdout
            .lines()
            .any(|l| l.contains(PEND_YES) && l.contains("has-test")),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("no-test: 2"),
        "summary no-test count: {}",
        o.stdout
    );
    assert!(
        o.stdout.contains("skipped (already checked): 0"),
        "summary skipped count: {}",
        o.stdout
    );

    let o2 = audit(&f, false);
    assert_eq!(o2.code, 0, "human second run: {}", o2.both());
    assert!(
        o2.stdout.contains("no unchecked open rows"),
        "nothing new -> `no unchecked open rows`: {}",
        o2.stdout
    );
    assert!(
        o2.stdout.contains("skipped (already checked): 4"),
        "second-run summary counts the 4 recorded rows: {}",
        o2.stdout
    );
    for id in [PEND_NONE, FAIL_NOTREP] {
        assert!(
            o2.stdout.contains(id),
            "still-open recorded no-test id {id} listed: {}",
            o2.stdout
        );
    }
}

#[test]
fn row_checked_as_no_test_stays_skipped_after_repro_added() {
    let f = Fixture::new("ao-sticky");
    let (_, label) = f.add_with_label("filed reproduced finding");
    // Planted LAST so a trailing [task.repro] table attaches to it.
    plant(
        &f,
        &f.tasks_path(),
        PEND_NONE,
        "pending without repro",
        "pending",
        &label,
        None,
    );
    let v1 = audit_json(&f);
    assert_eq!(checked_now(&v1)[PEND_NONE].1, "no-test", "{v1}");

    append(&f.tasks_path(), &repro_table("reproduced", REPRO_YES));
    let repro = &f.row(PEND_NONE)["repro"];
    assert_eq!(
        repro["outcome"], "reproduced",
        "setup: repro now recorded: {repro}"
    );

    let v2 = audit_json(&f);
    assert!(
        !checked_now(&v2).contains_key(PEND_NONE),
        "a recorded row is never re-checked, even after its repro changed: {v2}"
    );
    assert_eq!(v2["skipped_already_checked"], 2, "{v2}");
    assert_eq!(
        ledger(&f)["checked"][PEND_NONE]["result"],
        "no-test",
        "ledger unchanged"
    );
}

#[test]
fn new_open_row_after_first_run_is_checked_on_second() {
    let f = Fixture::new("ao-new");
    let (_, label) = scenario(&f, false);
    let first = audit(&f, true);
    assert_eq!(first.code, 0, "first run: {}", first.both());

    let fresh = "a0e00099";
    plant(
        &f,
        &f.tasks_path(),
        fresh,
        "planted later",
        "pending",
        &label,
        None,
    );
    let v = audit_json(&f);
    let now = checked_now(&v);
    assert_eq!(
        now.keys().cloned().collect::<BTreeSet<_>>(),
        set(&[fresh]),
        "only the new row is checked: {v}"
    );
    assert_eq!(now[fresh].1, "no-test", "{v}");
    assert_eq!(id_set(&v["no_test"]), set(&[fresh]), "{v}");
    assert_eq!(v["skipped_already_checked"], 4, "{v}");
    assert_eq!(
        id_set(&v["recorded_no_test_still_open"]),
        set(&[PEND_NONE, FAIL_NOTREP, fresh]),
        "{v}"
    );
    assert_eq!(ledger(&f)["checked"][fresh]["result"], "no-test");
}

fn garbage_ledger_case(json: bool) {
    let f = Fixture::new(if json { "ao-garbage-j" } else { "ao-garbage-h" });
    let _ = scenario(&f, true);
    let first = audit(&f, json);
    assert_eq!(
        first.code,
        0,
        "first run creates the ledger: {}",
        first.both()
    );
    let p = ledger_file(&f);
    let garbage = b"{ this is not json".to_vec();
    std::fs::write(&p, &garbage).unwrap();

    let o = audit(&f, json);
    assert_ne!(
        o.code,
        0,
        "unparseable ledger must fail (json={json}): {}",
        o.both()
    );
    assert!(
        o.stdout.is_empty(),
        "unparseable ledger: stdout must be EMPTY (json={json}): {:?}",
        o.stdout
    );
    assert!(
        o.stderr.contains("UNDETERMINED"),
        "stderr names UNDETERMINED (json={json}): {}",
        o.stderr
    );
    assert!(
        o.stderr.contains(p.file_name().unwrap().to_str().unwrap()),
        "stderr names the ledger path (json={json}): {}",
        o.stderr
    );
    assert_eq!(
        std::fs::read(&p).unwrap(),
        garbage,
        "ledger bytes untouched (json={json})"
    );
}

#[test]
fn garbage_ledger_is_undetermined_human() {
    garbage_ledger_case(false);
}

#[test]
fn garbage_ledger_is_undetermined_json() {
    garbage_ledger_case(true);
}

#[test]
fn store_bytes_unchanged_across_runs() {
    let f = Fixture::new("ao-bytes");
    let _ = scenario(&f, true);
    let before = f.store_bytes();
    for json in [true, false, true] {
        let o = audit(&f, json);
        assert_eq!(o.code, 0, "audit-open (json={json}): {}", o.both());
    }
    assert_eq!(
        f.store_bytes(),
        before,
        "store byte-identical after three runs"
    );
}

#[test]
fn no_test_is_executed() {
    let f = Fixture::new("ao-noexec");
    let (_, label) = f.add_with_label("filed reproduced finding");
    let marker = f.home.join("audit-open-EXECUTED-A-TEST");
    f.write_script(
        "tests/make_marker.sh",
        &format!("touch '{}'; exit 1", marker.display()),
    );
    f.commit_paths(&["tests/make_marker.sh"], "marker script");
    let cmd = "bash tests/make_marker.sh";
    let live = f.tasks_path();
    plant(
        &f,
        &live,
        "a0e00011",
        "marker reproduced",
        "pending",
        &label,
        Some(("reproduced", cmd)),
    );
    plant(
        &f,
        &live,
        "a0e00012",
        "marker not reproduced",
        "failed",
        &label,
        Some(("not-reproduced", cmd)),
    );
    plant(
        &f,
        &live,
        "a0e00013",
        "marker undetermined",
        "pending",
        &label,
        Some(("undetermined", cmd)),
    );
    assert!(!marker.exists(), "setup: marker must not exist yet");

    for json in [true, false] {
        let o = audit(&f, json);
        assert_eq!(o.code, 0, "audit-open (json={json}): {}", o.both());
    }
    assert!(
        !marker.exists(),
        "audit-open must READ the recorded repro, never execute repro.cmd"
    );
}
