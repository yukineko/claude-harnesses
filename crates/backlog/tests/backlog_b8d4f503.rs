//! backlog b8d4f503: a RED test run whose output exceeds the OS pipe buffer
//! (~64 KiB) must still be carried to a conclusion. The runner reads the
//! child's stdout/stderr only after the child exits, so a verbose RED child
//! blocks in write() until the timeout and the run is recorded Undetermined.
mod common;
use common::*;
use std::time::Instant;

fn has_outcome(row: &serde_json::Value, outcome: &str) -> bool {
    row.to_string().contains(&format!("\"{outcome}\""))
}

/// Writes ~256 KiB to stderr, then fails (= reproduces, like a RED test).
const VERBOSE_RED: &str = "head -c 262144 /dev/zero | tr '\\0' x >&2\nexit 1";

#[test]
fn control_quiet_red_confirm_is_reproduced() {
    let f = Fixture::new("b8d4f503-ctl");
    let (o, id) = f.add_extra("quiet red", &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.unwrap();
    let o = f.run_env(
        &["confirm", &id, "--repro-test", REPRO_YES],
        &[("BACKLOG_TEST_TIMEOUT_SECS", "20")],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending", "{}", f.row(&id));
}

#[test]
#[ignore = "backlog b8d4f503: RED repro"]
fn verbose_red_confirm_is_reproduced_not_timed_out() {
    let f = Fixture::new("b8d4f503");
    f.write_script("tests/verbose_red.sh", VERBOSE_RED);
    f.commit_paths(&["tests/verbose_red.sh"], "verbose red repro");
    let (o, id) = f.add_extra("verbose red", &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.unwrap();
    let started = Instant::now();
    let o = f.run_env(
        &["confirm", &id, "--repro-test", "bash tests/verbose_red.sh"],
        &[("BACKLOG_TEST_TIMEOUT_SECS", "20")],
    );
    let took = started.elapsed();
    let row = f.row(&id);
    assert!(
        !has_outcome(&row, "undetermined"),
        "a RED run writing 256 KiB to stderr must be read to a conclusion, \
         not left blocked in write() until the timeout (took {took:?}): {}\n{row}",
        o.both()
    );
    assert_eq!(f.status(&id), "pending", "{}\n{row}", o.both());
}
