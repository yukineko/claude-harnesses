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
//! The same rule applies once budgetguard *is* located: a spawn error, a
//! non-zero exit, stdout that is not JSON, or JSON without a boolean `pressure`
//! key (budgetguard deliberately omits that key when it cannot determine the
//! pressure itself) all mean the checker did not answer. A checker that did not
//! answer is not a checker that said "no pressure", so each of these resolves to
//! `true` with the reason on stderr. Only an explicit `"pressure": false`
//! from a successful run routes unchanged.
//! Read-only — never writes.

use std::process::Command;

use harness_core::plugin_bin;
use harness_core::verdict::Determination;
use serde::Deserialize;

/// `pressure` is required: a missing key is budgetguard saying "undetermined",
/// never "no pressure" (no `#[serde(default)]`).
#[derive(Debug, Deserialize)]
struct Status {
    pressure: bool,
}

/// True iff budgetguard reports the day's spend has reached the warn threshold,
/// OR budgetguard did not give a usable answer (lookup undetermined, spawn
/// error, non-zero exit, unparseable stdout or no `pressure` key) — the
/// restrictive side, with the reason printed on stderr. `false` only when
/// budgetguard is observed absent (soft dep → routing unchanged) or a
/// successful run explicitly reports `"pressure": false`.
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
    match query(&binary) {
        Ok(pressure) => pressure,
        Err(why) => {
            eprintln!(
                "fugu-router: budgetguard ({}) gave no usable pressure answer ({why}); treating the budget as under pressure",
                binary.display()
            );
            true
        }
    }
}

/// Run `budgetguard status --json`. `Err` = no usable answer (spawn error,
/// non-zero exit, unparseable stdout); the caller resolves it restrictively.
fn query(binary: &std::path::Path) -> Result<bool, String> {
    let out = Command::new(binary)
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("exited {}: {}", out.status, stderr.trim()));
    }
    parse_pressure(&out.stdout)
}

/// Parse `budgetguard status --json` stdout → pressure bit. Split out so the
/// decode is unit-testable without a real budgetguard binary. `Err` when the
/// stdout is not JSON or lacks a boolean `pressure` key — never a silent
/// `false`.
fn parse_pressure(stdout: &[u8]) -> Result<bool, String> {
    serde_json::from_slice::<Status>(stdout)
        .map(|s| s.pressure)
        .map_err(|e| format!("unparseable status --json output: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pressure_true() {
        assert_eq!(
            parse_pressure(
                br#"{"day_usd":9.0,"daily_warn_usd":5.0,"daily_block_usd":0.0,"pressure":true}"#
            ),
            Ok(true)
        );
    }

    #[test]
    fn parses_pressure_false() {
        assert_eq!(parse_pressure(br#"{"pressure":false}"#), Ok(false));
    }

    #[test]
    fn missing_pressure_key_is_undetermined() {
        // budgetguard omits `pressure` when it could not determine it
        // (`{"error":..,"undetermined":true}`); that must not read as `false`.
        assert!(parse_pressure(br#"{"day_usd":1.0}"#).is_err());
        assert!(parse_pressure(br#"{"error":"x","undetermined":true}"#).is_err());
        assert!(parse_pressure(br#"{"pressure":"no"}"#).is_err());
    }

    #[test]
    fn unparseable_is_undetermined() {
        assert!(parse_pressure(b"budgetguard: human line").is_err());
        assert!(parse_pressure(b"").is_err());
    }
}
