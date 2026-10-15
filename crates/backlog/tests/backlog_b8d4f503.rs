//! backlog b8d4f503: a RED test run whose output exceeds the OS pipe buffer
//! (~64 KiB) must still be carried to a conclusion. The runner reads the
//! child's stdout/stderr only after the child exits, so a verbose RED child
//! blocks in write() until the timeout and the run is recorded Undetermined.
//!
//! Driven through `add --repro-test` (the same repro runner the removed
//! `confirm` verb used): an Undetermined run makes `add` refuse and write
//! nothing, so the defect shows as a refused add.
mod common;
use common::*;
use std::time::Instant;

fn add_with_repro(f: &Fixture, title: &str, cmd: &str) -> (Out, Option<String>) {
    let p = f.project();
    let o = f.run_env(
        &[
            "add",
            "--title",
            title,
            "--project",
            &p,
            "--repro-test",
            cmd,
        ],
        &[("BACKLOG_TEST_TIMEOUT_SECS", "20")],
    );
    let id = o
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .map(|s| s.trim().to_string());
    (o, id)
}

/// Writes ~256 KiB to stderr, then fails (= reproduces, like a RED test).
const VERBOSE_RED: &str = "head -c 262144 /dev/zero | tr '\\0' x >&2\nexit 1";

#[test]
fn control_quiet_red_repro_is_reproduced() {
    let f = Fixture::new("b8d4f503-ctl");
    let (o, id) = add_with_repro(&f, "quiet red", REPRO_YES);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.unwrap_or_else(|| panic!("no `added:` line: {}", o.both()));
    assert_eq!(f.status(&id), "pending", "{}", f.row(&id));
}

#[test]
#[ignore = "backlog b8d4f503: RED repro"]
fn verbose_red_repro_is_reproduced_not_timed_out() {
    let f = Fixture::new("b8d4f503");
    f.write_script("tests/verbose_red.sh", VERBOSE_RED);
    f.commit_paths(&["tests/verbose_red.sh"], "verbose red repro");
    let started = Instant::now();
    let (o, id) = add_with_repro(&f, "verbose red", "bash tests/verbose_red.sh");
    let took = started.elapsed();
    assert_eq!(
        o.code,
        0,
        "a RED run writing 256 KiB to stderr must be read to a conclusion, \
         not left blocked in write() until the timeout (took {took:?}): {}",
        o.both()
    );
    let id = id.unwrap_or_else(|| panic!("no `added:` line: {}", o.both()));
    assert_eq!(f.status(&id), "pending", "{}\n{}", o.both(), f.row(&id));
}
