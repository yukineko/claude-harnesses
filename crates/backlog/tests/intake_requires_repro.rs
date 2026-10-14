//! Intake contract v2 (user rulings 2026-10-04):
//! 「testがないなら起票してはならない」 /
//! 「見つけたら テストを実装する、検証されたときには起票する、テストがかけない、
//!   証拠がない場合は起票はしてはいけない。ただし記録はのこし同一の内容を検討しないようにする」
//!
//! Written by an independent test author (CLAUDE.md §2(a)). Drives the REAL
//! binary through the shared fixture (isolated HOME + linked git worktree with
//! committed `tests/repro_yes.sh` (exit 1 = reproduced) and
//! `tests/repro_no.sh` (exit 0 = not reproduced)).
//!
//! Contract pinned here:
//! 1. `add` writes a task ONLY for a `reproduced` repro. Every other outcome
//!    (no `--repro-test`, `not-reproduced`, `undetermined`) writes NO row,
//!    exits exactly 2, and appends a `[[record]]` to `.backlog/unproven.toml`.
//!    An unwritable record (corrupt unproven.toml) is a hard error != 0, != 2,
//!    with the file's bytes untouched.
//! 2. Every `add` scans unproven.toml and reports `unproven-match: <id>` or
//!    an explicit `no prior unproven record`; unreadable -> `UNDETERMINED`.
//!    `unproven list|search` read the record; unreadable -> exit 3, empty stdout.
//! 3. Legacy rows: `next` carries `repro_required`; `confirm` accepts a
//!    pending task; old `unconfirmed` rows are flagged `expiry_candidate`
//!    (never auto-closed).
mod common;
use common::*;

use std::path::PathBuf;

const UNDET_SCRIPT: &str = "bash tests/untracked.sh";

fn unproven_path(f: &Fixture) -> PathBuf {
    f.tasks_path().with_file_name("unproven.toml")
}

/// All `[[record]]` entries of unproven.toml (empty when the file is absent).
fn records(f: &Fixture) -> Vec<toml::Value> {
    let p = unproven_path(f);
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Vec::new();
    };
    let v: toml::Value = toml::from_str(&s)
        .unwrap_or_else(|e| panic!("unproven.toml at {} unparseable ({e}):\n{s}", p.display()));
    v.get("record")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
}

fn s<'a>(rec: &'a toml::Value, key: &str) -> Option<&'a str> {
    rec.get(key).and_then(|v| v.as_str())
}

fn unproven_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("unproven: "))
        .map(|x| x.trim().to_string())
}

fn has_added_line(stdout: &str) -> bool {
    stdout.lines().any(|l| l.starts_with("added: "))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Plant a row straight into tasks.toml (add can no longer create
/// `unconfirmed`, and a legacy pending row has no repro by construction).
/// Shape copied from `plant_legacy_done` in common/mod.rs.
fn plant_task(f: &Fixture, id: &str, title: &str, status: &str, created_at: i64) {
    let body = format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = {created_at}\nupdated_at = {created_at}\nweight = 0.0\n",
        f.project()
    );
    let p = f.tasks_path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(&body);
    std::fs::write(&p, cur).unwrap();
}

/// The STORED status of a row, read from tasks.toml itself (not the derived
/// `claimed` view `list` renders while a lease is live).
fn stored_status(f: &Fixture, id: &str) -> String {
    let s = std::fs::read_to_string(f.tasks_path()).expect("tasks.toml readable");
    let v: toml::Value = toml::from_str(&s).expect("tasks.toml parses");
    v.get("task")
        .and_then(|t| t.as_array())
        .and_then(|a| {
            a.iter()
                .find(|t| t.get("id").and_then(|x| x.as_str()) == Some(id))
        })
        .and_then(|t| t.get("status").and_then(|x| x.as_str()))
        .unwrap_or_else(|| panic!("row {id} not in tasks.toml:\n{s}"))
        .to_string()
}

fn plant_unproven(f: &Fixture, id: &str, title: &str, notes: &str) {
    let body = format!(
        "[[record]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{}\"\nnotes = \"{notes}\"\ntags = []\noutcome = \"no-repro-test\"\ndetail = \"no --repro-test given\"\nrecorded_at = 1\n",
        f.project()
    );
    let p = unproven_path(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
    cur.push_str(&body);
    std::fs::write(&p, cur).unwrap();
}

const GARBAGE: &[u8] = b"\x00\xff[[record]\nid = = \"broken\nthis is not toml {{{\n";

fn corrupt_unproven(f: &Fixture) {
    let p = unproven_path(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, GARBAGE).unwrap();
}

/// Parse the JSON object a `next` call printed on stdout.
fn next_json(o: &Out) -> serde_json::Value {
    serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("next stdout is not a JSON object ({e}): {}", o.both()))
}

// ---------------------------------------------------------------- §1 intake

#[test]
fn add_without_repro_test_files_no_task_and_records_no_repro_test() {
    let f = Fixture::new("ir-norepro");
    let (o, id) = f.add_extra(
        "finding without a test",
        &["--notes", "seen once in a log", "--tag", "x"],
    );
    assert_eq!(
        o.code,
        2,
        "add without --repro-test must exit exactly 2 (not filed): {}",
        o.both()
    );
    assert!(
        id.is_none() && !has_added_line(&o.stdout),
        "no `added:` line: {}",
        o.stdout
    );
    assert!(
        f.list_all().is_empty(),
        "store must gain no row: {:?}",
        f.list_all()
    );
    let recs = records(&f);
    assert_eq!(recs.len(), 1, "exactly one unproven record: {recs:?}");
    let r = &recs[0];
    let rid = s(r, "id").expect("record id");
    assert!(rid.starts_with("u-"), "record id has prefix u-: {rid}");
    assert_eq!(s(r, "outcome"), Some("no-repro-test"), "{r:?}");
    assert_eq!(s(r, "title"), Some("finding without a test"), "{r:?}");
    assert_eq!(s(r, "notes"), Some("seen once in a log"), "{r:?}");
    assert!(
        r.get("repro_test").is_none(),
        "no repro_test when none given: {r:?}"
    );
    assert!(
        r.get("recorded_at").and_then(|v| v.as_integer()).is_some(),
        "{r:?}"
    );
    assert_eq!(
        unproven_line(&o.stdout).as_deref(),
        Some(rid),
        "stdout carries `unproven: <record-id>`: {}",
        o.stdout
    );
    // `project` is the SAME label a filed task would carry (the canonical
    // project identity, not the raw linked-worktree path), so the
    // re-examination guard matches records across worktrees of one project.
    // Measured by filing a reproduced task in this fixture AFTER the
    // "no row" assertions above.
    let rec_project = s(r, "project").map(str::to_string);
    let filed = f.add("reference task for the project label");
    let filed_project = f.row(&filed)["project"].as_str().unwrap().to_string();
    assert_eq!(
        rec_project.as_deref(),
        Some(filed_project.as_str()),
        "record project must equal a filed task's project label: {r:?}"
    );
}

#[test]
fn add_not_reproduced_files_no_task_and_records_not_reproduced() {
    let f = Fixture::new("ir-notrepro");
    let (o, id) = f.add_extra("phantom bug", &["--repro-test", REPRO_NO]);
    assert_eq!(
        o.code,
        2,
        "not-reproduced must exit exactly 2: {}",
        o.both()
    );
    assert!(id.is_none() && !has_added_line(&o.stdout), "{}", o.stdout);
    assert!(f.list_all().is_empty(), "store must gain no row");
    let recs = records(&f);
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(
        s(&recs[0], "outcome"),
        Some("not-reproduced"),
        "{:?}",
        recs[0]
    );
    assert_eq!(s(&recs[0], "repro_test"), Some(REPRO_NO), "{:?}", recs[0]);
    assert_eq!(s(&recs[0], "title"), Some("phantom bug"));
    assert_eq!(
        unproven_line(&o.stdout).as_deref(),
        s(&recs[0], "id"),
        "{}",
        o.stdout
    );
}

#[test]
fn add_undetermined_repro_files_no_task_and_records_undetermined() {
    let f = Fixture::new("ir-undet");
    // Exists but is NOT tracked at HEAD -> the runner cannot run it -> undetermined.
    f.write_script("tests/untracked.sh", "exit 1");
    let (o, id) = f.add_extra("cannot run my repro", &["--repro-test", UNDET_SCRIPT]);
    assert_eq!(o.code, 2, "undetermined must exit exactly 2: {}", o.both());
    assert!(id.is_none() && !has_added_line(&o.stdout), "{}", o.stdout);
    assert!(
        f.list_all().is_empty(),
        "store must gain no row (undetermined is not pending)"
    );
    let recs = records(&f);
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(
        s(&recs[0], "outcome"),
        Some("undetermined"),
        "{:?}",
        recs[0]
    );
    assert_eq!(s(&recs[0], "repro_test"), Some(UNDET_SCRIPT));
}

/// The reproduced path itself is UNCHANGED (exit 0, `added:`, pending,
/// outcome reproduced, no unproven record) — those assertions pin an existing
/// invariant. What is NEW (and makes this test RED today) is the
/// re-examination report every add must print: with no unproven.toml there is
/// no prior record, and that must be SAID (`no prior unproven record`).
#[test]
fn add_reproduced_files_pending_writes_no_unproven_record_and_reports_scan() {
    let f = Fixture::new("ir-repro");
    let (o, id) = f.add_extra("real bug", &["--repro-test", REPRO_YES]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.expect("added: <id>");
    assert_eq!(f.status(&id), "pending");
    assert!(
        f.row(&id).to_string().contains("\"reproduced\""),
        "{}",
        f.row(&id)
    );
    assert!(
        !unproven_path(&f).exists(),
        "no unproven record for a reproduced finding"
    );
    assert!(unproven_line(&o.stdout).is_none(), "{}", o.stdout);
    // NEW behaviour:
    assert!(
        o.stderr.contains("no prior unproven record"),
        "every add reports the re-examination scan, even when empty: {}",
        o.stderr
    );
}

// ------------------------------------------------------- §2 re-examination

#[test]
fn second_add_of_same_or_similar_finding_reports_prior_unproven_record() {
    let f = Fixture::new("ir-reexam");
    let title = "parser drops trailing comma in config";
    let (o1, _) = f.add_extra(title, &[]);
    assert_eq!(o1.code, 2, "first add is not filed: {}", o1.both());
    let first = unproven_line(&o1.stdout)
        .unwrap_or_else(|| panic!("first add prints `unproven: <id>`: {}", o1.both()));

    // Same title, unproven path again.
    let (o2, _) = f.add_extra(title, &[]);
    assert!(
        o2.stderr.contains(&format!("unproven-match: {first}")),
        "same title must surface the prior record: {}",
        o2.stderr
    );
    // Similar title on the reproduced path also reports it, and is still filed.
    let (o3, id3) = f.add_extra(
        "parser drops trailing comma in config file",
        &["--repro-test", REPRO_YES],
    );
    assert_eq!(o3.code, 0, "{}", o3.both());
    assert!(id3.is_some(), "{}", o3.stdout);
    assert!(
        o3.stderr.contains(&format!("unproven-match: {first}")),
        "similar title must surface the prior record: {}",
        o3.stderr
    );
    // An unrelated finding: explicit `no prior unproven record`, no match line.
    let (o4, _) = f.add_extra("quantum widget overflow zebra", &[]);
    assert!(
        o4.stderr.contains("no prior unproven record"),
        "unrelated finding must say so explicitly: {}",
        o4.stderr
    );
    assert!(!o4.stderr.contains("unproven-match:"), "{}", o4.stderr);
}

#[test]
fn corrupt_unproven_record_is_hard_error_on_unproven_path() {
    let f = Fixture::new("ir-corrupt1");
    corrupt_unproven(&f);
    let (o, id) = f.add_extra("finding while record is corrupt", &[]);
    assert!(
        o.code != 0 && o.code != 2,
        "unwritable record must be a hard error (not 0, not 2), got {}: {}",
        o.code,
        o.both()
    );
    assert!(id.is_none() && !has_added_line(&o.stdout), "{}", o.stdout);
    assert_eq!(
        std::fs::read(unproven_path(&f)).unwrap(),
        GARBAGE,
        "existing unproven.toml bytes must not be modified"
    );
    assert!(f.list_all().is_empty(), "no task written");
    assert!(o.stderr.contains("UNDETERMINED"), "{}", o.stderr);
    assert!(
        !o.stderr.contains("no prior unproven record"),
        "an unreadable record is not 'no prior record': {}",
        o.stderr
    );
}

#[test]
fn corrupt_unproven_record_still_files_reproduced_finding_but_says_undetermined() {
    let f = Fixture::new("ir-corrupt2");
    corrupt_unproven(&f);
    let (o, id) = f.add_extra("real bug with corrupt record", &["--repro-test", REPRO_YES]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some(), "added: line: {}", o.stdout);
    assert!(
        o.stderr.contains("UNDETERMINED"),
        "re-examination scan could not run; must say UNDETERMINED: {}",
        o.stderr
    );
    assert!(
        !o.stderr.contains("no prior unproven record"),
        "{}",
        o.stderr
    );
    assert_eq!(std::fs::read(unproven_path(&f)).unwrap(), GARBAGE);
}

// ---------------------------------------------------------- unproven read

#[test]
fn unproven_search_hits_by_substring_title_notes_and_japanese() {
    let f = Fixture::new("ir-search");
    plant_unproven(
        &f,
        "u-aaaa1111",
        "パーサが末尾カンマを落とす",
        "trailing comma lost",
    );
    plant_unproven(&f, "u-bbbb2222", "flaky lock heartbeat", "");

    let o = f.run(&["unproven", "search", "末尾カンマ"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(o.stdout.contains("u-aaaa1111"), "{}", o.stdout);
    assert!(!o.stdout.contains("u-bbbb2222"), "{}", o.stdout);

    let o = f.run(&["unproven", "search", "HEARTBEAT"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("u-bbbb2222") && o.stdout.contains("flaky lock heartbeat"),
        "{}",
        o.stdout
    );
    assert!(!o.stdout.contains("u-aaaa1111"), "{}", o.stdout);

    let o = f.run(&["unproven", "search", "Trailing COMMA"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("u-aaaa1111"),
        "notes substring hit: {}",
        o.stdout
    );

    let o = f.run(&["unproven", "search", "nonexistent xyzzy"]);
    assert_eq!(o.code, 0, "a miss is determined: {}", o.both());
    assert!(
        o.stdout.contains("no unproven record matches"),
        "{}",
        o.stdout
    );

    let o = f.run(&["unproven", "search", "heartbeat", "--json"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("--json not JSON ({e}): {}", o.stdout));
    let m = v["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("{{\"matches\":[...]}}: {v}"));
    assert_eq!(m.len(), 1, "{v}");
    assert_eq!(m[0]["id"], "u-bbbb2222");
    assert_eq!(m[0]["outcome"], "no-repro-test");
    assert_eq!(m[0]["title"], "flaky lock heartbeat");

    let o = f.run(&["unproven", "list"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("u-aaaa1111") && o.stdout.contains("u-bbbb2222"),
        "{}",
        o.stdout
    );
}

#[test]
fn unproven_search_missing_file_is_zero_records() {
    let f = Fixture::new("ir-search-missing");
    assert!(!unproven_path(&f).exists());
    let o = f.run(&["unproven", "search", "anything"]);
    assert_eq!(
        o.code,
        0,
        "missing record = zero records, determined: {}",
        o.both()
    );
    assert!(
        o.stdout.contains("no unproven record matches"),
        "{}",
        o.stdout
    );
    let o = f.run(&["unproven", "search", "anything", "--json"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("--json not JSON ({e}): {}", o.stdout));
    assert_eq!(v["matches"], serde_json::json!([]), "{v}");
}

#[test]
fn unproven_search_and_list_corrupt_file_exit_3_with_empty_stdout() {
    let f = Fixture::new("ir-search-corrupt");
    corrupt_unproven(&f);
    for args in [
        vec!["unproven", "search", "anything"],
        vec!["unproven", "search", "anything", "--json"],
        vec!["unproven", "list"],
        vec!["unproven", "list", "--json"],
    ] {
        let o = f.run(&args);
        assert_eq!(
            o.code,
            3,
            "{args:?}: unreadable record must exit 3: {}",
            o.both()
        );
        assert!(
            o.stdout.is_empty(),
            "{args:?}: stdout must be EMPTY: {:?}",
            o.stdout
        );
        assert!(o.stderr.contains("UNDETERMINED"), "{args:?}: {}", o.stderr);
        assert!(
            o.stderr.contains("unproven.toml"),
            "{args:?}: stderr names the path: {}",
            o.stderr
        );
    }
    assert_eq!(std::fs::read(unproven_path(&f)).unwrap(), GARBAGE);
}

// ------------------------------------------------------------- §3 legacy

#[test]
fn next_claim_on_legacy_pending_marks_repro_required_without_demotion() {
    let f = Fixture::new("ir-next-legacy");
    plant_task(
        &f,
        "legacy01",
        "old unproven finding",
        "pending",
        now_unix() - 100,
    );
    let p = f.project();

    let o = f.run(&["next", "--project", &p]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v = next_json(&o);
    assert_eq!(v["id"], "legacy01", "{v}");
    assert_eq!(
        v["repro_required"],
        serde_json::json!(true),
        "plain next: {v}"
    );

    let o = f.run(&["next", "--claim", "--project", &p]);
    assert_eq!(o.code, 0, "claim is still granted: {}", o.both());
    let v = next_json(&o);
    assert_eq!(v["id"], "legacy01", "{v}");
    assert_eq!(
        v["repro_required"],
        serde_json::json!(true),
        "next --claim: {v}"
    );
    assert!(
        o.stderr
            .lines()
            .any(|l| l.contains("repro_required") && l.contains("legacy01")),
        "stderr must name repro_required and the id: {}",
        o.stderr
    );
    assert_eq!(
        stored_status(&f, "legacy01"),
        "pending",
        "no demotion of the stored status"
    );
}

#[test]
fn next_claim_on_reproduced_task_is_not_repro_required() {
    let f = Fixture::new("ir-next-repro");
    let id = f.add("bug with a reproduced repro");
    let p = f.project();
    let o = f.run(&["next", "--claim", "--project", &p]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v = next_json(&o);
    assert_eq!(v["id"], id.as_str(), "{v}");
    assert_eq!(v["repro_required"], serde_json::json!(false), "{v}");
}

#[test]
fn confirm_on_legacy_pending_reproduced_keeps_status_and_clears_repro_required() {
    let f = Fixture::new("ir-confirm-yes");
    plant_task(
        &f,
        "legacy02",
        "old finding now tested",
        "pending",
        now_unix() - 100,
    );
    let o = f.run(&["confirm", "legacy02", "--repro-test", REPRO_YES]);
    assert_eq!(
        o.code,
        0,
        "confirm accepts a legacy pending task: {}",
        o.both()
    );
    assert_eq!(
        stored_status(&f, "legacy02"),
        "pending",
        "status stays as it was"
    );
    assert!(
        f.row("legacy02").to_string().contains("\"reproduced\""),
        "{}",
        f.row("legacy02")
    );
    let p = f.project();
    let o = f.run(&["next", "--project", &p]);
    let v = next_json(&o);
    assert_eq!(v["id"], "legacy02", "{v}");
    assert_eq!(v["repro_required"], serde_json::json!(false), "{v}");
}

/// The non-zero exit and unchanged status already hold today (confirm refuses
/// a pending row outright) — that part pins an invariant. What is NEW is that
/// the failed attempt is RECORDED on the row (`repro` with outcome
/// not-reproduced), which is what makes this test RED today.
#[test]
fn confirm_on_legacy_pending_not_reproduced_fails_but_records_attempt() {
    let f = Fixture::new("ir-confirm-no");
    plant_task(
        &f,
        "legacy03",
        "old finding still phantom",
        "pending",
        now_unix() - 100,
    );
    let o = f.run(&["confirm", "legacy03", "--repro-test", REPRO_NO]);
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(stored_status(&f, "legacy03"), "pending", "status unchanged");
    assert!(
        f.row("legacy03").to_string().contains("\"not-reproduced\""),
        "the attempt is recorded in `repro`: {} / {}",
        f.row("legacy03"),
        o.both()
    );
}

#[test]
fn old_unconfirmed_rows_are_expiry_candidates_but_never_auto_closed() {
    let f = Fixture::new("ir-expiry");
    let now = now_unix();
    plant_task(
        &f,
        "unconfold",
        "eight day old suspicion",
        "unconfirmed",
        now - 8 * 86400,
    );
    plant_task(
        &f,
        "unconfnew",
        "two day old suspicion",
        "unconfirmed",
        now - 2 * 86400,
    );

    let rows = f.list_all();
    let get = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} not listed: {rows:?}"))
            .clone()
    };
    assert_eq!(
        get("unconfold")["expiry_candidate"],
        serde_json::json!(true),
        "{}",
        get("unconfold")
    );
    assert_eq!(
        get("unconfnew")["expiry_candidate"],
        serde_json::json!(false),
        "{}",
        get("unconfnew")
    );

    let o = f.run(&["list"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let old_line = o
        .stdout
        .lines()
        .find(|l| l.contains("unconfold"))
        .unwrap_or("");
    let new_line = o
        .stdout
        .lines()
        .find(|l| l.contains("unconfnew"))
        .unwrap_or("");
    assert!(
        old_line.contains("expiry candidate"),
        "old row flagged: {}",
        o.stdout
    );
    assert!(
        !new_line.contains("expiry candidate"),
        "young row not flagged: {}",
        o.stdout
    );
    assert!(
        o.stdout.contains("backlog cancel"),
        "unconfirmed header names the cancel exit: {}",
        o.stdout
    );

    let input = format!(
        "{{\"session_id\":\"s\",\"cwd\":\"{}\",\"hook_event_name\":\"SessionStart\"}}",
        f.repo.display()
    );
    let o = f.run_stdin(&["session-start"], &[], input.as_bytes());
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("expiry candidate"),
        "SessionStart context states the expiry candidates: {}",
        o.both()
    );

    assert_eq!(
        stored_status(&f, "unconfold"),
        "unconfirmed",
        "NO auto-close"
    );
    assert_eq!(stored_status(&f, "unconfnew"), "unconfirmed");
}
