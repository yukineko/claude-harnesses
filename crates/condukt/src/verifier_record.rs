//! The verifier's verdict for a task, recorded in run state (backlog
//! ea6547c9: before this, the verdict survived only in the agent transcript;
//! `state set` stored just `verifier_model` / `verifier_agent_id`).
//!
//! Flattened into `state::TaskState`, so `state show` renders the keys
//! `verifier_verdict` / `verifier_confidence` / `verifier_reason` at the task
//! level. Recorded verbatim as supplied; condukt's own gates never consult it
//! (the `status` transition remains the gate input).

use serde::{Deserialize, Serialize};

/// `state set --verifier-verdict/--verifier-confidence/--verifier-reason`.
#[derive(clap::Args, Debug, Default, Clone)]
pub struct VerdictArgs {
    /// The verifier's verdict for this task (e.g. `pass` / `fail`), recorded
    /// verbatim as `verifier_verdict`. Omitted leaves it unchanged.
    #[arg(long)]
    pub verifier_verdict: Option<String>,
    /// The verifier's stated confidence (e.g. `low` / `medium` / `high`),
    /// recorded verbatim as `verifier_confidence`. Omitted leaves it unchanged.
    #[arg(long)]
    pub verifier_confidence: Option<String>,
    /// The verifier's reason for its verdict, recorded verbatim as
    /// `verifier_reason`. Omitted leaves it unchanged.
    #[arg(long)]
    pub verifier_reason: Option<String>,
}

/// The stored verdict. Each field is `None` when never supplied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierVerdict {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_reason: Option<String>,
}

impl VerifierVerdict {
    /// Overwrite each field that `args` supplies; leave the rest unchanged.
    pub fn apply(&mut self, args: VerdictArgs) {
        if args.verifier_verdict.is_some() {
            self.verifier_verdict = args.verifier_verdict;
        }
        if args.verifier_confidence.is_some() {
            self.verifier_confidence = args.verifier_confidence;
        }
        if args.verifier_reason.is_some() {
            self.verifier_reason = args.verifier_reason;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overwrites_only_supplied_fields() {
        let mut v = VerifierVerdict {
            verifier_verdict: Some("fail".into()),
            verifier_confidence: Some("low".into()),
            verifier_reason: Some("r0".into()),
        };
        v.apply(VerdictArgs {
            verifier_verdict: Some("pass".into()),
            ..Default::default()
        });
        assert_eq!(v.verifier_verdict.as_deref(), Some("pass"));
        assert_eq!(v.verifier_confidence.as_deref(), Some("low"));
        assert_eq!(v.verifier_reason.as_deref(), Some("r0"));
    }
}
