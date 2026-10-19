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
//! The prose half was fixed under backlog dc955812 (user ruling 2026-10-20):
//! `classify` now reads prose only; the commands a task runs go to
//! `classify_command` / `classify_change(.., Some(commands), ..)` separately.
//!
//! Repro: cargo test -p blastguard --test backlog_11be9484

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

/// backlog dc955812 (user ruling 2026-10-20): prose is no longer parsed as a
/// command; the commands a task runs are classified separately.
#[test]
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

/// Control for dc955812: the very command the prose above describes, passed
/// as a COMMAND the task will run, is still force-gated — through both the
/// single-command entry and `classify_change` (condukt's call), under the
/// same benign prose.
#[test]
fn control_the_described_command_passed_as_a_command_still_force_gates() {
    use blastguard::classify::{classify_change, classify_command};
    use blastguard::diffrisk::SensitiveConfig;
    let cmd = "git config core.hooksPath /dev/null";
    let a = classify_command(cmd);
    assert!(a.requires_gate(), "{cmd:?} as a command must gate: {a:?}");

    let cmds = [cmd.to_string()];
    let d = classify_change(
        &action_text(
            TITLE_B,
            "block the retry and report the original block reason",
            &["scripts/check-retry.py"],
        ),
        Some(&cmds),
        &["scripts/check-retry.py".to_string()],
        "",
        &SensitiveConfig::default(),
    );
    assert!(
        matches!(&d, harness_core::verdict::Determination::Known(a) if a.requires_gate()),
        "declared {cmd:?} must classify (not undetermined) and force-gate: {d:?}"
    );
}

/// Control for dc955812 (fail-closed): a task that does not declare its
/// commands is NOT classified from its prose — the answer is undetermined,
/// which condukt resolves to the restricted side, never Low.
#[test]
fn control_undeclared_commands_are_undetermined_not_low() {
    let d = blastguard::classify::classify_change(
        TITLE_B,
        None,
        &[],
        "",
        &blastguard::diffrisk::SensitiveConfig::default(),
    );
    assert!(
        matches!(d, harness_core::verdict::Determination::Undetermined(_)),
        "undeclared commands must be undetermined"
    );
}
