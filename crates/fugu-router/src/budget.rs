//! Read-only budget-pressure check.
//!
//! budgetguard is a post-hoc Stop gate: it only reacts *after* a turn has
//! already spent. fugu-router picks models *before* the spend. When the day's
//! budget is under pressure (spend has reached the daily warn threshold), the
//! router should bias cheaper — shaving a tier and suppressing opus escalation —
//! so the remaining budget isn't burned on an opus×N fan-out.
//!
//! This asks budgetguard for its deterministic pressure verdict
//! (`budgetguard status --json`). Soft dependency: if budgetguard is observed
//! to be **not installed** (`plugin_bin::resolve` → `Known(None)`), we return
//! `false` and routing is unchanged — there is no budget to protect.
//!
//! Locating budgetguard goes through `harness_core::plugin_bin::resolve`
//! (plugin cache first, numeric version order, `$PATH` second). When that
//! lookup is **undetermined** (the cache dir exists but could not be read, a
//! `$PATH` entry exists but cannot be spawned, …) we could not tell whether a
//! budget is being enforced. "No pressure" is the permissive side of this check
//! (no downgrade, full-price models), so an undetermined lookup resolves to
//! `true` — pressured, routing downgrades one tier — and the reason is printed
//! on stderr (CLAUDE.md §3: cannot-determine resolves to the restrictive side).
//!
//! Not changed here (pre-existing, still soft): once budgetguard *is* located,
//! a spawn error, a non-zero exit or unparseable stdout still reads as `false`.
//! Read-only — never writes.

use std::process::Command;

use harness_core::plugin_bin;
use harness_core::verdict::Determination;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Status {
    #[serde(default)]
    pressure: bool,
}

/// True iff budgetguard reports the day's spend has reached the warn threshold,
/// OR budgetguard's presence could not be determined (restrictive side; the
/// reason is printed on stderr). `false` when budgetguard is observed absent
/// (soft dep → routing unchanged) or, once located, errors / emits garbage.
pub fn under_pressure() -> bool {
    let binary = match plugin_bin::resolve("budgetguard") {
        Determination::Known(Some(p)) => p,
        // Observed: budgetguard is not installed here ⇒ no budget to protect.
        Determination::Known(None) => return false,
        // Could not tell whether a budget is enforced ⇒ do NOT fall to the
        // permissive "no pressure" answer; downgrade and say why.
        Determination::Undetermined(why) => {
            eprintln!(
                "fugu-router: could not locate budgetguard ({why}); treating the budget as under pressure"
            );
            return true;
        }
    };
    let Ok(out) = Command::new(&binary).args(["status", "--json"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    parse_pressure(&out.stdout)
}

/// Parse `budgetguard status --json` stdout → pressure bit. Split out so the
/// decode is unit-testable without a real budgetguard binary.
fn parse_pressure(stdout: &[u8]) -> bool {
    serde_json::from_slice::<Status>(stdout)
        .map(|s| s.pressure)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pressure_true() {
        assert!(parse_pressure(
            br#"{"day_usd":9.0,"daily_warn_usd":5.0,"daily_block_usd":0.0,"pressure":true}"#
        ));
    }

    #[test]
    fn parses_pressure_false() {
        assert!(!parse_pressure(br#"{"pressure":false}"#));
        // Missing field defaults to no pressure.
        assert!(!parse_pressure(br#"{"day_usd":1.0}"#));
    }

    #[test]
    fn unparseable_is_no_pressure() {
        assert!(!parse_pressure(b"budgetguard: human line"));
        assert!(!parse_pressure(b""));
    }
}
