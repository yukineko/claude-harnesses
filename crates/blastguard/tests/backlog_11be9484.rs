//! backlog 11be9484: `blastguard::classify::classify` runs the deploy needles
//! and `detect::detect` over a task's free text (title + done_criteria +
//! touched files, exactly as condukt's `schedule::task_action_text` joins
//! them), so a FILE NAME containing a deploy needle, or prose that merely
//! DESCRIBES a command, force-gates a task that performs no such action.
//!
//! Observed 2026-10-03 (flow batch 13): 73c2c089 stayed force-gated because
//! its touched file is `scripts/check-plugin-rollout.py` (needle `rollout`),
//! e033c406 because its prose quoted `git config core.hooksPath`.
//!
//! Each RED test is paired with a control that drops only the offending
//! fragment, so the fragment alone is shown to flip the verdict.
//! Measured while writing this: the prose case flips only when the command is
//! backtick-quoted (as markdown-ish task prose usually quotes it); the same
//! words without backticks were not gated.
//!
//! Repro: cargo test -p blastguard --test backlog_11be9484 -- --ignored

use blastguard::classify::classify;

/// The same join condukt's `task_action_text` performs.
fn action_text(title: &str, done: &str, files: &[&str]) -> String {
    let mut s = format!("{title} {done}");
    for f in files {
        s.push(' ');
        s.push_str(f);
    }
    s
}

const TITLE_A: &str = "check script exits 0 when the registry is absent";
const DONE_A: &str = "exit non-zero when the registry file is missing; add a regression test";

#[test]
fn control_same_task_without_the_file_name_is_not_gated() {
    let a = classify(&action_text(
        TITLE_A,
        DONE_A,
        &["scripts/check-plugin-cache.py"],
    ));
    assert!(!a.requires_gate(), "control must not be gated, got {a:?}");
}

#[test]
fn file_name_containing_a_deploy_needle_does_not_force_gate() {
    let a = classify(&action_text(
        TITLE_A,
        DONE_A,
        &["scripts/check-plugin-rollout.py"],
    ));
    assert!(
        !a.requires_gate(),
        "a touched file NAMED check-plugin-rollout.py force-gated a task that rolls nothing out: {a:?}"
    );
}

const TITLE_B: &str = "new gate: detect a retry of a blocked command through another syntax";

#[test]
fn control_prose_without_a_quoted_command_is_not_gated() {
    let a = classify(&action_text(
        TITLE_B,
        "block the retry and report the original block reason",
        &["scripts/check-retry.py"],
    ));
    assert!(!a.requires_gate(), "control must not be gated, got {a:?}");
}

#[test]
#[ignore = "backlog 11be9484 prose half: out of scope by user ruling 2026-10-16"]
fn prose_describing_a_command_does_not_force_gate() {
    let a = classify(&action_text(
        TITLE_B,
        "block the retry when it re-runs `git config core.hooksPath /dev/null` after a block, \
         and report the original block reason",
        &["scripts/check-retry.py"],
    ));
    assert!(
        !a.requires_gate(),
        "prose that DESCRIBES `git config core.hooksPath` force-gated a task that runs no command: {a:?}"
    );
}
