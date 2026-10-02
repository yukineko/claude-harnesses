//! Roll session records up into a human-facing report: overall totals plus
//! breakdowns by project, model, and day. Cost is recomputed from stored token
//! counts on every report, so editing the pricing table re-prices history.

use std::collections::BTreeMap;

use crate::config::PriceOverride;
use crate::store::{SessionRecord, Usage};
use harness_core::pricing;

/// Format an integer with thousands separators.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Format a USD amount, widening precision for small sums.
pub fn money(c: f64) -> String {
    let c = if c.abs() < 1e-9 { 0.0 } else { c };
    if c >= 1.0 {
        format!("${c:.2}")
    } else {
        format!("${c:.4}")
    }
}

/// Compact token count: 12.3k / 4.56M.
pub fn tokens_short(n: u64) -> String {
    let f = n as f64;
    if f >= 1_000_000.0 {
        format!("{:.2}M", f / 1_000_000.0)
    } else if f >= 1_000.0 {
        format!("{:.1}k", f / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Cache hit rate: cached input tokens read ÷ total input tokens (fresh +
/// cached). Returns 0.0 when there is no input at all, so a fresh/empty
/// session reads as 0% rather than dividing by zero.
pub fn cache_hit_rate(u: &Usage) -> f64 {
    let denom = u.input + u.cache_read;
    if denom == 0 {
        0.0
    } else {
        u.cache_read as f64 / denom as f64
    }
}

/// Total cache *write* tokens (both 5m and 1h TTL buckets).
pub fn cache_write(u: &Usage) -> u64 {
    u.cache_write_5m + u.cache_write_1h
}

/// Format a 0.0–1.0 fraction as a whole-percent string like `73%`.
pub fn pct(f: f64) -> String {
    format!("{:.0}%", f * 100.0)
}

fn record_cost(rec: &SessionRecord, overrides: &[PriceOverride]) -> f64 {
    rec.models
        .iter()
        .map(|(m, u)| pricing::cost(m, u, overrides))
        .sum()
}

/// Render the full report. `records` is consumed read-only.
pub fn render(records: &[SessionRecord], overrides: &[PriceOverride]) -> String {
    if records.is_empty() {
        return "no sessions recorded yet. Run some turns, then `gauge report`.".to_string();
    }

    let mut total_cost = 0.0;
    let mut total_tokens = 0u64;
    let mut total_turns = 0u64;

    // project -> (cost, tokens, sessions)
    let mut by_project: BTreeMap<String, (f64, u64, u64)> = BTreeMap::new();
    // model -> (cost, usage)
    let mut by_model: BTreeMap<String, (f64, Usage)> = BTreeMap::new();
    // day -> (cost, tokens)
    let mut by_day: BTreeMap<String, (f64, u64)> = BTreeMap::new();
    // agent bucket (main / sub-agent) -> (cost, tokens, turns)
    let mut by_agent: BTreeMap<String, (f64, u64, u64)> = BTreeMap::new();

    for rec in records {
        let cost = record_cost(rec, overrides);
        let toks = rec.total_tokens();
        total_cost += cost;
        total_tokens += toks;
        total_turns += rec.turns;

        let p = by_project.entry(rec.project.clone()).or_default();
        p.0 += cost;
        p.1 += toks;
        p.2 += 1;

        for (m, u) in &rec.models {
            let e = by_model.entry(m.clone()).or_default();
            e.0 += pricing::cost(m, u, overrides);
            e.1.add(u);
        }

        if let Some(day) = rec.day() {
            let d = by_day.entry(day).or_default();
            d.0 += cost;
            d.1 += toks;
        }

        for (bucket, a) in &rec.agents {
            let e = by_agent.entry(bucket.clone()).or_default();
            e.0 += a
                .models
                .iter()
                .map(|(m, u)| pricing::cost(m, u, overrides))
                .sum::<f64>();
            e.1 += a.total_tokens();
            e.2 += a.turns;
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "gauge — {} セッション / {} turns\n",
        commas(records.len() as u64),
        commas(total_turns)
    ));
    out.push_str(&format!(
        "合計コスト {}  ·  トークン {} ({})\n",
        money(total_cost),
        tokens_short(total_tokens),
        commas(total_tokens),
    ));

    // --- by project ---
    out.push_str("\nプロジェクト別\n");
    let mut projects: Vec<_> = by_project.into_iter().collect();
    projects.sort_by(|a, b| {
        b.1 .0
            .partial_cmp(&a.1 .0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (name, (cost, toks, sessions)) in projects.iter().take(15) {
        out.push_str(&format!(
            "  {:<24} {:>9}  {:>8}  {} sess\n",
            truncate(name, 24),
            money(*cost),
            tokens_short(*toks),
            sessions,
        ));
    }

    // --- by model ---
    out.push_str("\nモデル別\n");
    let mut models: Vec<_> = by_model.into_iter().collect();
    models.sort_by(|a, b| {
        b.1 .0
            .partial_cmp(&a.1 .0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (name, (cost, u)) in models.iter() {
        out.push_str(&format!(
            "  {:<24} {:>9}  in {} / out {} / cache r {} w {} ({} hit)\n",
            truncate(name, 24),
            money(*cost),
            tokens_short(u.input),
            tokens_short(u.output),
            tokens_short(u.cache_read),
            tokens_short(cache_write(u)),
            pct(cache_hit_rate(u)),
        ));
    }

    // --- by agent (main vs sub-agent) ---
    if !by_agent.is_empty() {
        out.push_str("\nエージェント別 (main vs sub-agent)\n");
        let mut agents: Vec<_> = by_agent.into_iter().collect();
        agents.sort_by(|a, b| {
            b.1 .0
                .partial_cmp(&a.1 .0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (name, (cost, toks, turns)) in agents.iter() {
            out.push_str(&format!(
                "  {:<12} {:>9}  {:>8}  {} turns\n",
                truncate(name, 12),
                money(*cost),
                tokens_short(*toks),
                commas(*turns),
            ));
        }
    }

    // --- by day (most recent 14) ---
    out.push_str("\n日別 (直近14日)\n");
    let mut days: Vec<_> = by_day.into_iter().collect();
    days.sort_by(|a, b| b.0.cmp(&a.0));
    for (day, (cost, toks)) in days.iter().take(14) {
        out.push_str(&format!(
            "  {}  {:>9}  {:>8}\n",
            day,
            money(*cost),
            tokens_short(*toks),
        ));
    }

    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_groups() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(1234), "1,234");
        assert_eq!(commas(1234567), "1,234,567");
    }

    #[test]
    fn tokens_short_scales() {
        assert_eq!(tokens_short(500), "500");
        assert_eq!(tokens_short(12_300), "12.3k");
        assert_eq!(tokens_short(4_560_000), "4.56M");
    }

    #[test]
    fn empty_report() {
        assert!(render(&[], &[]).contains("no sessions"));
    }

    #[test]
    fn cache_hit_rate_guards_zero_and_splits_read_write() {
        // No input at all → 0% rather than a divide-by-zero NaN.
        assert_eq!(cache_hit_rate(&Usage::default()), 0.0);

        // 750 cached read out of 1000 total input → 75% hit.
        let u = Usage {
            input: 250,
            cache_read: 750,
            cache_write_5m: 100,
            cache_write_1h: 40,
            ..Default::default()
        };
        assert_eq!(cache_hit_rate(&u), 0.75);
        assert_eq!(pct(cache_hit_rate(&u)), "75%");
        assert_eq!(cache_write(&u), 140);
    }

    #[test]
    fn report_shows_agent_breakdown() {
        use harness_core::usage::AgentUsage;
        let mut rec = SessionRecord {
            project: "proj".to_string(),
            turns: 2,
            last_ts: Some("2026-06-27T00:00:00Z".to_string()),
            ..Default::default()
        };
        rec.models.insert(
            "claude-opus-4-8".to_string(),
            Usage {
                input: 150,
                output: 150,
                ..Default::default()
            },
        );
        let mut main = AgentUsage {
            turns: 1,
            ..Default::default()
        };
        main.models.insert(
            "claude-opus-4-8".to_string(),
            Usage {
                input: 100,
                output: 100,
                ..Default::default()
            },
        );
        let mut sub = AgentUsage {
            turns: 1,
            ..Default::default()
        };
        sub.models.insert(
            "claude-opus-4-8".to_string(),
            Usage {
                input: 50,
                output: 50,
                ..Default::default()
            },
        );
        rec.agents.insert("main".to_string(), main);
        rec.agents.insert("sub-agent".to_string(), sub);

        let out = render(&[rec], &[]);
        assert!(out.contains("エージェント別"));
        assert!(out.contains("main"));
        assert!(out.contains("sub-agent"));
    }
}

/// backlog 5a4630a5: the report collapses `cache_write_5m` and
/// `cache_write_1h` into one `w` figure, so a session downgraded to the 5m TTL
/// (the only local signal of that downgrade) is invisible in `gauge report`.
/// Property asserted: the per-model line shows the two buckets separately.
/// Written by an independent auditor, not an implementer.
#[cfg(test)]
mod backlog_5a4630a5 {
    use super::*;

    #[test]
    #[ignore = "backlog 5a4630a5: open defect, remove ignore when fixed"]
    fn report_shows_5m_and_1h_cache_writes_separately() {
        let mut rec = SessionRecord {
            project: "proj".to_string(),
            turns: 1,
            last_ts: Some("2026-06-27T00:00:00Z".to_string()),
            ..Default::default()
        };
        rec.models.insert(
            "claude-opus-4-8".to_string(),
            Usage {
                input: 10,
                output: 10,
                cache_write_5m: 12_300,
                cache_write_1h: 4_560_000,
                ..Default::default()
            },
        );
        let out = render(&[rec], &[]);
        let line = out
            .lines()
            .find(|l| l.contains("claude-opus-4-8"))
            .unwrap_or_else(|| panic!("no model line in report:\n{out}"));
        assert!(
            line.contains(&tokens_short(12_300)) && line.contains(&tokens_short(4_560_000)),
            "cache writes are collapsed into one figure ({} = 5m + 1h); the 5m TTL-downgrade \
             indicator is not visible: {line:?}",
            tokens_short(12_300 + 4_560_000)
        );
    }
}
