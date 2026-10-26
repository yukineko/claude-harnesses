//! gap (DESIGN §3) — deterministic ASSEMBLY of the inputs a skill reasons over
//! to derive "goal − current-state", plus a write-back of the skill-produced
//! gap text into the charter.
//!
//! # Architecture constraint
//!
//! A Rust binary cannot call an LLM, so the SEMANTIC gap derivation ("which DoD
//! items are unmet, what's the biggest gap") is a SKILL job — NOT here. This
//! module only deterministically gathers the raw material the skill will reason
//! over ([`assemble_gap_inputs`]) and persists the gap string the skill writes
//! back ([`persist_gap`]). No judgment, no LLM.

use std::path::Path;

use anyhow::Result;
use harness_core::interrogate::{Authority, Bundle};
use serde::Serialize;

use crate::charter::Charter;
use crate::outcome::Outcome;

/// The deterministic inputs to the (skill-side) gap derivation. Serialized to
/// JSON and printed so the skill can read and reason over it.
///
/// (`Eq` is intentionally not derived: `opportunities` carry `f64` weights.)
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GapInputs {
    /// Observable done conditions (charter `definition_of_done`).
    pub dod: Vec<String>,
    /// Mid-authority "what the project is actually doing now" fragments
    /// (git log / status / diff), newest/most-relevant first.
    pub recent_activity: Vec<String>,
    /// What the next move is measured by (charter `measuring_stick`).
    pub measuring_stick: String,
    /// The most recently recorded outcome (verdict + evidence + the gap it
    /// judged), or `None` if no move has been judged yet. Closes the measurement
    /// loop: the skill sees how the last move actually moved the needle. Set by
    /// `gap_command`; `assemble_gap_inputs` leaves it `None`.
    pub last_outcome: Option<Outcome>,
    /// Per-opportunity gap scaffolding (§3, OST). One entry per opportunity under
    /// the active outcome (charter `north_star`), so the skill derives a gap
    /// *per named bet* instead of one flat gap. The binary supplies `id`/`title`
    /// deterministically; the SKILL fills each `gap` (LLM job — left `None`
    /// here). Set by `gap_command`; `assemble_gap_inputs` leaves it empty.
    pub opportunities: Vec<OpportunityGap>,
}

/// One opportunity's gap slot in the gap output (§3, OST). The deterministic
/// `id`/`title`/`weight` scaffold the skill's per-opportunity gap reasoning;
/// `gap` is the skill-derived text (None until written — no LLM in the binary).
///
/// (`Eq` is intentionally not derived: `weight` is an `f64`.)
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpportunityGap {
    /// The opportunity identifier (slug + hash), matching `Opportunity::id`.
    pub id: String,
    /// The opportunity's human-readable named bet (`Opportunity::title`).
    pub title: String,
    /// The opportunity's ordering weight (`Opportunity::weight`), surfaced so the
    /// skill (and a later deterministic sort) can prioritize by it.
    pub weight: f64,
    /// The skill-derived gap for this opportunity; `None` until the skill writes
    /// it. The binary never fills this (no LLM judgment here).
    pub gap: Option<String>,
}

/// Deterministically assemble the gap inputs from the charter (the stated goal)
/// and the gather [`Bundle`] (what the project is actually doing now).
///
/// The split is purely by provenance, no semantic judgment:
///   - `dod` / `measuring_stick` come straight from the charter,
///   - `recent_activity` is the Mid-authority git fragments (log/status/diff).
pub fn assemble_gap_inputs(charter: &Charter, bundle: &Bundle) -> GapInputs {
    let mut recent_activity = Vec::new();

    for frag in &bundle.fragments {
        if frag.authority != Authority::Mid {
            continue;
        }
        match frag.source_path.as_str() {
            // git activity → recent_activity (preserve gather()'s order).
            "git:log" | "git:status" | "git:diff" => {
                recent_activity.push(frag.text.clone());
            }
            // deepwiki and anything else is not gap-input material here.
            _ => {}
        }
    }

    GapInputs {
        dod: charter.definition_of_done.clone(),
        recent_activity,
        measuring_stick: charter.measuring_stick.clone(),
        // The caller (`gap_command`) fills these from the outcomes/opportunity
        // stores; the pure assembly has no repo root to read them from.
        last_outcome: None,
        opportunities: Vec::new(),
    }
}

/// Persist the skill-produced gap text into `charter.current_gap` and save the
/// charter back to `charter_path`. Deterministic write-back; no interpretation.
pub fn persist_gap(charter_path: &Path, charter: &mut Charter, gap: &str) -> Result<()> {
    charter.current_gap = gap.trim().to_string();
    charter.save(charter_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::interrogate::Fragment;

    fn frag(text: &str, source: &str, authority: Authority) -> Fragment {
        Fragment {
            text: text.to_string(),
            source_path: source.to_string(),
            authority,
            score: 0,
            anchor: None,
        }
    }

    #[test]
    fn assemble_splits_by_provenance() {
        let charter = Charter {
            definition_of_done: vec!["crate builds".to_string(), "tests pass".to_string()],
            measuring_stick: "defensibility x closeness / cost".to_string(),
            ..Charter::default()
        };
        let bundle = Bundle {
            fragments: vec![
                frag("north_star text", ".compass/charter.md", Authority::High),
                frag("abc123 do a thing", "git:log", Authority::Mid),
                frag(" M src/lib.rs", "git:status", Authority::Mid),
                frag("- remaining item", ".claude/progress.md", Authority::Mid),
                frag("some wiki page", ".deepwiki/arch.md", Authority::Mid),
            ],
        };

        let inputs = assemble_gap_inputs(&charter, &bundle);
        assert_eq!(inputs.dod, vec!["crate builds", "tests pass"]);
        assert_eq!(inputs.measuring_stick, "defensibility x closeness / cost");
        // git fragments in gather order; deepwiki excluded; charter excluded.
        assert_eq!(
            inputs.recent_activity,
            vec!["abc123 do a thing".to_string(), " M src/lib.rs".to_string()]
        );
        // User ruling 2026-10-27: a progress-file fragment is not gap input.
        let json = serde_json::to_string(&inputs).expect("serialize");
        assert!(!json.contains("progress_excerpt") && !json.contains("remaining item"));
    }

    #[test]
    fn assemble_with_no_activity_is_empty_not_error() {
        let charter = Charter::default();
        let bundle = Bundle { fragments: vec![] };
        let inputs = assemble_gap_inputs(&charter, &bundle);
        assert!(inputs.recent_activity.is_empty());
        assert!(inputs.last_outcome.is_none());
        assert!(inputs.dod.is_empty());
    }

    #[test]
    fn last_outcome_is_null_when_none_recorded() {
        let charter = Charter::default();
        let bundle = Bundle { fragments: vec![] };
        let inputs = assemble_gap_inputs(&charter, &bundle);
        assert!(inputs.last_outcome.is_none());

        let json = serde_json::to_value(&inputs).expect("serialize");
        assert!(json["last_outcome"].is_null());
    }

    #[test]
    fn last_outcome_surfaces_latest_recorded() {
        use crate::outcome::{self, Verdict};

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let charter = Charter {
            north_star: "ship the loop".to_string(),
            current_gap: "moves are unjudged".to_string(),
            ..Charter::default()
        };

        // record one outcome, then mirror what `gap_command` does.
        outcome::record(
            root,
            &charter,
            Verdict::Forward,
            vec!["p95 fell 20%".to_string()],
        )
        .expect("record");

        let mut inputs = assemble_gap_inputs(&charter, &Bundle { fragments: vec![] });
        inputs.last_outcome = outcome::latest(root).expect("latest");

        let last = inputs.last_outcome.as_ref().expect("some outcome");
        assert_eq!(last.verdict, Verdict::Forward);
        assert_eq!(last.evidence, vec!["p95 fell 20%".to_string()]);
        assert_eq!(last.current_gap, "moves are unjudged");

        let json = serde_json::to_value(&inputs).expect("serialize");
        assert_eq!(json["last_outcome"]["verdict"], "forward");
        assert_eq!(json["last_outcome"]["evidence"][0], "p95 fell 20%");
        assert_eq!(json["last_outcome"]["current_gap"], "moves are unjudged");
    }

    #[test]
    fn assemble_leaves_opportunities_empty() {
        // The pure assembly has no repo root; per-opportunity scaffolding is the
        // caller's (gap_command's) job. Output array is present but empty.
        let charter = Charter::default();
        let inputs = assemble_gap_inputs(&charter, &Bundle { fragments: vec![] });
        assert!(inputs.opportunities.is_empty());

        let json = serde_json::to_value(&inputs).expect("serialize");
        assert!(json["opportunities"].is_array());
        assert_eq!(json["opportunities"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn opportunities_surface_per_opportunity_gap_array() {
        use crate::opportunity;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let north_star = "ship the OST loop";
        let charter = Charter {
            north_star: north_star.to_string(),
            ..Charter::default()
        };

        // Two named bets under the active outcome.
        opportunity::record(root, "faster cold start", north_star, 1.0).expect("record a");
        opportunity::record(root, "fewer retries", north_star, 1.0).expect("record b");
        // One bet under a *different* outcome — must not leak into this gap.
        opportunity::record(root, "unrelated bet", "some other north_star", 1.0).expect("record c");

        // Mirror what gap_command does to fill the per-opportunity scaffold.
        let mut inputs = assemble_gap_inputs(&charter, &Bundle { fragments: vec![] });
        inputs.opportunities = opportunity::list_under(root, north_star)
            .expect("list_under")
            .into_iter()
            .map(|o| OpportunityGap {
                id: o.id,
                title: o.title,
                weight: o.weight,
                gap: None,
            })
            .collect();

        // Per-opportunity array (not a flat single gap), scoped to this outcome.
        assert_eq!(inputs.opportunities.len(), 2);
        let titles: Vec<&str> = inputs
            .opportunities
            .iter()
            .map(|o| o.title.as_str())
            .collect();
        assert!(titles.contains(&"faster cold start"));
        assert!(titles.contains(&"fewer retries"));
        assert!(!titles.contains(&"unrelated bet"));

        let json = serde_json::to_value(&inputs).expect("serialize");
        let arr = json["opportunities"].as_array().expect("array");
        assert_eq!(arr.len(), 2);
        // Each entry carries id + title + weight and a null (skill-derived) gap.
        for entry in arr {
            assert!(entry["id"].is_string());
            assert!(entry["title"].is_string());
            assert!(entry["weight"].is_number());
            assert!(entry["gap"].is_null());
        }
    }

    #[test]
    fn persist_gap_writes_and_saves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".compass").join("charter.md");
        let mut charter = Charter {
            north_star: "ship compass".to_string(),
            definition_of_done: vec!["builds".to_string()],
            ..Charter::default()
        };

        persist_gap(&path, &mut charter, "  the biggest gap is X  ").expect("persist");
        assert_eq!(charter.current_gap, "the biggest gap is X");

        let reloaded = Charter::load(&path).expect("load");
        assert_eq!(reloaded.current_gap, "the biggest gap is X");
        // other fields preserved.
        assert_eq!(reloaded.north_star, "ship compass");
    }
}
