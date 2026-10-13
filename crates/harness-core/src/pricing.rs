//! Cost estimation. A built-in table maps a model id to USD-per-1M-token input
//! and output rates; cache writes bill at 1.25x input (5-minute TTL) or 2x
//! (1-hour TTL), cache reads at 0.1x input — the standard Claude cache
//! economics. Callers can override or extend the table per model.
//!
//! Built-in rates (USD per 1M tokens, input/output):
//!   Fable / Mythos  10 / 50
//!   Opus            5 / 25
//!   Sonnet          3 / 15
//!   Haiku           1 / 5
//!
//! An unrecognized model id (no built-in family substring, no matching
//! override) has an UNKNOWN rate, not a zero one: [`rate_for`] returns
//! `Undetermined`, and so do [`cost`] / [`session_cost`] for any usage that
//! has to be priced at that rate. Model ids are external input — the day a new
//! family ships, pricing it at $0 would silently disable every budget gate that
//! reads these numbers (backlog ff98fed0). A model that is genuinely free (a
//! local model) is declared with an explicit override whose rates are `0.0`;
//! that is a `Known(0.0)`. A model with zero tokens of usage costs `Known(0.0)`
//! whatever its rate (transcripts carry `"model":"<synthetic>"` entries with
//! all-zero usage — multiplying zero tokens needs no rate).

use crate::usage::ModelUsage;
use crate::verdict::Determination;

const CACHE_WRITE_5M_MULT: f64 = 1.25;
const CACHE_WRITE_1H_MULT: f64 = 2.0;
const CACHE_READ_MULT: f64 = 0.1;

/// A per-model price override (USD per 1M input/output tokens). `pattern` is
/// matched as a substring against the lower-cased model id; the first matching
/// override wins over the built-in table. Rates of `0.0` are honoured as a
/// deliberate "this model is free" declaration.
#[derive(Debug, Clone)]
pub struct PriceOverride {
    pub pattern: String,
    pub input: f64,
    pub output: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// USD per 1M input tokens.
    pub input: f64,
    /// USD per 1M output tokens.
    pub output: f64,
}

/// The built-in table. `None` = this model id is not in the table.
fn builtin_rate(model: &str) -> Option<Rate> {
    let m = model.to_lowercase();
    if m.contains("fable") || m.contains("mythos") {
        Some(Rate {
            input: 10.0,
            output: 50.0,
        })
    } else if m.contains("opus") {
        Some(Rate {
            input: 5.0,
            output: 25.0,
        })
    } else if m.contains("sonnet") {
        Some(Rate {
            input: 3.0,
            output: 15.0,
        })
    } else if m.contains("haiku") {
        Some(Rate {
            input: 1.0,
            output: 5.0,
        })
    } else {
        None
    }
}

/// Resolve the rate for a model: the first matching override (substring), else
/// the built-in table, else `Undetermined` — an unrecognized model's rate is
/// unknown, never `0`.
pub fn rate_for(model: &str, overrides: &[PriceOverride]) -> Determination<Rate> {
    let m = model.to_lowercase();
    for o in overrides {
        if m.contains(&o.pattern) {
            return Determination::Known(Rate {
                input: o.input,
                output: o.output,
            });
        }
    }
    match builtin_rate(model) {
        Some(r) => Determination::Known(r),
        None => Determination::undetermined(format!(
            "no price for model `{model}`: it is not in the built-in table and no \
             pricing override matches it, so its cost is unknown (not $0); add an \
             override for it (input/output = 0.0 if it is genuinely free)"
        )),
    }
}

/// Estimated USD cost of one model's usage within a session. `Undetermined`
/// when the usage is nonzero and the model's rate is unknown (see
/// [`rate_for`]); zero usage is `Known(0.0)` for any model.
pub fn cost(model: &str, u: &ModelUsage, overrides: &[PriceOverride]) -> Determination<f64> {
    if u.total_tokens() == 0 {
        return Determination::Known(0.0);
    }
    rate_for(model, overrides).map(|r| {
        let per_token_in = r.input / 1_000_000.0;
        let per_token_out = r.output / 1_000_000.0;
        u.input as f64 * per_token_in
            + u.output as f64 * per_token_out
            + u.cache_write_5m as f64 * per_token_in * CACHE_WRITE_5M_MULT
            + u.cache_write_1h as f64 * per_token_in * CACHE_WRITE_1H_MULT
            + u.cache_read as f64 * per_token_in * CACHE_READ_MULT
    })
}

/// Total USD cost across a session's per-model tallies. `Undetermined` as soon
/// as any one model's cost is (a partial sum would be an under-count of unknown
/// size); the first such model's reason is forwarded.
pub fn session_cost<'a, I>(models: I, overrides: &[PriceOverride]) -> Determination<f64>
where
    I: IntoIterator<Item = (&'a String, &'a ModelUsage)>,
{
    let mut total = 0.0;
    for (m, u) in models {
        match cost(m, u, overrides) {
            Determination::Known(c) => total += c,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
    }
    Determination::Known(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::Required;

    fn known(d: Determination<f64>) -> f64 {
        d.require()
            .expect("cost must be determined in this fixture")
    }

    #[test]
    fn opus_input_output() {
        let u = ModelUsage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        // 5 + 25
        assert!((known(cost("claude-opus-4-8", &u, &[])) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn cache_pricing_writes_and_reads() {
        // cache_write_5m at 1.25x input, cache_write_1h at 2x, cache_read at 0.1x,
        // all against the Opus input rate of 5/1M.
        let u = ModelUsage {
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            cache_read: 1_000_000,
            ..Default::default()
        };
        // 5*1.25 + 5*2.0 + 5*0.1 = 6.25 + 10.0 + 0.5 = 16.75
        assert!((known(cost("claude-opus-4-8", &u, &[])) - 16.75).abs() < 1e-9);
    }

    #[test]
    fn cache_read_is_cheap() {
        let u = ModelUsage {
            cache_read: 1_000_000,
            ..Default::default()
        };
        // 5 * 0.1
        assert!((known(cost("claude-opus-4-8", &u, &[])) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn substring_override_wins() {
        let ov = vec![PriceOverride {
            pattern: "opus".into(),
            input: 1.0,
            output: 1.0,
        }];
        let u = ModelUsage {
            input: 1_000_000,
            ..Default::default()
        };
        assert!((known(cost("claude-opus-4-8", &u, &ov)) - 1.0).abs() < 1e-9);
    }

    // Inverted from the former `unknown_model_is_free`, which asserted
    // `cost("some-local-model", 1M input, no overrides) == 0.0` and so froze the
    // fail-open as spec (backlog ff98fed0). An unrecognized model's cost is
    // unknown; "free" must be declared with an explicit override (next test).
    #[test]
    fn unknown_model_is_not_free() {
        let u = ModelUsage {
            input: 1_000_000,
            ..Default::default()
        };
        assert!(
            matches!(
                cost("some-local-model", &u, &[]),
                Determination::Undetermined(_)
            ),
            "an unrecognized model with usage must be Undetermined, not $0"
        );
        assert!(matches!(
            rate_for("some-local-model", &[]),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn explicit_zero_override_is_known_free() {
        let ov = vec![PriceOverride {
            pattern: "some-local".into(),
            input: 0.0,
            output: 0.0,
        }];
        let u = ModelUsage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        assert_eq!(known(cost("some-local-model", &u, &ov)), 0.0);
    }

    #[test]
    fn zero_usage_of_unknown_model_is_known_zero() {
        // Real transcripts carry `"model":"<synthetic>"` turns with all-zero usage.
        assert_eq!(known(cost("<synthetic>", &ModelUsage::default(), &[])), 0.0);
    }

    #[test]
    fn session_cost_with_one_unknown_model_is_undetermined() {
        use std::collections::BTreeMap;
        let mut models: BTreeMap<String, ModelUsage> = BTreeMap::new();
        let u = ModelUsage {
            input: 1_000_000,
            ..Default::default()
        };
        models.insert("claude-opus-4-8".into(), u.clone());
        models.insert("claude-newfamily-1".into(), u);
        match session_cost(models.iter(), &[]).require() {
            Required::Blocked(why) => assert!(why.as_str().contains("claude-newfamily-1")),
            Required::Determined(c) => panic!("partial sum {c} reported as the session cost"),
        }
    }

    #[test]
    fn session_cost_sums_models() {
        use std::collections::BTreeMap;
        let mut models: BTreeMap<String, ModelUsage> = BTreeMap::new();
        models.insert(
            "claude-opus-4-8".into(),
            ModelUsage {
                input: 1_000_000,
                ..Default::default()
            },
        );
        models.insert(
            "claude-haiku-4-5".into(),
            ModelUsage {
                output: 1_000_000,
                ..Default::default()
            },
        );
        // opus input 5 + haiku output 5 = 10
        assert!((known(session_cost(models.iter(), &[])) - 10.0).abs() < 1e-9);
    }
}
