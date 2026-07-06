//! REMOVE-GATE execute-vs-escalate: deterministic "may this gated task
//! auto-execute?" decision core.
//!
//! When condukt is about to execute a task that carries a gate, it must decide
//! whether the action is safe enough to run autonomously or whether it must
//! escalate to a human. This module is the deterministic core consulted before
//! executing a gated task: given the action's graded risk
//! ([`blastguard::classify::Risk`]), whether it is reversible, and whether the
//! prevailing run policy is `auto`, it returns a two-state verdict. The rule is
//! deliberately conservative — only the clearly-safe corner (Low risk AND
//! reversible AND policy auto) auto-executes; every irreversible or high-risk
//! gated action always escalates.
//!
//! Purity guarantee (mirrors [`crate::run_policy::decide_run_policy`] and
//! [`crate::circuit::decide_circuit`]): no filesystem, no `std::time`, no env,
//! no LLM. Same inputs always yield the same output, and it never panics.

// This pure core is not yet wired to a subcommand: the `condukt gate check`
// consumer is the intentional downstream task (mirrors circuit.rs before it was
// consumed).
#![allow(dead_code)]

use blastguard::classify::Risk;

/// The two-state verdict emitted by [`decide_gate_exec`]: run the gated task
/// autonomously, or escalate it to a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateExec {
    /// The action is clearly safe — auto-execute without human sign-off.
    AutoExec,
    /// The action is risky, irreversible, or policy-gated — escalate to a human.
    Escalate,
}

/// Decide whether a gated task may auto-execute or must escalate.
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Never
/// panics. Returns [`GateExec::AutoExec`] **if and only if** ALL THREE hold:
/// `risk` is [`Risk::Low`] AND `reversible == true` AND `policy_is_auto ==
/// true`. In every other case — Medium/High risk, not reversible, or a policy
/// that is not auto — it returns [`GateExec::Escalate`]. Conservative by design:
/// only the clearly-safe corner auto-execs.
pub fn decide_gate_exec(risk: Risk, reversible: bool, policy_is_auto: bool) -> GateExec {
    if matches!(risk, Risk::Low) && reversible && policy_is_auto {
        GateExec::AutoExec
    } else {
        GateExec::Escalate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_exec_low_reversible_auto_autoexecs() {
        assert_eq!(
            decide_gate_exec(Risk::Low, true, true),
            GateExec::AutoExec,
            "the only safe corner (Low + reversible + auto) must auto-exec"
        );
    }

    #[test]
    fn gate_exec_low_reversible_not_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Low, true, false),
            GateExec::Escalate,
            "policy not auto must escalate even when Low + reversible"
        );
    }

    #[test]
    fn gate_exec_low_not_reversible_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Low, false, true),
            GateExec::Escalate,
            "not reversible must escalate even when Low + auto"
        );
    }

    #[test]
    fn gate_exec_medium_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::Medium, true, true),
            GateExec::Escalate,
            "Medium risk must escalate"
        );
    }

    #[test]
    fn gate_exec_high_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::High, true, true),
            GateExec::Escalate,
            "High risk must escalate"
        );
    }

    #[test]
    fn gate_exec_high_not_reversible_auto_escalates() {
        assert_eq!(
            decide_gate_exec(Risk::High, false, true),
            GateExec::Escalate,
            "High + irreversible must escalate"
        );
    }

    #[test]
    fn gate_exec_is_deterministic() {
        let v1 = decide_gate_exec(Risk::Low, true, true);
        let v2 = decide_gate_exec(Risk::Low, true, true);
        assert_eq!(v1, v2, "same inputs must yield the same verdict");
    }
}
