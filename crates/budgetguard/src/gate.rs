//! Stop hook: read the transcript, compute cost, check against limits.
//!
//! A budget violation emits `{"decision":"block","reason":"…"}` so Claude
//! receives the overage notice and can wind down gracefully. A warn-only
//! crossing emits `{"additionalContext":"…"}` (advisory, no block). Harness
//! errors always exit 0 and allow the stop.

use harness_core::estimate_transcript_cost;
use serde_json::json;

use crate::config::Config;
use harness_core::ledger::Ledger;

pub struct GateResult {
    pub session_usd: f64,
    pub day_usd: f64,
    pub verdict: Verdict,
}

pub enum Verdict {
    Allow,
    Warn(String),
    Block(String),
}

/// Run the budget gate. Returns a GateResult (or None on data errors).
pub fn evaluate(
    cfg: &Config,
    session_id: &str,
    transcript_path: &str,
    today: &str,
) -> Option<GateResult> {
    // Aggregate this session's transcript to compute USD cost, via the single
    // shared transcript→cost estimator (usage::aggregate + pricing::session_cost).
    let session_usd = estimate_transcript_cost(transcript_path, &cfg.price_overrides)?.cost_usd;

    // Update the daily ledger with this session's latest cost. Serialize the
    // whole load → record → save against other concurrent sessions so a
    // simultaneous Stop can't clobber our entry (lost update).
    let _guard = crate::lock::LedgerLock::acquire(&cfg.state_dir);
    let day_usd = match Ledger::load_checked(&cfg.state_dir) {
        Ok(mut ledger) => {
            let day_usd = ledger.record(session_id, today, session_usd);
            let _ = ledger.save(&cfg.state_dir);
            day_usd
        }
        Err(_corrupt) => {
            // The on-disk ledger is unparseable. Do NOT overwrite it (that would
            // erase the day's accumulated spend and fail the budget open). Leave
            // the file untouched and fall back to this session's own cost as the
            // day total — conservative: never under-reports below this session.
            eprintln!(
                "budgetguard: ledger.json is corrupt; preserving it and skipping \
                 update (day total falls back to this session's cost)"
            );
            session_usd
        }
    };
    drop(_guard);

    let verdict = verdict(cfg, session_usd, day_usd);
    Some(GateResult {
        session_usd,
        day_usd,
        verdict,
    })
}

/// Deterministic "budget pressure" signal for downstream cost-aware routing
/// (consumed by `fugu-router` via `budgetguard status --json`). True once the
/// day's spend has reached the daily warn threshold — the same point at which
/// the gate starts warning. A non-positive threshold means "unset" → no
/// pressure (parity with the gate, which treats `0.0` limits as disabled).
pub fn budget_pressure(day_usd: f64, daily_warn_usd: f64) -> bool {
    daily_warn_usd > 0.0 && day_usd >= daily_warn_usd
}

fn verdict(cfg: &Config, session_usd: f64, day_usd: f64) -> Verdict {
    // Check block limits first (higher priority than warn).
    if cfg.session_block_usd > 0.0 && session_usd >= cfg.session_block_usd {
        return Verdict::Block(format!(
            "budgetguard: セッション予算超過 ${:.4} / ${:.2} (上限)。\n\
             作業を保存し、コミットして終了してください。",
            session_usd, cfg.session_block_usd
        ));
    }
    if cfg.daily_block_usd > 0.0 && day_usd >= cfg.daily_block_usd {
        return Verdict::Block(format!(
            "budgetguard: 日次予算超過 ${:.4} / ${:.2} (上限)。\n\
             作業を保存し、コミットして終了してください。",
            day_usd, cfg.daily_block_usd
        ));
    }

    // Warn limits.
    let mut warns = Vec::new();
    if cfg.session_warn_usd > 0.0 && session_usd >= cfg.session_warn_usd {
        warns.push(format!(
            "セッション費用 ${:.4} が警告閾値 ${:.2} を超えています",
            session_usd, cfg.session_warn_usd
        ));
    }
    if cfg.daily_warn_usd > 0.0 && day_usd >= cfg.daily_warn_usd {
        warns.push(format!(
            "本日累計 ${:.4} が警告閾値 ${:.2} を超えています",
            day_usd, cfg.daily_warn_usd
        ));
    }

    if warns.is_empty() {
        Verdict::Allow
    } else {
        Verdict::Warn(format!("⚠ budgetguard:\n{}", warns.join("\n")))
    }
}

/// Emit the Stop hook output and exit.
pub fn emit_and_exit(result: Option<GateResult>) -> ! {
    match result {
        None => {
            // Data error or no transcript yet — allow silently.
            std::process::exit(0);
        }
        Some(r) => {
            // Running totals to stderr (operator-visible log; never touches the
            // stdout JSON the hook protocol parses).
            eprintln!(
                "budgetguard: session ${:.4} / day ${:.4}",
                r.session_usd, r.day_usd
            );
            match r.verdict {
                Verdict::Allow => std::process::exit(0),
                Verdict::Warn(msg) => {
                    println!("{}", json!({ "additionalContext": msg }));
                    std::process::exit(0);
                }
                Verdict::Block(reason) => {
                    println!("{}", json!({ "decision": "block", "reason": reason }));
                    std::process::exit(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn budget_pressure_tracks_warn_threshold() {
        // Below warn => no pressure; at/over warn => pressure.
        assert!(!budget_pressure(4.0, 5.0));
        assert!(budget_pressure(5.0, 5.0));
        assert!(budget_pressure(9.0, 5.0));
        // Unset (non-positive) threshold => never pressure, matching the gate.
        assert!(!budget_pressure(100.0, 0.0));
    }

    fn cfg(sw: f64, sb: f64, dw: f64, db: f64) -> Config {
        Config {
            session_warn_usd: sw,
            session_block_usd: sb,
            daily_warn_usd: dw,
            daily_block_usd: db,
            ..Config::default()
        }
    }

    #[test]
    fn allow_below_all_thresholds() {
        let c = cfg(1.0, 2.0, 5.0, 10.0);
        assert!(matches!(verdict(&c, 0.5, 0.5), Verdict::Allow));
    }

    #[test]
    fn warn_is_inclusive_at_threshold() {
        let c = cfg(1.0, 2.0, 5.0, 10.0);
        // session cost exactly at the warn threshold (>=) warns but doesn't block.
        assert!(matches!(verdict(&c, 1.0, 1.0), Verdict::Warn(_)));
    }

    #[test]
    fn block_is_inclusive_at_threshold_and_beats_warn() {
        let c = cfg(1.0, 2.0, 5.0, 10.0);
        // session cost exactly at the block threshold blocks (>=), not just warns.
        assert!(matches!(verdict(&c, 2.0, 2.0), Verdict::Block(_)));
    }

    #[test]
    fn daily_block_triggers_independently_of_session() {
        let c = cfg(0.0, 0.0, 0.0, 10.0); // only a daily block configured
        assert!(matches!(verdict(&c, 0.01, 10.0), Verdict::Block(_)));
        assert!(matches!(verdict(&c, 0.01, 9.99), Verdict::Allow));
    }

    #[test]
    fn zero_threshold_means_disabled() {
        let c = cfg(0.0, 0.0, 0.0, 0.0);
        // Even a large cost is allowed when every limit is 0 (disabled).
        assert!(matches!(verdict(&c, 999.0, 999.0), Verdict::Allow));
    }
}
