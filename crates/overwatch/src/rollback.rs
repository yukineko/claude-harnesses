/// Canary rollback events — the persisted record of a canary health-gate
/// rollback advisory/execution.
///
/// The canary gate/rollback DECISION logic lives in `canary.rs` (pure) and is
/// consumed by `scripts/rollout-plugins.sh`. That path *decides and executes* a
/// rollback but historically wrote nothing to any store overwatch could read
/// back — the verdict was printed and acted on, then lost. This module adds the
/// missing observational record: when a rollback is advised/executed, a
/// [`RollbackEvent`] is appended to a readable JSONL stream so the unified
/// review surface (`overwatch review-queue`) can show it alongside systemic
/// violations and AI-review findings.
///
/// Everything here is data + pure construction. Emission is fail-soft (the
/// caller must never break a rollout to log): see `store::append_rollback` and
/// `rollback_cli::record`.
use serde::{Deserialize, Serialize};

/// Why the canary gate advised a rollback: it exceeded the tolerated threshold
/// counting either *raw* violations in the window, or only *systemic* recurring
/// signatures. Mirrors the `--systemic` flag on `canary-gate`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RollbackReason {
    /// The gate counted raw violations within the window.
    Raw,
    /// The gate counted only systemic (cross-task recurring) signatures.
    Systemic,
}

impl RollbackReason {
    /// Stable lowercase token.
    pub fn token(self) -> &'static str {
        match self {
            RollbackReason::Raw => "raw",
            RollbackReason::Systemic => "systemic",
        }
    }

    /// Parse a reason token back from its string form. Unknown tokens map to
    /// `Raw` (the conservative default) rather than erroring, since this is an
    /// observational record and a bad token must not drop the whole event.
    pub fn parse_lenient(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "systemic" => RollbackReason::Systemic,
            _ => RollbackReason::Raw,
        }
    }
}

/// A single recorded canary rollback event.
///
/// One event per plugin that was rolled back in a stage (the shell emits one
/// per restored plugin), so the review surface can attribute a rollback to a
/// specific plugin/version transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackEvent {
    /// The plugin that was rolled back.
    pub plugin: String,
    /// The version that was live BEFORE the canary (what the rollback restored
    /// to). `None` when the plugin was newly introduced by the canary and there
    /// was nothing to restore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The canary version that was rolled back FROM (the version the stage
    /// moved the plugin to before the gate tripped).
    pub to_version: String,
    /// The 0-based canary stage index the rollback halted at.
    pub stage: usize,
    /// Why the gate advised the rollback (raw vs systemic count).
    pub reason: RollbackReason,
    /// Unix timestamp when the rollback was recorded.
    pub ts: i64,
    /// Optional free-text detail for the audit trail (e.g. observed count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RollbackEvent {
    /// Construct a rollback event. `from_version` is `None` for a plugin the
    /// canary newly introduced (nothing to restore).
    pub fn new(
        plugin: String,
        from_version: Option<String>,
        to_version: String,
        stage: usize,
        reason: RollbackReason,
        ts: i64,
        detail: Option<String>,
    ) -> Self {
        Self {
            plugin,
            from_version,
            to_version,
            stage,
            reason,
            ts,
            detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_token_round_trips() {
        assert_eq!(
            RollbackReason::parse_lenient(RollbackReason::Raw.token()),
            RollbackReason::Raw
        );
        assert_eq!(
            RollbackReason::parse_lenient(RollbackReason::Systemic.token()),
            RollbackReason::Systemic
        );
    }

    #[test]
    fn reason_parse_is_lenient_and_defaults_to_raw() {
        assert_eq!(
            RollbackReason::parse_lenient("SYSTEMIC"),
            RollbackReason::Systemic
        );
        assert_eq!(
            RollbackReason::parse_lenient(" systemic "),
            RollbackReason::Systemic
        );
        // Unknown / blank tokens degrade to Raw rather than erroring.
        assert_eq!(RollbackReason::parse_lenient("bogus"), RollbackReason::Raw);
        assert_eq!(RollbackReason::parse_lenient(""), RollbackReason::Raw);
    }

    #[test]
    fn rollback_event_serializes_with_kind_free_shape() {
        let ev = RollbackEvent::new(
            "overwatch".to_string(),
            Some("0.1.7".to_string()),
            "0.1.8".to_string(),
            2,
            RollbackReason::Systemic,
            1000,
            Some("observed=5".to_string()),
        );
        let json = serde_json::to_string(&ev).unwrap();
        let back: RollbackEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
        assert_eq!(back.from_version.as_deref(), Some("0.1.7"));
        assert_eq!(back.reason, RollbackReason::Systemic);
    }

    #[test]
    fn rollback_event_omits_none_from_version() {
        let ev = RollbackEvent::new(
            "newplugin".to_string(),
            None,
            "0.1.0".to_string(),
            0,
            RollbackReason::Raw,
            42,
            None,
        );
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("from_version"));
        assert!(!json.contains("detail"));
        let back: RollbackEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.from_version, None);
    }
}
