//! User ruling 2026-10-15: unknown fields in backlog store rows fail closed.
//! A row in tasks.toml or tasks.done.toml carrying a field the row types do
//! not declare (removed legacy `ruling_kind` / `rationale` / `untestable_reason`,
//! a `[task.closure.ruling]` table, or any arbitrary unknown key on the task,
//! closure, repro, green or red tables) must make the store fail to load:
//! `list` and `next` exit non-zero, the store bytes are unchanged, and the
//! error names the offending row id. Written by an independent test writer
//! (CLAUDE.md section 2(a)).
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

const GREEN: &str = "[task.closure.green]\nrunner = \"cargo\"\ncmd = \"cargo test\"\nexit = 0\npassed = 1\nrev = \"0123456789012345678901234567890123456789\"\nobserved_at = 1\noutput_digest = \"d\"\nexcerpt = \"ok\"\n";
const RED: &str = "[task.closure.red]\nrev = \"0123456789012345678901234567890123456789\"\nexit = 101\nkind = \"behavioural\"\n";
const REPRO: &str = "[task.repro]\noutcome = \"reproduced\"\ncmd = \"cargo test\"\nobserved_at = 1\ndetail = \"d\"\n";

fn assert_fail_closed(f: &Fixture, id: &str, what: &str) {
    let before_t = f.store_bytes();
    let before_d = std::fs::read(f.done_path()).ok();
    for args in [vec!["list", "--all"], vec!["next"]] {
        let o = f.run(&args);
        assert_ne!(
            o.code,
            0,
            "{what}: `{args:?}` must exit non-zero: {}",
            o.both()
        );
        assert!(
            o.both().contains(id),
            "{what}: `{args:?}` must name row id {id}: {}",
            o.both()
        );
    }
    assert_eq!(
        f.store_bytes(),
        before_t,
        "{what}: tasks.toml bytes changed"
    );
    assert_eq!(
        std::fs::read(f.done_path()).ok(),
        before_d,
        "{what}: tasks.done.toml bytes changed"
    );
}

#[test]
fn legacy_task_level_field_fails_closed() {
    let f = Fixture::new("uf-legacy");
    let p = f.project();
    append(
        &f.tasks_path(),
        &raw_row(
            "legacy01",
            "pending",
            &p,
            "ruling_kind = \"judgment\"\nrationale = \"x\"\nuntestable_reason = \"y\"",
        ),
    );
    assert_fail_closed(
        &f,
        "legacy01",
        "legacy ruling_kind/rationale/untestable_reason",
    );
}

#[test]
fn closure_ruling_table_in_done_file_fails_closed() {
    let f = Fixture::new("uf-ruling");
    let p = f.project();
    let extra = "[task.closure]\nreason = \"ruling\"\n[task.closure.ruling]\nkind = \"judgment\"\nrationale = \"v\"\napproved_by = \"yuki\"\napproved_at = 1\napproved_via = \"tty\"\n";
    append(&f.done_path(), &raw_row("ruled001", "done", &p, extra));
    assert_fail_closed(&f, "ruled001", "[task.closure.ruling] in done file");
}

#[test]
fn unknown_key_in_repro_fails_closed() {
    let f = Fixture::new("uf-repro");
    let p = f.project();
    let extra = format!("{REPRO}bogus_repro_key = 1\n");
    append(&f.tasks_path(), &raw_row("repro001", "pending", &p, &extra));
    assert_fail_closed(&f, "repro001", "unknown key in [task.repro]");
}

#[test]
fn unknown_key_in_closure_green_fails_closed() {
    let f = Fixture::new("uf-green");
    let p = f.project();
    let extra = format!("[task.closure]\nreason = \"f2p\"\n{GREEN}bogus_green_key = 1\n{RED}");
    append(&f.done_path(), &raw_row("green001", "done", &p, &extra));
    assert_fail_closed(&f, "green001", "unknown key in [task.closure.green]");
}

#[test]
fn arbitrary_unknown_task_key_fails_closed() {
    let f = Fixture::new("uf-task");
    let p = f.project();
    append(
        &f.tasks_path(),
        &raw_row("taskkey1", "pending", &p, "totally_unknown_key = \"x\""),
    );
    assert_fail_closed(&f, "taskkey1", "arbitrary unknown task key");
}

#[test]
fn positive_control_declared_fields_only_loads() {
    let f = Fixture::new("uf-ok");
    let p = f.project();
    append(&f.tasks_path(), &raw_row("okopen01", "pending", &p, REPRO));
    let extra = format!("[task.closure]\nreason = \"f2p\"\n{GREEN}{RED}");
    append(&f.done_path(), &raw_row("okdone01", "done", &p, &extra));
    let o = f.run(&["list", "--all"]);
    assert_eq!(o.code, 0, "declared-only store must load: {}", o.both());
    assert!(
        o.stdout.contains("okopen01") && o.stdout.contains("okdone01"),
        "{}",
        o.both()
    );
    let o = f.run(&["next"]);
    assert_eq!(
        o.code,
        0,
        "declared-only store `next` must succeed: {}",
        o.both()
    );
}
