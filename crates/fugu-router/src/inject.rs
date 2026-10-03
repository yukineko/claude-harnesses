//! Build the advisory routing-memory summary injected at UserPromptSubmit, so
//! the main loop (and the interpreter it briefs) is aware of what models have
//! worked before. Advisory only — never decisive on its own.

use std::collections::BTreeMap;

use crate::store::Episode;

/// True if the prompt looks like coding / orchestration work worth routing.
pub fn looks_actionable(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    [
        "condukt",
        "implement",
        "refactor",
        "fix",
        "add ",
        "build",
        "migrate",
        "feature",
        "実装",
        "修正",
        "リファクタ",
        "追加",
        "機能",
    ]
    .iter()
    .any(|k| p.contains(k))
}

/// One-block per-model pass-rate summary from the store, capped to `limit` bytes.
pub fn summary(episodes: &[Episode], limit: usize) -> Option<String> {
    if episodes.is_empty() {
        return None;
    }
    // (count, passes, cost-measured count, measured cost total). The average
    // cost covers MEASURED episodes only (backlog 55fb3861): an episode with
    // `cost_usd = None` is unmeasured, not free.
    let mut agg: BTreeMap<String, (usize, usize, usize, f64)> = BTreeMap::new();
    for e in episodes {
        let s = agg.entry(e.model.clone()).or_insert((0, 0, 0, 0.0));
        s.0 += 1;
        if e.pass {
            s.1 += 1;
        }
        if let Some(c) = e.cost_usd {
            s.2 += 1;
            s.3 += c;
        }
    }
    let mut lines = vec![format!(
        "[fugu-router] {} routing episode(s) recorded — per-model outcomes:",
        episodes.len()
    )];
    for (m, (n, p, cn, cost)) in &agg {
        let avg = if *cn > 0 {
            format!("avg ${:.4} (n={cn})", cost / *cn as f64)
        } else {
            "cost unmeasured".to_string()
        };
        lines.push(format!(
            "  {m}: {p}/{n} pass ({:.0}%), {avg}",
            if *n > 0 {
                *p as f64 / *n as f64 * 100.0
            } else {
                0.0
            },
        ));
    }
    lines.push(
        "Run `fugu-router route --file <decomp.json>` to set suggested_model per task.".into(),
    );
    let mut out = lines.join("\n");
    if limit > 0 && out.len() > limit {
        out.truncate(limit);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_injects_nothing() {
        assert!(summary(&[], 1500).is_none());
    }

    /// backlog 55fb3861: an episode with no recorded cost is unmeasured, not
    /// free — the injected per-model average covers measured episodes only.
    #[test]
    fn summary_average_excludes_unmeasured_cost() {
        let eps: Vec<Episode> = [
            r#"{"title":"a","model":"sonnet","pass":true,"cost_usd":2.0}"#,
            r#"{"title":"b","model":"sonnet","pass":true}"#,
        ]
        .iter()
        .map(|l| serde_json::from_str(l).expect("episode json"))
        .collect();
        let s = summary(&eps, 0).expect("summary");
        assert!(
            s.contains("avg $2.0000"),
            "unmeasured episode averaged in as $0 (would give $1.0000): {s}"
        );
    }

    #[test]
    fn summary_reports_unmeasured_instead_of_zero_average() {
        let eps: Vec<Episode> =
            vec![
                serde_json::from_str(r#"{"title":"b","model":"sonnet","pass":true}"#)
                    .expect("episode json"),
            ];
        let s = summary(&eps, 0).expect("summary");
        assert!(
            !s.contains("avg $0.0000") && s.contains("cost unmeasured"),
            "no measured cost must be shown as unmeasured, not $0: {s}"
        );
    }

    #[test]
    fn actionable_detection() {
        assert!(looks_actionable("/condukt implement the parser"));
        assert!(looks_actionable("認証を実装して"));
        assert!(!looks_actionable("what time is it"));
    }
}
