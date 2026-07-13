//! Stop hook: read the transcript, compute cost, check against limits.
//!
//! A budget violation emits `{"decision":"block","reason":"…"}` so Claude
//! receives the overage notice and can wind down gracefully. A warn-only
//! crossing emits `{"additionalContext":"…"}` (advisory, no block). Harness
//! errors always exit 0 and allow the stop.

use harness_core::estimate_transcript_cost;
use harness_core::pricing;
use harness_core::session::{self, SessionRecord};
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
    // Cost source: prefer gauge's persisted canonical SessionRecord (avoids a
    // full transcript re-parse on every Stop) — but ONLY when it's fresh enough
    // to cover the current turn. budgetguard is a budget GATE, not a passive
    // recorder: gauge's record can lag by one turn if this Stop hook runs
    // before gauge's, and under-counting the current turn would let a turn
    // slip over budget. So a stale/missing/empty record falls back to the
    // accurate, timely `estimate_transcript_cost` (the pre-existing behavior).
    let session_usd = session_cost(
        cfg,
        &session::default_state_dir(),
        session_id,
        transcript_path,
    )?;

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

/// Resolve this session's USD cost. Prefers gauge's persisted canonical
/// `SessionRecord` (looked up under `gauge_state_dir` — gauge's own store, NOT
/// `cfg.state_dir`, which is budgetguard's ledger dir) when it is fresh enough
/// to cover the current turn (see [`record_is_fresh`]); otherwise falls back
/// to a full transcript re-parse via [`estimate_transcript_cost`]. `None`
/// propagates the same data-error fail-soft behavior `estimate_transcript_cost`
/// already had (empty/unreadable transcript => allow the stop).
///
/// `gauge_state_dir` is a parameter (rather than calling
/// `session::default_state_dir()` directly) so tests can point it at a
/// tempdir instead of the real `~/.gauge/store`; `evaluate` always passes the
/// real default.
fn session_cost(
    cfg: &Config,
    gauge_state_dir: &std::path::Path,
    session_id: &str,
    transcript_path: &str,
) -> Option<f64> {
    if let Some(rec) = session::load_one(gauge_state_dir, session_id) {
        if !rec.models.is_empty() && record_is_fresh(&rec, transcript_path) {
            return Some(pricing::session_cost(
                rec.models.iter(),
                &cfg.price_overrides,
            ));
        }
    }
    Some(estimate_transcript_cost(transcript_path, &cfg.price_overrides)?.cost_usd)
}

/// A gauge record "covers the current turn" when it was written at/after the
/// transcript file's last modification — i.e. gauge's Stop hook already ran
/// (and persisted) for this same turn. If the record predates the transcript's
/// mtime (or either timestamp is unavailable/unparseable), treat it as stale:
/// the record may lag by one turn behind an in-flight write, and a budget gate
/// must never under-count the current turn.
fn record_is_fresh(rec: &SessionRecord, transcript_path: &str) -> bool {
    let Some(mtime) = std::fs::metadata(transcript_path)
        .ok()
        .and_then(|m| m.modified().ok())
    else {
        return false;
    };
    let Some(updated_at) = chrono::DateTime::parse_from_rfc3339(&rec.updated_at).ok() else {
        return false;
    };
    let updated_at: std::time::SystemTime = updated_at.into();
    updated_at >= mtime
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

    // --- gauge SessionRecord source + freshness guard -----------------------

    use harness_core::usage::ModelUsage;
    use std::collections::BTreeMap;

    /// Write a one-model transcript (opus, 1M in + 1M out => $30.00 USD) to a
    /// temp file and return its path. Every test below shares this fixture so
    /// the "same cost" assertions have a single ground truth.
    fn write_transcript(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "budgetguard-gate-{}-{name}.jsonl",
            std::process::id()
        ));
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":1000000,"output_tokens":1000000,"cache_read_input_tokens":0}}}"#,
            "\n",
        );
        std::fs::write(&p, body).unwrap();
        p
    }

    fn opus_models(input: u64, output: u64) -> BTreeMap<String, ModelUsage> {
        let mut m = BTreeMap::new();
        m.insert(
            "claude-opus-4-8".to_string(),
            ModelUsage {
                input,
                output,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: 0,
            },
        );
        m
    }

    fn sample_record(updated_at: &str) -> SessionRecord {
        SessionRecord {
            session_id: "sess-fresh".to_string(),
            project: "harness".to_string(),
            cwd: "/cwd".to_string(),
            models: opus_models(1_000_000, 1_000_000),
            turns: 1,
            tools: BTreeMap::new(),
            first_ts: None,
            last_ts: None,
            agents: BTreeMap::new(),
            updated_at: updated_at.to_string(),
        }
    }

    /// (a) A fresh record (updated_at at/after the transcript's mtime) yields
    /// the SAME cost `estimate_transcript_cost` would compute from the raw
    /// transcript — the whole point of preferring the canon is that it must
    /// not change the number.
    #[cfg(unix)]
    #[test]
    fn fresh_record_matches_transcript_parse_cost() {
        let tp = write_transcript("fresh-match");
        let store = tempfile::tempdir().unwrap();

        // updated_at far in the future relative to the transcript's mtime =>
        // unambiguously fresh regardless of filesystem timestamp resolution.
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let rec = sample_record(&future);
        session::upsert(store.path(), &rec);

        let cfg = Config::default();
        let via_record = session_cost(&cfg, store.path(), "sess-fresh", tp.to_str().unwrap())
            .expect("fresh record path");
        let via_parse = estimate_transcript_cost(tp.to_str().unwrap(), &cfg.price_overrides)
            .expect("transcript parse")
            .cost_usd;
        assert!(
            (via_record - via_parse).abs() < 1e-9,
            "record={via_record} parse={via_parse}"
        );
        assert!((via_record - 30.0).abs() < 1e-9, "{via_record}");

        let _ = std::fs::remove_file(&tp);
    }

    /// (b) A stale record (updated_at BEFORE the transcript's mtime — i.e.
    /// gauge's write predates the current turn) must NOT be trusted; the gate
    /// falls back to the transcript parse instead of under-counting.
    #[cfg(unix)]
    #[test]
    fn stale_record_falls_back_to_transcript_parse() {
        let tp = write_transcript("stale-fallback");
        let store = tempfile::tempdir().unwrap();

        // updated_at far in the past relative to the transcript's mtime =>
        // unambiguously stale.
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let rec = sample_record(&past);
        session::upsert(store.path(), &rec);

        let cfg = Config::default();
        let cost = session_cost(&cfg, store.path(), "sess-fresh", tp.to_str().unwrap())
            .expect("fallback path");
        // Falls back to the transcript (same fixture => same $30.00), proving
        // the stale record's numbers were NOT used blindly (record_is_fresh
        // gated it out even though it happens to carry identical usage here).
        assert!((cost - 30.0).abs() < 1e-9, "{cost}");
        assert!(!record_is_fresh(&rec, tp.to_str().unwrap()));

        let _ = std::fs::remove_file(&tp);
    }

    /// (b, continued) Missing record (no gauge store / no record for this
    /// session id) also falls back cleanly.
    #[test]
    fn missing_record_falls_back_to_transcript_parse() {
        let tp = write_transcript("missing-fallback");
        let store = tempfile::tempdir().unwrap(); // empty — no record written

        let cfg = Config::default();
        let cost = session_cost(&cfg, store.path(), "no-such-session", tp.to_str().unwrap())
            .expect("fallback path");
        assert!((cost - 30.0).abs() < 1e-9, "{cost}");

        let _ = std::fs::remove_file(&tp);
    }

    /// An empty-models record (e.g. a legacy/degenerate record) is also
    /// treated as unusable and falls back, even if nominally "fresh".
    #[cfg(unix)]
    #[test]
    fn empty_models_record_falls_back_even_if_fresh() {
        let tp = write_transcript("empty-models");
        let store = tempfile::tempdir().unwrap();

        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let mut rec = sample_record(&future);
        rec.models = BTreeMap::new();
        session::upsert(store.path(), &rec);

        let cfg = Config::default();
        let cost = session_cost(&cfg, store.path(), "sess-fresh", tp.to_str().unwrap())
            .expect("fallback path");
        assert!((cost - 30.0).abs() < 1e-9, "{cost}");

        let _ = std::fs::remove_file(&tp);
    }

    /// (c) Enforcement (verdict + day ledger) is unchanged by the cost source:
    /// `evaluate` still blocks/warns/allows purely off the resulting
    /// `session_usd`/`day_usd` numbers, regardless of whether they came from
    /// the record or the transcript. This exercises the full `evaluate` path
    /// (ledger update included) with a fresh record supplying the cost.
    #[cfg(unix)]
    #[test]
    fn evaluate_enforcement_unaffected_by_record_source() {
        let tp = write_transcript("enforcement");
        let store = tempfile::tempdir().unwrap();
        let ledger_dir = tempfile::tempdir().unwrap();

        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let mut rec = sample_record(&future);
        rec.session_id = "sess-enforce".to_string();
        session::upsert(store.path(), &rec);

        // session_block_usd well below the $30.00 fixture cost => must block,
        // exactly as it would if the cost had come from a transcript parse.
        let cfg = Config {
            session_block_usd: 10.0,
            state_dir: ledger_dir.path().to_path_buf(),
            ..Config::default()
        };

        let session_usd =
            session_cost(&cfg, store.path(), "sess-enforce", tp.to_str().unwrap()).expect("cost");
        assert!((session_usd - 30.0).abs() < 1e-9);
        assert!(matches!(
            verdict(&cfg, session_usd, session_usd),
            Verdict::Block(_)
        ));

        let _ = std::fs::remove_file(&tp);
    }
}
