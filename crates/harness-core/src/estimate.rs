//! Bundled transcript → (usage, cost) estimator.
//!
//! One entry point that folds [`usage::aggregate`] (per-model token tallies from
//! a session's JSONL transcript) and [`pricing::session_cost`] (USD from those
//! tallies) into a single call, so every consumer derives cost from a transcript
//! the same way instead of hand-rolling the aggregate-then-price pair.
//!
//! This is purely additive glue over the two building blocks; it changes no
//! number they produce. `usage::aggregate` and `pricing::session_cost` remain
//! the internal primitives (and stay directly usable for the models-only path,
//! e.g. pricing an already-persisted [`crate::session::SessionRecord`]).

use crate::pricing::{self, PriceOverride};
use crate::usage::{self, Aggregate};

/// A transcript's aggregated usage plus its estimated USD cost, computed in one
/// pass. `aggregate` is the full per-model / per-agent tally (identical to what
/// [`usage::aggregate`] returns); `cost_usd` is [`pricing::session_cost`] over
/// its grand-total `models`.
#[derive(Debug)]
pub struct TranscriptCostEstimate {
    pub aggregate: Aggregate,
    pub cost_usd: f64,
}

/// Aggregate the session transcript at `path` and price it with `overrides` in a
/// single call. Returns `None` on exactly the same conditions as
/// [`usage::aggregate`] (empty/unreadable path, or a transcript with no turns
/// and no tool calls) — fail-soft, never breaks the turn.
pub fn estimate_transcript_cost(
    path: &str,
    overrides: &[PriceOverride],
) -> Option<TranscriptCostEstimate> {
    let aggregate = usage::aggregate(path)?;
    let cost_usd = pricing::session_cost(aggregate.models.iter(), overrides);
    Some(TranscriptCostEstimate {
        aggregate,
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "harness-core-estimate-{}-{name}.jsonl",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn bundles_aggregate_and_cost_identically_to_building_blocks() {
        // opus 1M in + 1M out = $5 + $25 = $30.00.
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":1000000,"output_tokens":1000000}}}"#,
            "\n",
        );
        let path = write_temp("bundle", body);
        let p = path.to_str().unwrap();

        let est = estimate_transcript_cost(p, &[]).expect("estimate");
        // cost matches the hand-rolled aggregate-then-price sequence.
        let agg = usage::aggregate(p).unwrap();
        let expected = pricing::session_cost(agg.models.iter(), &[]);
        assert!((est.cost_usd - expected).abs() < 1e-9);
        assert!((est.cost_usd - 30.0).abs() < 1e-9);
        // aggregate is preserved intact.
        assert_eq!(est.aggregate.turns, 1);
        assert!(est.aggregate.models.contains_key("claude-opus-4-8"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn none_on_empty_or_missing_matches_aggregate() {
        assert!(estimate_transcript_cost("", &[]).is_none());
        assert!(estimate_transcript_cost("/no/such/transcript.jsonl", &[]).is_none());
    }
}
