//! Independent adversarial tests for `harness_core::relay::post_run_cwds` and
//! `mark_executed` (backlog be745685, ruled contract rule (i)).
//!
//! Contract: a relay is marked executed only when the command string matches
//! exactly AND the payload cwd equals the queued cwd or a resolved target of a
//! LITERAL `cd` of the command, resolved in sequence from the queued cwd. A
//! `cd` whose target is not literal contributes nothing, and so does every
//! later `cd`. When in doubt: do not mark executed.
//!
//! Written by a test author who did not write the implementation. Each test
//! asserts only what the contract states.

use harness_core::relay::{mark_executed, post_run_cwds, queue};
use std::path::{Path, PathBuf};

const Q: &str = "/q/w";

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

// ---------------------------------------------------------------- positives

#[test]
fn absolute_literal_cd_yields_that_absolute_dir() {
    assert_eq!(post_run_cwds(Q, "cd /abs && make"), s(&["/abs"]));
}

#[test]
fn relative_literal_cd_yields_queued_cwd_joined_with_target() {
    assert_eq!(post_run_cwds(Q, "cd sub && make"), s(&["/q/w/sub"]));
}

#[test]
fn two_literal_cds_resolve_in_sequence_from_the_previous_target() {
    assert_eq!(post_run_cwds(Q, "cd a && cd b"), s(&["/q/w/a", "/q/w/a/b"]));
}

#[test]
fn dotdot_is_folded_against_the_queued_cwd() {
    assert_eq!(post_run_cwds(Q, "cd .. && make"), s(&["/q"]));
}

#[test]
fn dotdot_inside_a_relative_target_is_folded() {
    assert_eq!(post_run_cwds(Q, "cd a/../b && make"), s(&["/q/w/b"]));
}

#[test]
fn absolute_cd_after_relative_cd_replaces_the_base() {
    assert_eq!(
        post_run_cwds(Q, "cd a && cd /abs && cd c"),
        s(&["/q/w/a", "/abs", "/abs/c"])
    );
}

// ---------------------------------------------------------------- negatives

#[test]
fn cd_of_dollar_variable_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd $HOME && make").is_empty());
}

#[test]
fn cd_of_quoted_variable_contributes_nothing() {
    assert!(post_run_cwds(Q, r#"cd "$X" && make"#).is_empty());
}

#[test]
fn cd_of_command_substitution_prefix_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd $(pwd)/x && make").is_empty());
}

#[test]
fn cd_of_backtick_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd `x` && make").is_empty());
}

#[test]
fn cd_of_bare_tilde_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd ~ && make").is_empty());
}

#[test]
fn cd_of_tilde_path_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd ~/x && make").is_empty());
}

#[test]
fn cd_of_glob_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd * && make").is_empty());
}

#[test]
fn literal_relative_cd_after_a_non_literal_cd_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd $X && cd sub && make").is_empty());
}

#[test]
fn literal_absolute_cd_after_a_non_literal_cd_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd $X && cd /abs && make").is_empty());
}

#[test]
fn literal_cd_before_a_non_literal_cd_still_contributes_but_nothing_after_it() {
    assert_eq!(post_run_cwds(Q, "cd a && cd $X && cd b"), s(&["/q/w/a"]));
}

#[test]
fn cd_inside_a_subshell_contributes_nothing() {
    assert!(post_run_cwds(Q, "(cd /abs && make)").is_empty());
}

#[test]
fn cd_piped_to_another_command_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd /abs | cat").is_empty());
}

#[test]
fn cd_dash_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd - && make").is_empty());
}

#[test]
fn cd_without_operand_contributes_nothing() {
    assert!(post_run_cwds(Q, "cd && make").is_empty());
}

#[test]
fn cd_as_an_argument_of_another_command_contributes_nothing() {
    assert!(post_run_cwds(Q, "echo cd /abs").is_empty());
}

#[test]
fn quoted_cd_command_as_one_word_contributes_nothing() {
    assert!(post_run_cwds(Q, r#""cd /abs""#).is_empty());
}

#[test]
fn command_without_cd_contributes_nothing() {
    assert!(post_run_cwds(Q, "make test").is_empty());
}

// ---------------------------------------------------- mark_executed (public)

fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::Builder::new()
        .prefix(&format!("t4-pcwd-{name}-"))
        .tempdir()
        .unwrap();
    // Session transcript `<d>/t/s.jsonl` with subagent `a1`.
    let main = d.path().join("t").join("s.jsonl");
    let sub = d.path().join("t").join("s").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(&main, "{}\n").unwrap();
    std::fs::write(sub.join("agent-a1.jsonl"), "{}\n").unwrap();
    (d, main)
}

fn run(dir: &Path, t: &Path, cwd: &str, cmd: &str) -> Option<String> {
    mark_executed(dir, "s", Some(t), cwd, cmd, Some(0)).unwrap()
}

#[test]
fn mark_executed_marks_matching_command_at_post_cd_cwd() {
    let (d, t) = fixture("pos");
    let cmd = "cd sub && make";
    let id = queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    assert_eq!(run(d.path(), &t, "/q/w/sub", cmd), Some(id));
}

#[test]
fn mark_executed_does_not_mark_a_different_command_at_the_post_cd_cwd() {
    let (d, t) = fixture("othercmd");
    let cmd = "cd sub && make";
    queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    assert_eq!(run(d.path(), &t, "/q/w/sub", "cd sub && make clean"), None);
    assert_eq!(run(d.path(), &t, "/q/w/sub", "make"), None);
}

#[test]
fn mark_executed_does_not_mark_an_unrelated_cwd() {
    let (d, t) = fixture("unrelated");
    let cmd = "cd sub && make";
    queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    assert_eq!(run(d.path(), &t, "/some/random/dir", cmd), None);
}

#[test]
fn mark_executed_does_not_mark_the_expansion_of_a_non_literal_cd() {
    let (d, t) = fixture("nonlit");
    let cmd = "cd $HOME && make";
    queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/nobody".to_string());
    assert_eq!(run(d.path(), &t, &home, cmd), None);
    assert_eq!(run(d.path(), &t, "/anything", cmd), None);
}

#[test]
fn mark_executed_does_not_mark_a_literal_cd_after_a_non_literal_one() {
    let (d, t) = fixture("afternonlit");
    let cmd = "cd $X && cd sub && make";
    queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    assert_eq!(run(d.path(), &t, "/q/w/sub", cmd), None);
}

#[test]
fn mark_executed_still_marks_the_unchanged_queued_cwd() {
    let (d, t) = fixture("queuedcwd");
    let cmd = "cd $X && make";
    let id = queue(d.path(), "s", Some("a1"), Q, cmd, "r").unwrap();
    assert_eq!(run(d.path(), &t, Q, cmd), Some(id));
}
