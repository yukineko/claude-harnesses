//! Backlog defect (2026-11-27, main 3dc21b1b9): re-failing an already-failed
//! task is an "idempotent no-op" in `store::mark_failed`, so an elapsed
//! `defer_until` is never refreshed, while `Command::Fail` still prints
//! "deferred until <dt> (2 日後に再実行されます)". `next --claim` then hands
//! the same task out again immediately.
//!
//! Passes under either fix: exit 0 => stored defer_until is in the future and
//! `next` no longer hands the task out; non-zero exit => the output says the
//! deferral was not refreshed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::Fixture;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Rewrite `defer_until` of row `id` in `tasks.toml` to `ts`.
fn set_defer(f: &Fixture, id: &str, ts: i64) {
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    let mut v: toml::Value = toml::from_str(&text).unwrap();
    let row = v["task"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id} not in tasks.toml"));
    row.as_table_mut()
        .unwrap()
        .insert("defer_until".into(), toml::Value::Integer(ts));
    std::fs::write(f.tasks_path(), toml::to_string(&v).unwrap()).unwrap();
}

#[test]
#[ignore = "RED: re-fail of an already-failed task leaves an elapsed defer_until (prints '2 日後') — see notes"]
fn refail_of_already_failed_task_refreshes_elapsed_defer_until() {
    let f = Fixture::new("refail-defer");
    let id = f.add("task failed twice");

    // Fail once, then push the deferral into the past.
    let o = f.run(&["fail", &id, "--reason", "first"]);
    assert_eq!(o.code, 0, "first fail: {}", o.both());
    assert_eq!(f.row(&id)["status"], "deferred", "fixture: {}", f.row(&id));
    set_defer(&f, &id, now_unix() - 200_000);
    assert_eq!(f.status(&id), "failed", "fixture: elapsed deferral => failed");

    // Control: the failed row with an elapsed deferral is handed out.
    let o = f.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "control next --claim: {}", o.both());
    assert!(
        o.stdout.contains(&id),
        "control: elapsed-defer failed task must be handed out (fixture broken otherwise): {}",
        o.both()
    );

    // Fail it again.
    let o = f.run(&["fail", &id, "--reason", "SECOND-REASON"]);
    let now = now_unix();
    let defer = f.row(&id)["defer_until"].as_i64();
    let out = o.both();

    if o.code == 0 {
        // Message and stored value must agree.
        let defer = defer.unwrap_or(0);
        assert!(
            defer >= now + 3600,
            "exit 0 (output: {out}) but stored defer_until={defer} is not in the future (now={now}); \
             the command claims a 2-day deferral that was never stored"
        );
        let o = f.run(&["next"]);
        assert!(
            !o.stdout.contains(&id),
            "after a successful re-fail the task must not be handed out again: {}",
            o.both()
        );
        // Notes coverage: the reason should be appended.
        let notes = f.row(&id)["notes"].as_str().unwrap_or("").to_string();
        assert!(
            notes.contains("SECOND-REASON"),
            "re-fail reason must be appended to notes: {notes:?}"
        );
    } else {
        assert!(
            out.contains("not") || out.contains("未") || out.contains("refresh") || out.contains("更新"),
            "non-zero exit must say the deferral was not refreshed: {out}"
        );
    }
}
