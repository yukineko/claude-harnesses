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
use crate::verdict::Determination;

/// A transcript's aggregated usage plus its estimated USD cost, computed in one
/// pass. `aggregate` is the full per-model / per-agent tally (identical to what
/// [`usage::aggregate`] returns); `cost_usd` is [`pricing::session_cost`] over
/// its grand-total `models` — `Undetermined` when a model with usage has no
/// known rate (see [`pricing::rate_for`]).
///
/// `cost_usd` alone still **cannot express "the sub-agent spend is missing"**.
/// When the transcript's sibling `subagents/` directory could not be read,
/// `aggregate.subagent_scan` is `Undetermined` and a `Known` `cost_usd` omits
/// that sub-agent spend. A consumer whose answer depends on the number being
/// complete (a budget gate; a persisted canonical record) must read it through
/// [`TranscriptCostEstimate::complete_cost`], which folds both give-ups.
#[derive(Debug)]
pub struct TranscriptCostEstimate {
    pub aggregate: Aggregate,
    pub cost_usd: Determination<f64>,
}

impl TranscriptCostEstimate {
    /// The cost, but only when every sub-agent spend source was actually read
    /// AND every model with usage had a known rate. `Undetermined` (carrying
    /// the sub-agent scan's reason first, else the pricing reason) when the
    /// number would be an under-count of unknown size.
    pub fn complete_cost(&self) -> Determination<f64> {
        match &self.aggregate.subagent_scan {
            Determination::Known(()) => self.cost_usd.clone(),
            Determination::Undetermined(why) => Determination::Undetermined(why.clone()),
        }
    }
}

/// Aggregate the session transcript at `path` and price it with `overrides` in a
/// single call. Returns `None` on exactly the same conditions as
/// [`usage::aggregate`] (empty/unreadable path, or a transcript with no turns
/// and no tool calls) — fail-soft, never breaks the turn.
///
/// Note that "could not read this session's sub-agent spend" is **not** one of
/// those `None` conditions: it comes back as a `Some` whose
/// [`complete_cost`](TranscriptCostEstimate::complete_cost) is `Undetermined`.
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
        assert_eq!(est.cost_usd, expected);
        let got = est.complete_cost().require().expect("opus is priced");
        assert!((got - 30.0).abs() < 1e-9);
        // aggregate is preserved intact.
        assert_eq!(est.aggregate.turns, 1);
        assert!(est.aggregate.models.contains_key("claude-opus-4-8"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_model_makes_complete_cost_undetermined() {
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-newfamily-1","content":[],"usage":{"input_tokens":1000,"output_tokens":0}}}"#,
            "\n",
        );
        let path = write_temp("unknown-model", body);
        let est = estimate_transcript_cost(path.to_str().unwrap(), &[]).expect("estimate");
        assert!(matches!(
            est.complete_cost(),
            Determination::Undetermined(_)
        ));
        // The aggregate is still produced: the tokens were observed, only their price is unknown.
        assert!(est.aggregate.models.contains_key("claude-newfamily-1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn none_on_empty_or_missing_matches_aggregate() {
        assert!(estimate_transcript_cost("", &[]).is_none());
        assert!(estimate_transcript_cost("/no/such/transcript.jsonl", &[]).is_none());
    }
}
