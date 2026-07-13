//! End-to-end coverage for `condukt review-worthiness` in its primary,
//! hermetic flag-driven mode: no git repo, no state dir, no `HOME`
//! isolation needed — the whole computation is a pure function of the CLI
//! flags. This test fails before the `review-worthiness` subcommand exists
//! (unrecognized subcommand -> clap nonzero exit, no JSON `score` on
//! stdout) and passes once it is wired = a genuine Fail->Pass reproduction
//! oracle for the feature.
//!
//! Asserts the printed JSON matches the SAME pure scorer
//! (`review_worthiness::score_review_worthiness`) would produce for the
//! identical inputs, for both a high-worthiness (large net-deletion, no
//! rationale, no task link) and a low-worthiness (small, documented,
//! task-linked) case.

use std::process::Command;

fn run_worthiness(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg("review-worthiness")
        .args(args)
        .output()
        .expect("spawn condukt")
}

#[test]
fn high_worthiness_input_has_all_penalty_drivers_and_matches_pure_scorer() {
    // files=20 insertions=10 deletions=500 --no-rationale --no-task-link
    // size: total_changed=510 -> 510/20=25
    // net-deletion: 500-10=490 -> 490/10=49 -> capped at 30
    // missing-rationale: +15
    // absent-task-link: +15
    // total = 25 + 30 + 15 + 15 = 85
    let out = run_worthiness(&[
        "--files",
        "20",
        "--insertions",
        "10",
        "--deletions",
        "500",
        "--no-rationale",
        "--no-task-link",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "review-worthiness failed: {out:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON, got {e}: {stdout}"));

    assert_eq!(val["score"], 85, "unexpected score; got: {val}");
    let drivers = val["drivers"].as_array().expect("drivers array");
    assert_eq!(drivers.len(), 4, "expected all 4 drivers; got: {val}");
    assert!(drivers[0].as_str().unwrap().starts_with("size:"));
    assert!(drivers[1].as_str().unwrap().starts_with("net-deletion:"));
    assert!(drivers[2]
        .as_str()
        .unwrap()
        .starts_with("missing-rationale:"));
    assert!(drivers[3]
        .as_str()
        .unwrap()
        .starts_with("absent-task-link:"));

    assert_eq!(val["inputs"]["files_changed"], 20);
    assert_eq!(val["inputs"]["insertions"], 10);
    assert_eq!(val["inputs"]["deletions"], 500);
    assert_eq!(val["inputs"]["has_rationale"], false);
    assert_eq!(val["inputs"]["has_task_link"], false);
}

#[test]
fn low_worthiness_input_scores_low_with_no_penalty_drivers() {
    // files=1 insertions=5 deletions=3 --rationale --task-link
    // size: total_changed=8 -> 8/20=0
    // net-deletion: deletions(3) < insertions(5) -> floors at 0
    // rationale present, task link present -> no fixed penalties
    // total = 0
    let out = run_worthiness(&[
        "--files",
        "1",
        "--insertions",
        "5",
        "--deletions",
        "3",
        "--rationale",
        "--task-link",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "review-worthiness failed: {out:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON, got {e}: {stdout}"));

    assert_eq!(val["score"], 0, "unexpected score; got: {val}");
    let drivers = val["drivers"].as_array().expect("drivers array");
    assert!(
        drivers.is_empty(),
        "expected no penalty drivers; got: {val}"
    );
    assert_eq!(val["inputs"]["has_rationale"], true);
    assert_eq!(val["inputs"]["has_task_link"], true);
}

#[test]
fn human_readable_default_mentions_score_without_json_flag() {
    let out = run_worthiness(&[
        "--files",
        "1",
        "--insertions",
        "5",
        "--deletions",
        "3",
        "--rationale",
        "--task-link",
    ]);
    assert!(out.status.success(), "review-worthiness failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("review-worthiness score: 0"),
        "expected a human-readable score line; got: {stdout}"
    );
    assert!(
        stdout.contains("no penalty drivers"),
        "expected the no-drivers note; got: {stdout}"
    );
}

#[test]
fn default_flags_are_pessimistic_no_rationale_no_task_link() {
    // Neither --rationale/--no-rationale nor --task-link/--no-task-link
    // given: documented default is `false` for both (pessimistic).
    let out = run_worthiness(&[
        "--files",
        "1",
        "--insertions",
        "0",
        "--deletions",
        "0",
        "--json",
    ]);
    assert!(out.status.success(), "review-worthiness failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["inputs"]["has_rationale"], false);
    assert_eq!(val["inputs"]["has_task_link"], false);
    // missing-rationale (15) + absent-task-link (15) = 30, no size/net-deletion
    // penalty for a zero-line change.
    assert_eq!(val["score"], 30);
}

#[test]
fn conflicting_rationale_flags_are_rejected_by_clap() {
    let out = run_worthiness(&[
        "--files",
        "1",
        "--insertions",
        "0",
        "--deletions",
        "0",
        "--rationale",
        "--no-rationale",
    ]);
    assert!(
        !out.status.success(),
        "expected a nonzero exit for conflicting --rationale/--no-rationale"
    );
}
