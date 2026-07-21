//! The canonical per-session record, shared across the harness plugins.
//!
//! Each Claude Code session is one JSON file under
//! `<state_dir>/sessions/<session_id>.json`, rewritten on every Stop with the
//! cumulative totals for the whole session so far. `gauge` owns the write path
//! (its Stop hook re-reads the full transcript and upserts the record); other
//! plugins can *read* the same record via [`load_one`] / [`load_all`] instead
//! of re-parsing the transcript themselves — this is the single source of truth
//! that keeps cost/turn/tool numbers from drifting between plugins.
//!
//! `SessionRecord` lived in the `gauge` binary crate originally; it moved here
//! so passive consumers (session-insights, harness-status) can share the exact
//! type and the one construction path ([`SessionRecord::from_aggregate`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::usage::{AgentUsage, Aggregate, ModelUsage};
use crate::verdict::Determination;

/// Token counts for one model within a session. Alias kept for call sites that
/// referred to it as `Usage`; the type itself is [`crate::usage::ModelUsage`].
pub type Usage = ModelUsage;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    #[serde(default)]
    pub models: BTreeMap<String, Usage>,
    #[serde(default)]
    pub turns: u64,
    #[serde(default)]
    pub tools: BTreeMap<String, u64>,
    #[serde(default)]
    pub first_ts: Option<String>,
    #[serde(default)]
    pub last_ts: Option<String>,
    /// Per-agent (main vs sub-agent) token breakdown; empty on legacy records.
    #[serde(default)]
    pub agents: BTreeMap<String, AgentUsage>,
    #[serde(default)]
    pub updated_at: String,
}

impl SessionRecord {
    /// Build a record from a transcript [`Aggregate`] — the single construction
    /// path every writer uses, so the persisted shape can't diverge per plugin.
    /// `track_tools = false` drops the per-tool counts (some configs opt out).
    pub fn from_aggregate(
        session_id: impl Into<String>,
        project: impl Into<String>,
        cwd: impl Into<String>,
        agg: Aggregate,
        track_tools: bool,
        updated_at: impl Into<String>,
    ) -> Self {
        SessionRecord {
            session_id: session_id.into(),
            project: project.into(),
            cwd: cwd.into(),
            models: agg.models,
            turns: agg.turns,
            tools: if track_tools {
                agg.tools
            } else {
                BTreeMap::new()
            },
            first_ts: agg.first_ts,
            last_ts: agg.last_ts,
            agents: agg.agents,
            updated_at: updated_at.into(),
        }
    }

    /// USD spent by each agent bucket (main / sub-agent), via `session_cost`.
    /// Empty when the record predates per-agent attribution.
    pub fn agent_costs(
        &self,
        overrides: &[crate::pricing::PriceOverride],
    ) -> BTreeMap<String, f64> {
        self.agents
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    crate::pricing::session_cost(a.models.iter(), overrides),
                )
            })
            .collect()
    }

    /// Total tokens across all models.
    pub fn total_tokens(&self) -> u64 {
        self.models.values().map(|u| u.total_tokens()).sum()
    }

    /// The day (YYYY-MM-DD) this session last ran, from `last_ts`.
    pub fn day(&self) -> Option<String> {
        self.last_ts
            .as_ref()
            .and_then(|t| t.get(0..10).map(|s| s.to_string()))
    }
}

/// gauge's default store directory (`~/.gauge/store`). Consumers that want to
/// read the canon use this to locate it without depending on the gauge crate.
/// If a user overrides gauge's `state_dir` in its config, the canon won't be
/// found here and the consumer should fall back to a fresh transcript parse.
pub fn default_state_dir() -> PathBuf {
    crate::config::base_dir("gauge").join("store")
}

pub fn sessions_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions")
}

/// Keep only filesystem-safe characters so a hostile/empty session id can't
/// escape the store directory.
pub fn safe_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Write (overwrite) the record for its session.
///
/// **The write is best-effort and its failure is invisible to the caller**: if
/// the store directory cannot be created, the record cannot be serialized, or
/// the write fails, this returns normally having persisted nothing. It holds no
/// verdict itself, and the two consumers of a *missing* record both resolve to
/// the restricted side rather than reading absence as "no spend":
///
/// * `budgetguard::gate::session_cost` (`crates/budgetguard/src/gate.rs:164`)
///   falls back to a full transcript re-parse when [`load_one`] yields nothing,
///   which is the accurate (never under-counting) source.
/// * [`load_all`] reports [`Determination::Undetermined`] when the store dir
///   exists but cannot be enumerated, so an unreadable store is no longer
///   indistinguishable from an empty one.
///
/// A lost write therefore degrades to "re-derive from the transcript", never to
/// "there was no spend".
pub fn upsert(state_dir: &Path, rec: &SessionRecord) {
    let dir = sessions_dir(state_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.json", safe_id(&rec.session_id)));
    if let Ok(text) = serde_json::to_string_pretty(rec) {
        let _ = std::fs::write(path, text);
    }
}

/// Load a single session's record by id, if present and parseable.
pub fn load_one(state_dir: &Path, session_id: &str) -> Option<SessionRecord> {
    let path = sessions_dir(state_dir).join(format!("{}.json", safe_id(session_id)));
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Load every session record in the store.
///
/// Three-valued on purpose (see [`crate::verdict::Determination`]):
///
/// * `Known(records)` — the store directory was enumerated. An **absent** store
///   (`NotFound`: gauge never ran / a custom `state_dir`) is a genuine
///   `Known(vec![])`, and individual `.json` files that are unreadable or
///   unparseable are skipped (one corrupt record must not hide the rest).
/// * `Undetermined(why)` — the store directory exists but could not be walked
///   (permissions, IO error), or one of its directory entries could not be
///   read. **No partial list is returned in that case**: a truncated list read
///   as "all sessions" is the under-count this type exists to prevent. `why`
///   carries the path and the OS error.
///
/// Before this returned `Vec<SessionRecord>`, an unreadable store yielded the
/// same empty `Vec` as an empty one — observed directly: a store holding one
/// record reported `1 record(s)` readable and `0 record(s)` after `chmod 000`.
pub fn load_all(state_dir: &Path) -> Determination<Vec<SessionRecord>> {
    let dir = sessions_dir(state_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // An absent store is a real observation: there are no records.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(Vec::new())
        }
        Err(e) => {
            return Determination::undetermined(format!(
                "session store {} could not be read: {e}",
                dir.display()
            ))
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        // A failed directory entry means the listing is incomplete; returning
        // what we have so far would report an under-count as the whole store.
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "session store {}: a directory entry could not be read \
                     (listing is incomplete): {e}",
                    dir.display()
                ))
            }
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(rec) = serde_json::from_str::<SessionRecord>(&text) {
                out.push(rec);
            }
        }
    }
    Determination::Known(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{AgentUsage, AGENT_MAIN, AGENT_SUB};
    use tempfile::TempDir;

    #[test]
    fn safe_id_strips_separators() {
        assert_eq!(safe_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(safe_id("abc-123_DEF"), "abc-123_DEF");
        assert_eq!(safe_id(""), "unknown");
    }

    #[test]
    fn usage_totals() {
        let u = Usage {
            input: 10,
            output: 20,
            cache_write_5m: 1,
            cache_write_1h: 2,
            cache_read: 3,
        };
        assert_eq!(u.total_tokens(), 36);
    }

    fn sample_aggregate() -> Aggregate {
        let usage = Usage {
            input: 5,
            output: 7,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 1,
        };
        let mut models = BTreeMap::new();
        models.insert("claude-x".to_string(), usage.clone());
        let mut tools = BTreeMap::new();
        tools.insert("Bash".to_string(), 3);
        // Split: 3 turns main, 1 turn sub-agent (totals still 4 turns / 13 tokens).
        let mut agents = BTreeMap::new();
        let mut main_models = BTreeMap::new();
        main_models.insert("claude-x".to_string(), usage.clone());
        agents.insert(
            AGENT_MAIN.to_string(),
            AgentUsage {
                models: main_models,
                turns: 3,
            },
        );
        agents.insert(
            AGENT_SUB.to_string(),
            AgentUsage {
                models: BTreeMap::new(),
                turns: 1,
            },
        );
        Aggregate {
            models,
            turns: 4,
            tools,
            first_ts: Some("2026-06-27T01:00:00Z".to_string()),
            last_ts: Some("2026-06-27T02:00:00Z".to_string()),
            agents,
            ..Aggregate::default()
        }
    }

    #[test]
    fn from_aggregate_tracks_tools_when_enabled() {
        let rec = SessionRecord::from_aggregate(
            "sid",
            "proj",
            "/cwd",
            sample_aggregate(),
            true,
            "2026-06-27T02:00:01Z",
        );
        assert_eq!(rec.session_id, "sid");
        assert_eq!(rec.turns, 4);
        assert_eq!(rec.tools.get("Bash"), Some(&3));
        assert_eq!(rec.total_tokens(), 13);
        assert_eq!(rec.day(), Some("2026-06-27".to_string()));
    }

    #[test]
    fn from_aggregate_drops_tools_when_disabled() {
        let rec =
            SessionRecord::from_aggregate("sid", "proj", "/cwd", sample_aggregate(), false, "ts");
        assert!(rec.tools.is_empty());
        assert_eq!(rec.turns, 4); // turns/models still kept
    }

    #[test]
    fn upsert_then_load_one_roundtrips() {
        let dir = TempDir::new().unwrap();
        let rec =
            SessionRecord::from_aggregate("sess-1", "proj", "/cwd", sample_aggregate(), true, "ts");
        upsert(dir.path(), &rec);

        let loaded = load_one(dir.path(), "sess-1").expect("record present");
        assert_eq!(loaded.session_id, "sess-1");
        assert_eq!(loaded.total_tokens(), 13);
        assert_eq!(loaded.tools.get("Bash"), Some(&3));
        // Per-agent breakdown survives the JSON roundtrip.
        assert_eq!(loaded.agents.get(AGENT_MAIN).map(|a| a.turns), Some(3));
        assert_eq!(loaded.agents.get(AGENT_SUB).map(|a| a.turns), Some(1));

        // Missing id → None, not an error.
        assert!(load_one(dir.path(), "nope").is_none());
    }

    #[test]
    fn agent_costs_split_by_bucket() {
        let rec =
            SessionRecord::from_aggregate("sid", "proj", "/cwd", sample_aggregate(), true, "ts");
        let costs = rec.agent_costs(&[]);
        // Main carried the tokens; sub-agent bucket had none → 0 USD.
        assert!(costs.contains_key(AGENT_MAIN));
        assert_eq!(costs.get(AGENT_SUB).copied(), Some(0.0));
    }

    #[test]
    fn load_one_matches_load_all() {
        let dir = TempDir::new().unwrap();
        let rec =
            SessionRecord::from_aggregate("sess-2", "proj", "/cwd", sample_aggregate(), true, "ts");
        upsert(dir.path(), &rec);
        let all = match load_all(dir.path()) {
            Determination::Known(all) => all,
            Determination::Undetermined(why) => panic!("readable store must be Known: {why:?}"),
        };
        let one = load_one(dir.path(), "sess-2").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, one.session_id);
        assert_eq!(all[0].total_tokens(), one.total_tokens());
    }
}
