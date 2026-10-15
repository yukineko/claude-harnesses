//! H4 (independent verifier): `backlog add` WITHOUT `--repro-test` must file no
//! task, exit 2, and append a record to `.backlog/unproven.toml`. This checks
//! that the "considered-unproven" recording mechanism actually works.
mod common;
use common::*;

#[test]
fn add_without_repro_test_appends_unproven_record_and_files_no_task() {
    let f = Fixture::new("h4-unproven");
    let title = "h4 finding without repro";
    let p = f.project();
    let unproven = f.tasks_path().with_file_name("unproven.toml");
    assert!(!unproven.exists(), "precondition: no unproven.toml yet");

    let o = f.run(&["add", "--title", title, "--project", &p]);
    assert_eq!(o.code, 2, "no --repro-test => exit 2: {}", o.both());
    assert!(
        f.list_all().is_empty(),
        "no task may be filed: {:?}",
        f.list_all()
    );
    let body = std::fs::read_to_string(&unproven).unwrap_or_else(|e| {
        panic!(
            "unproven.toml must exist after a refused add ({e}); out: {}",
            o.both()
        )
    });
    assert!(
        body.contains(title),
        "unproven.toml must contain the refused title; got:\n{body}"
    );
    let v: toml::Value = toml::from_str(&body).expect("unproven.toml parses");
    let n = v
        .get("record")
        .and_then(|r| r.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(n, 1, "exactly one record appended:\n{body}");
}
