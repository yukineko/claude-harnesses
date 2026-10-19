//! Controls for backlog dc955812 (user ruling 2026-10-20): task prose is no
//! longer parsed as a command, and the commands a task runs are classified
//! separately. These pin the restrictive halves of that change; the RED/GREEN
//! repro itself is `tests/backlog_11be9484.rs::prose_describing_a_command_does_not_force_gate`
//! (kept in its own file so it still compiles at the pre-fix revision).

use blastguard::classify::{classify_change, classify_command};
use blastguard::diffrisk::SensitiveConfig;
use harness_core::verdict::Determination;

const TITLE_B: &str = "new gate: detect a retry of a blocked command through another syntax";

/// The very command the repro's prose describes, passed as a COMMAND the task
/// will run, is still force-gated — through both the single-command entry and
/// `classify_change` (condukt's call), under benign prose.
#[test]
fn control_the_described_command_passed_as_a_command_still_force_gates() {
    let cmd = "git config core.hooksPath /dev/null";
    let a = classify_command(cmd);
    assert!(a.requires_gate(), "{cmd:?} as a command must gate: {a:?}");

    let cmds = [cmd.to_string()];
    let d = classify_change(
        &format!(
            "{TITLE_B} block the retry and report the original block reason scripts/check-retry.py"
        ),
        Some(&cmds),
        &["scripts/check-retry.py".to_string()],
        "",
        &SensitiveConfig::default(),
    );
    assert!(
        matches!(&d, Determination::Known(a) if a.requires_gate()),
        "declared {cmd:?} must classify (not undetermined) and force-gate: {d:?}"
    );
}

/// FAIL-CLOSED: a task that does not declare its commands is NOT classified
/// from its prose — the answer is undetermined, which condukt resolves to the
/// restricted side, never Low.
#[test]
fn control_undeclared_commands_are_undetermined_not_low() {
    let d = classify_change(TITLE_B, None, &[], "", &SensitiveConfig::default());
    assert!(
        matches!(d, Determination::Undetermined(_)),
        "undeclared commands must be undetermined"
    );
}
