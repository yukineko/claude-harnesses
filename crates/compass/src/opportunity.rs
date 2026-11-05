//! opportunity (DESIGN §-OST) — the discovery-side layer between the charter's
//! single `north_star` (the active outcome) and the one solution-action `route`
//! hands to condukt. PDO's Opportunity Solution Tree puts named customer
//! needs/opportunities under an outcome; compass previously went
//! north_star→gap→solution with no opportunity layer at all.
//!
//! This is the persisted store (charter DoD#1): an append-only JSON array of
//! [`Opportunity`] under `.compass/opportunities.json`, each scoped to an active
//! outcome (snapshotted as the charter `north_star`, mirroring how
//! [`crate::outcome`] snapshots its goal). Wiring the opportunity ref into the
//! `route` handoff (DoD#2) and per-opportunity `gap` (DoD#3) build on top and are
//! parked follow-ups.
//!
//! # Persistence
//!
//! Load → append → atomic-write, mirroring the outcome/charter store conventions.
//! Recording REQUIRES a non-empty title (build is not validation: an empty
//! opportunity is not a recorded bet), mirroring the evidence guard in
//! [`crate::outcome::record`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default weight for an opportunity recorded without an explicit `--weight`
/// (and for legacy records persisted before the field existed). A neutral 1.0 so
/// an unweighted opportunity neither floats to the top nor sinks: the layer stays
/// backward-compatible until someone actually scores a bet.
pub const DEFAULT_WEIGHT: f64 = 1.0;

/// serde default for the `weight` field: legacy `opportunities.json` written
/// before `weight` existed deserialize to [`DEFAULT_WEIGHT`] rather than failing.
fn default_weight() -> f64 {
    DEFAULT_WEIGHT
}

/// A single named opportunity (customer need / bet) sitting under an active
/// outcome. Self-describing: it snapshots the `outcome_ref` it was filed under so
/// a later reader can group opportunities by outcome with no other context.
///
/// (`Eq` is intentionally not derived: `weight` is an `f64`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Opportunity {
    /// Stable, human-readable id derived from the title (slug + short hash).
    pub id: String,
    /// The opportunity statement (non-empty, trimmed).
    pub title: String,
    /// The active outcome this opportunity sits under (charter `north_star`
    /// snapshot, or an explicit `--outcome` override).
    pub outcome_ref: String,
    /// Relative weight (outcome-impact or confidence) that drives ordering: the
    /// skill sorts opportunities by this so the layer is load-bearing, not inert.
    /// Legacy records without it deserialize to [`DEFAULT_WEIGHT`].
    #[serde(default = "default_weight")]
    pub weight: f64,
    /// Wall-clock record time, unix seconds (0 if the clock is pre-epoch).
    pub created_at: u64,
}

/// On-disk shape: a JSON object with an `opportunities` array (forward-compatible).
#[derive(Debug, Default, Serialize, Deserialize)]
struct OpportunitiesFile {
    #[serde(default)]
    opportunities: Vec<Opportunity>,
}

/// `.compass/opportunities.json` under the project root.
pub fn store_path(root: &Path) -> PathBuf {
    root.join(".compass").join("opportunities.json")
}

/// Load all recorded opportunities (oldest first). A missing file => empty Vec; a
/// corrupt file is a hard error (we don't silently drop recorded bets).
pub fn load(root: &Path) -> Result<Vec<Opportunity>> {
    let path = store_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file: OpportunitiesFile = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            Ok(file.opportunities)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Atomic-write the full opportunities array, creating `.compass/` if absent.
fn save(root: &Path, opportunities: &[Opportunity]) -> Result<()> {
    let path = store_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = OpportunitiesFile {
        opportunities: opportunities.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file).context("serializing opportunities")?;
    crate::outcome::write_atomic(&path, &json)
}

/// Current wall-clock as unix seconds (0 if the system clock is before epoch).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A stable, human-readable id for an opportunity title: an ASCII slug plus a
/// 6-hex hash suffix for uniqueness. A purely non-ASCII title (e.g. Japanese)
/// slugs to empty, so it falls back to `opp-<hash>`. Deterministic per title
/// (mirrors the slug-id convention used by curate/replaykit).
fn slug_id(title: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut h);
    let suffix = format!("{:06x}", h.finish() & 0xff_ffff);
    let slug = title
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("opp-{suffix}")
    } else {
        format!("{slug}-{suffix}")
    }
}

/// Record one named opportunity under `outcome_ref` (with `weight` driving later
/// ordering) and append it to the store. REQUIRES a non-empty title (trimmed);
/// bails otherwise (an empty bet is not a recorded opportunity). Returns the
/// persisted [`Opportunity`].
pub fn record(root: &Path, title: &str, outcome_ref: &str, weight: f64) -> Result<Opportunity> {
    let title = title.trim();
    if title.is_empty() {
        anyhow::bail!("opportunity requires a non-empty --title (a named bet, not a blank)");
    }
    // load/push/save under the store's exclusive lock (refused if it is held).
    let _lock = crate::outcome::lock_store(&store_path(root))?;
    let mut opportunities = load(root)?;
    let opportunity = Opportunity {
        id: slug_id(title),
        title: title.to_string(),
        outcome_ref: outcome_ref.trim().to_string(),
        weight,
        created_at: now_unix(),
    };
    opportunities.push(opportunity.clone());
    save(root, &opportunities)?;
    Ok(opportunity)
}

/// All opportunities filed under `outcome_ref` (oldest first). An empty store or
/// a non-matching outcome yields an empty Vec.
pub fn list_under(root: &Path, outcome_ref: &str) -> Result<Vec<Opportunity>> {
    let want = outcome_ref.trim();
    Ok(load(root)?
        .into_iter()
        .filter(|o| o.outcome_ref == want)
        .collect())
}

/// Opportunities under `outcome_ref`, ranked by `weight` descending so the layer
/// is load-bearing (the heaviest bet leads). The sort is STABLE and starts from
/// [`list_under`]'s oldest-first order, so equal weights keep insertion order as
/// the deterministic tiebreak. `f64::total_cmp` gives a total order (no NaN
/// surprises). This is the single canonical ranking both `gap` and the `route`
/// handoff consume.
pub fn list_under_ranked(root: &Path, outcome_ref: &str) -> Result<Vec<Opportunity>> {
    let mut found = list_under(root, outcome_ref)?;
    found.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_with_title_persists_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let rec = record(
            root,
            "  users can't see why a move was chosen  ",
            "ship OST",
            2.5,
        )
        .expect("record");

        // title trimmed, scoped to the outcome, stable id derived, weight kept.
        assert_eq!(rec.title, "users can't see why a move was chosen");
        assert_eq!(rec.outcome_ref, "ship OST");
        assert_eq!(rec.weight, 2.5);
        assert!(rec.id.ends_with(&rec.id[rec.id.len() - 6..]));
        assert!(!rec.id.is_empty());

        // reload from disk: the record round-trips.
        let loaded = load(root).expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], rec);
    }

    #[test]
    fn record_persists_weight_and_legacy_records_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        // an explicitly weighted record round-trips its weight.
        record(root, "high-impact bet", "outcome-1", 7.0).expect("record");
        assert_eq!(load(root).expect("load")[0].weight, 7.0);

        // a legacy store written WITHOUT a weight field deserializes to the
        // default (backward compatible — no panic, no data loss).
        let legacy = r#"{ "opportunities": [
            { "id": "legacy-abc123", "title": "old bet", "outcome_ref": "outcome-1", "created_at": 1 }
        ] }"#;
        std::fs::create_dir_all(store_path(root).parent().unwrap()).unwrap();
        std::fs::write(store_path(root), legacy).unwrap();

        let loaded = load(root).expect("load legacy");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "old bet");
        assert_eq!(loaded[0].weight, DEFAULT_WEIGHT);
    }

    #[test]
    fn empty_title_is_rejected_and_persists_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let err = record(root, "   ", "ship OST", DEFAULT_WEIGHT).unwrap_err();
        assert!(err.to_string().contains("non-empty --title"));

        // nothing was written.
        assert!(!store_path(root).exists());
        assert_eq!(load(root).expect("load").len(), 0);
    }

    #[test]
    fn list_under_filters_by_active_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        record(root, "opportunity A", "outcome-1", DEFAULT_WEIGHT).expect("a");
        record(root, "opportunity B", "outcome-1", DEFAULT_WEIGHT).expect("b");
        record(root, "opportunity C", "outcome-2", DEFAULT_WEIGHT).expect("c");

        let under_1 = list_under(root, "outcome-1").expect("list 1");
        assert_eq!(under_1.len(), 2);
        assert!(under_1.iter().all(|o| o.outcome_ref == "outcome-1"));

        let under_2 = list_under(root, "outcome-2").expect("list 2");
        assert_eq!(under_2.len(), 1);

        // a non-matching outcome yields empty.
        assert!(list_under(root, "outcome-X").expect("list x").is_empty());
    }

    #[test]
    fn list_under_ranked_orders_by_weight_desc_with_insertion_tiebreak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        // recorded out of weight order; two share a weight (insertion tiebreak).
        record(root, "low bet", "o", 1.0).expect("low");
        record(root, "top bet", "o", 9.0).expect("top");
        record(root, "mid bet A", "o", 5.0).expect("mid a");
        record(root, "mid bet B", "o", 5.0).expect("mid b");

        let ranked: Vec<String> = list_under_ranked(root, "o")
            .expect("ranked")
            .into_iter()
            .map(|o| o.title)
            .collect();
        // weight desc; equal weights (5.0) keep insertion order A before B.
        assert_eq!(ranked, vec!["top bet", "mid bet A", "mid bet B", "low bet"]);

        // changing a weight changes the order: bump "low bet" above the rest.
        record(root, "low bet now heavy", "o", 99.0).expect("heavy");
        let top = list_under_ranked(root, "o").expect("ranked2");
        assert_eq!(top[0].title, "low bet now heavy");
    }

    #[test]
    fn list_under_is_empty_when_no_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(list_under(dir.path(), "any").expect("list").is_empty());
    }

    #[test]
    fn slug_id_falls_back_for_non_ascii() {
        // a purely non-ASCII (Japanese) title slugs to empty => opp-<hash>.
        let id = slug_id("機会の層");
        assert!(id.starts_with("opp-"), "got {id}");
        // deterministic per title.
        assert_eq!(id, slug_id("機会の層"));
        // distinct titles get distinct ids (overwhelmingly likely).
        assert_ne!(slug_id("alpha"), slug_id("beta"));
    }
}
