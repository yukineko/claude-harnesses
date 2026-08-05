//! Per-model token/usage aggregation from a session's JSONL transcript.
//!
//! Aggregate one session's transcript into per-model token usage, tool call
//! counts, and a timespan. A missing/unreadable *main transcript* and an
//! unparseable individual line are fail-soft: the former yields `None` (the
//! caller records nothing), the latter is skipped.
//!
//! The **sub-agent** side is deliberately not fail-soft, because there its
//! failure mode is an under-count rather than an absence: when the sibling
//! `<stem>/subagents/` directory exists but cannot be enumerated, that is
//! reported as [`Determination::Undetermined`] (on
//! [`Aggregate::subagent_scan`] / from [`subagent_usage`]), never as an empty
//! list. An unreadable sub-agent directory is not a session without sub-agents.
//!
//! Each assistant line carries a `message.usage` block (`input_tokens`,
//! `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`,
//! and a `cache_creation` 5m/1h split) plus a `message.model`; tool calls appear
//! as `tool_use` parts in `message.content`.
//!
//! NOTE: this module's [`ModelUsage`] is the per-model *cost-accounting* tally
//! (uncached input/output plus the cache write/read split). It is distinct from
//! [`crate::transcript::Usage`], which is the live-window occupancy snapshot used
//! for context estimation. The two are intentionally separate types.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::verdict::Determination;

/// Agent bucket key for the main (top-level) conversation.
pub const AGENT_MAIN: &str = "main";
/// Agent bucket key for sub-agent (Task / sidechain) activity, aggregated.
pub const AGENT_SUB: &str = "sub-agent";

/// Token counts for one model within a session. `input`/`output` are the
/// uncached counts; cache writes/reads are tracked separately for pricing.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_write_5m: u64,
    #[serde(default)]
    pub cache_write_1h: u64,
    #[serde(default)]
    pub cache_read: u64,
}

impl ModelUsage {
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }

    pub fn add(&mut self, other: &ModelUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_read += other.cache_read;
    }
}

/// Per-agent token tally: which models a single agent bucket (main or
/// sub-agent) used and how many turns it took. Lets reports attribute cost to
/// the main thread vs sub-agent fan-out instead of collapsing them.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    #[serde(default)]
    pub models: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub turns: u64,
}

impl AgentUsage {
    pub fn total_tokens(&self) -> u64 {
        self.models.values().map(|u| u.total_tokens()).sum()
    }
}

/// The whole-transcript aggregation: per-model token tallies, the number of
/// assistant turns, per-tool call counts, the first/last timestamp, and a
/// per-agent (main vs sub-agent) breakdown.
///
/// `models`/`turns` are the session grand total (main + sub-agent); `agents`
/// splits the same totals by [`AGENT_MAIN`] / [`AGENT_SUB`] so reports can show
/// where the spend went.
#[derive(Debug)]
pub struct Aggregate {
    pub models: BTreeMap<String, ModelUsage>,
    /// Number of assistant model requests (turns) with usage.
    pub turns: u64,
    pub tools: BTreeMap<String, u64>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    /// Per-agent breakdown keyed by [`AGENT_MAIN`] / [`AGENT_SUB`].
    pub agents: BTreeMap<String, AgentUsage>,
    /// Whether every sub-agent spend source that applies to this transcript was
    /// actually read.
    ///
    /// `Known(())` — nothing was left unread: either the sub-agent turns were
    /// inline in the main transcript, or the sibling `<stem>/subagents/`
    /// directory was enumerated to completion (including "it is absent").
    ///
    /// `Undetermined(why)` — the sibling `subagents/` directory exists but
    /// could not be walked, so **the totals in this struct are an under-count
    /// of unknown size**. A consumer that prices this aggregate (budgetguard's
    /// budget gate, gauge's persisted [`crate::session::SessionRecord`]) must
    /// not treat the number as a spend measurement; use
    /// [`Aggregate::complete`] to get that refusal as a value.
    pub subagent_scan: Determination<()>,
}

impl Default for Aggregate {
    /// A zeroed accumulator. `subagent_scan` starts at `Known(())` because a
    /// freshly-built accumulator has not *attempted* any sub-agent scan, and so
    /// cannot have failed one; only [`aggregate`] moves it to `Undetermined`,
    /// and it does so before the value is ever handed to a caller. This is not
    /// a defaulted pass over a check that ran — there is no check yet.
    fn default() -> Self {
        Aggregate {
            models: BTreeMap::new(),
            turns: 0,
            tools: BTreeMap::new(),
            first_ts: None,
            last_ts: None,
            agents: BTreeMap::new(),
            subagent_scan: Determination::Known(()),
        }
    }
}

impl Aggregate {
    /// This aggregate, but only when every sub-agent source was read — the
    /// fail-closed accessor for consumers whose answer depends on the totals
    /// being complete (a budget gate, a persisted canonical record).
    ///
    /// `Undetermined` carries the same reason [`subagent_scan`](Self::subagent_scan)
    /// holds, so `require()`'s blocked arm hands the caller a fail-closed
    /// verdict with that reason already attached.
    pub fn complete(self) -> Determination<Self> {
        match &self.subagent_scan {
            Determination::Known(()) => Determination::Known(self),
            Determination::Undetermined(why) => Determination::Undetermined(why.clone()),
        }
    }
}

/// Aggregate one session's transcript at `path` into per-model usage.
///
/// Returns `None` for an empty path, an unreadable file, or a transcript with
/// no assistant turns and no tool calls. Reads the whole transcript (cost
/// accounting must see every line); parse errors on individual lines are
/// skipped rather than aborting.
///
/// Sub-agent (Task) spend is attributed to the [`AGENT_SUB`] bucket and folded
/// into the grand total. Two layouts are handled: sidechain lines inline in the
/// main transcript (`isSidechain: true`), and the newer layout where each
/// sub-agent has its own file under `<transcript-stem>/subagents/*.jsonl`. The
/// sibling files are only read when the main transcript had no inline sidechain
/// lines, so the two layouts never double-count.
///
/// **When the sibling `subagents/` directory exists but cannot be enumerated,
/// this does NOT return `None`** — `None` means "no transcript / nothing
/// recorded", and collapsing "could not read the sub-agent spend" into it would
/// hand a caller silence where an under-count happened. Instead the returned
/// [`Aggregate`] carries `subagent_scan = Undetermined(why)` (see
/// [`Aggregate::complete`]), and it is returned even when the main transcript
/// contributed no turns or tools, so the undetermined answer always reaches the
/// caller.
pub fn aggregate(path: &str) -> Option<Aggregate> {
    if path.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut agg = Aggregate::default();

    // Main transcript: bucket per-line by isSidechain, track timestamps + tools.
    let saw_inline_sub = ingest(&mut agg, &text, None, true, true);

    // Newer layout: sub-agent transcripts live in sibling files. Only fold them
    // in when the main transcript carried no inline sidechain turns.
    if !saw_inline_sub {
        match subagent_files(path) {
            Determination::Known(files) => {
                for file in files {
                    if let Ok(sub_text) = std::fs::read_to_string(&file) {
                        ingest(&mut agg, &sub_text, Some(AGENT_SUB), false, false);
                    }
                }
            }
            Determination::Undetermined(why) => {
                // The sub-agent spend for this session could not be read: these
                // totals are an under-count. Say so instead of returning them
                // (or an empty `None`) as if they were the whole session.
                agg.subagent_scan = Determination::Undetermined(why);
                return Some(agg);
            }
        }
    }

    if agg.turns == 0 && agg.tools.is_empty() {
        return None;
    }
    Some(agg)
}

/// Fold one transcript's lines into `agg`.
///
/// - `force_bucket`: if set, every turn is attributed to that agent bucket;
///   otherwise the bucket is derived from each line's `isSidechain` flag.
/// - `track_ts`: update `first_ts`/`last_ts` (only the main transcript should).
/// - `count_tools`: tally `tool_use` calls into `agg.tools` (main thread only).
///
/// Returns whether any sub-agent (sidechain) turn was seen.
fn ingest(
    agg: &mut Aggregate,
    text: &str,
    force_bucket: Option<&str>,
    track_ts: bool,
    count_tools: bool,
) -> bool {
    let mut saw_sub = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        if track_ts {
            if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
                if agg.first_ts.is_none() {
                    agg.first_ts = Some(ts.to_string());
                }
                agg.last_ts = Some(ts.to_string());
            }
        }

        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };

        let is_sub = force_bucket == Some(AGENT_SUB)
            || v.get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if is_sub {
            saw_sub = true;
        }
        let bucket = match force_bucket {
            Some(b) => b,
            None if is_sub => AGENT_SUB,
            None => AGENT_MAIN,
        };

        if count_tools {
            if let Some(content) = msg.get("content").and_then(Value::as_array) {
                for p in content {
                    if p.get("type").and_then(Value::as_str) == Some("tool_use") {
                        if let Some(name) = p.get("name").and_then(Value::as_str) {
                            *agg.tools.entry(name.to_string()).or_default() += 1;
                        }
                    }
                }
            }
        }

        let Some(usage) = msg.get("usage") else {
            continue;
        };
        let model = msg
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let get = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mut delta = ModelUsage {
            input: get("input_tokens"),
            output: get("output_tokens"),
            cache_read: get("cache_read_input_tokens"),
            ..Default::default()
        };
        // Split cache-creation tokens by TTL when the breakdown is present;
        // otherwise attribute the whole amount to the 5-minute tier.
        if let Some(cc) = usage.get("cache_creation") {
            let g = |k: &str| cc.get(k).and_then(Value::as_u64).unwrap_or(0);
            delta.cache_write_5m = g("ephemeral_5m_input_tokens");
            delta.cache_write_1h = g("ephemeral_1h_input_tokens");
        } else {
            delta.cache_write_5m = get("cache_creation_input_tokens");
        }

        // Grand total (every caller's existing source of truth) …
        agg.models.entry(model.clone()).or_default().add(&delta);
        agg.turns += 1;
        // … and the per-agent breakdown.
        let agent = agg.agents.entry(bucket.to_string()).or_default();
        agent.models.entry(model).or_default().add(&delta);
        agent.turns += 1;
    }
    saw_sub
}

/// Token usage for a single sub-agent (one `Task` invocation), keyed by its
/// stable `agent_id` and enriched with the `description` / `agent_type` recorded
/// in the `agent-<id>.meta.json` sidecar. Unlike the lumped [`AGENT_SUB`] bucket,
/// this distinguishes individual sub-agents so a caller (e.g. condukt) can
/// attribute cost to the specific worker that ran one task.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SubAgentUsage {
    pub agent_id: String,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub turns: u64,
}

impl SubAgentUsage {
    pub fn total_tokens(&self) -> u64 {
        self.models.values().map(|u| u.total_tokens()).sum()
    }
}

/// Per-sub-agent usage read **live** from the sibling transcript files
/// `<dir>/<stem>/subagents/agent-<id>.jsonl`, keyed by `agent_id`.
///
/// Unlike [`aggregate`] (which lumps every sub-agent into the single
/// [`AGENT_SUB`] bucket), this returns one [`SubAgentUsage`] per `Task`
/// invocation, so cost can be attributed to the individual worker that ran a
/// task — the granularity fugu-router's cost-per-pass routing needs. The
/// `description` / `agent_type` come from the `agent-<id>.meta.json` sidecar
/// written next to each transcript; correlate on `description` (the caller
/// controls the `Task` description).
///
/// Reads the transcript files directly (not the post-Stop store), so it sees
/// the spend of a sub-agent that has just finished, mid-session.
///
/// Three-valued (see [`Determination`]): `Known(vec![])` when the `subagents/`
/// directory is genuinely **absent** (e.g. the older inline-sidechain layout,
/// which is not attributable per agent) or holds no sub-agent with usage;
/// `Undetermined(why)` when the directory exists but could not be enumerated.
/// The two must not collapse — an unreadable directory reported as an empty
/// list is "this session spent nothing on sub-agents", which is a claim this
/// function did not observe.
///
/// Within a successfully enumerated directory, individual unreadable files and
/// per-line parse errors are still skipped.
pub fn subagent_usage(main_transcript: &str) -> Determination<Vec<SubAgentUsage>> {
    subagent_files(main_transcript).map(|files| {
        let mut out = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let mut agg = Aggregate::default();
            ingest(&mut agg, &text, Some(AGENT_SUB), false, false);
            if agg.turns == 0 {
                continue;
            }
            // agent_id from the `agent-<id>.jsonl` stem.
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let agent_id = stem.strip_prefix("agent-").unwrap_or(stem).to_string();
            // Sidecar: agent-<id>.jsonl -> agent-<id>.meta.json.
            let (agent_type, description) = read_subagent_meta(&file.with_extension("meta.json"));
            out.push(SubAgentUsage {
                agent_id,
                agent_type,
                description,
                models: agg.models,
                turns: agg.turns,
            });
        }
        out
    })
}

/// Read `{agentType, description}` from an `agent-<id>.meta.json` sidecar.
/// Returns `(None, None)` if the file is missing or malformed (fail-soft).
fn read_subagent_meta(meta_path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(meta_path) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    (s("agentType"), s("description"))
}

/// Locate sub-agent transcript files for a main transcript at `main_path`:
/// `<dir>/<stem>/subagents/*.jsonl`.
///
/// `Known(vec![])` when the directory is genuinely **absent** (`NotFound`, or a
/// `main_path` with no parent/stem — no sub-agent layout can exist there);
/// `Undetermined(why)` when it exists but `read_dir` failed (permissions, IO)
/// or one of its entries could not be read. A partial listing is never
/// returned: every caller folds these files into a spend total, so a truncated
/// list is an under-count presented as a measurement. `why` carries the
/// directory path and the OS error.
fn subagent_files(main_path: &str) -> Determination<Vec<PathBuf>> {
    let p = Path::new(main_path);
    let (Some(parent), Some(stem)) = (p.parent(), p.file_stem()) else {
        return Determination::Known(Vec::new());
    };
    let dir = parent.join(stem).join("subagents");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No subagents dir at all: this session has no per-agent transcripts.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(Vec::new())
        }
        Err(e) => {
            return Determination::undetermined(format!(
                "sub-agent transcript dir {} could not be read: {e}",
                dir.display()
            ))
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "sub-agent transcript dir {}: a directory entry could not be \
                     read (listing is incomplete): {e}",
                    dir.display()
                ))
            }
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
    Determination::Known(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Resolve a `Determination` in a test whose sub-agent dir is readable by
    /// construction. Panics (rather than substituting an empty list) if it came
    /// back `Undetermined`, so a regression cannot make these assertions pass
    /// vacuously over an empty vec.
    fn known<T>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                panic!("readable subagents dir must be Known, got Undetermined({why:?})")
            }
        }
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "harness-core-usage-{}-{name}.jsonl",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn usage_totals_and_add() {
        let mut u = ModelUsage {
            input: 10,
            output: 20,
            cache_write_5m: 1,
            cache_write_1h: 2,
            cache_read: 3,
        };
        assert_eq!(u.total_tokens(), 36);
        let other = ModelUsage {
            input: 5,
            output: 5,
            ..Default::default()
        };
        u.add(&other);
        assert_eq!(u.input, 15);
        assert_eq!(u.output, 25);
        assert_eq!(u.total_tokens(), 46);
    }

    #[test]
    fn aggregates_multi_model_with_cache_split_and_tools() {
        // Two assistant turns from two different models, plus a tool_use.
        // One usage block carries the 5m/1h cache_creation split; the other
        // uses the flat cache_creation_input_tokens (→ 5m tier).
        let body = concat!(
            r#"{"type":"user","timestamp":"2026-06-22T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Bash"}],"usage":{"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":50,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20}}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:05Z","message":{"model":"claude-sonnet-4-5","content":[{"type":"text","text":"y"}],"usage":{"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":7}}}"#,
            "\n",
        );
        let path = write_temp("multi", body);
        let agg = aggregate(path.to_str().unwrap()).expect("aggregate");

        assert_eq!(agg.turns, 2);
        assert_eq!(agg.tools.get("Bash"), Some(&1));
        assert_eq!(agg.first_ts.as_deref(), Some("2026-06-22T10:00:00Z"));
        assert_eq!(agg.last_ts.as_deref(), Some("2026-06-22T10:00:05Z"));

        let opus = agg.models.get("claude-opus-4-8").unwrap();
        assert_eq!(opus.input, 100);
        assert_eq!(opus.output, 200);
        assert_eq!(opus.cache_read, 50);
        assert_eq!(opus.cache_write_5m, 10);
        assert_eq!(opus.cache_write_1h, 20);

        let sonnet = agg.models.get("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.input, 1);
        assert_eq!(sonnet.output, 2);
        // Flat cache_creation_input_tokens lands in the 5m tier.
        assert_eq!(sonnet.cache_write_5m, 7);
        assert_eq!(sonnet.cache_write_1h, 0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_transcript_yields_none() {
        let path = write_temp("empty", "{\"type\":\"user\",\"message\":{}}\n");
        assert!(aggregate(path.to_str().unwrap()).is_none());
        let _ = std::fs::remove_file(&path);
        assert!(aggregate("").is_none());
        assert!(aggregate("/no/such/transcript.jsonl").is_none());
    }

    #[test]
    fn pure_main_transcript_is_all_main_bucket() {
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","isSidechain":false,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":100,"output_tokens":200}}}"#,
            "\n",
        );
        let path = write_temp("main_only", body);
        let agg = aggregate(path.to_str().unwrap()).expect("aggregate");
        assert_eq!(agg.turns, 1);
        assert_eq!(agg.agents.len(), 1);
        let main = agg.agents.get(AGENT_MAIN).expect("main bucket");
        assert_eq!(main.turns, 1);
        assert_eq!(main.total_tokens(), 300);
        assert!(!agg.agents.contains_key(AGENT_SUB));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn inline_sidechain_splits_main_vs_sub() {
        // Main turn + a sidechain (sub-agent) turn in the SAME transcript.
        let body = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","isSidechain":false,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":100,"output_tokens":100}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:02Z","isSidechain":true,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#,
            "\n",
        );
        let path = write_temp("inline_sub", body);
        let agg = aggregate(path.to_str().unwrap()).expect("aggregate");

        // Grand total includes both.
        assert_eq!(agg.turns, 2);
        assert_eq!(
            agg.models.get("claude-opus-4-8").unwrap().total_tokens(),
            230
        );
        // Split by bucket.
        assert_eq!(agg.agents.get(AGENT_MAIN).unwrap().total_tokens(), 200);
        assert_eq!(agg.agents.get(AGENT_SUB).unwrap().total_tokens(), 30);
        assert_eq!(agg.agents.get(AGENT_SUB).unwrap().turns, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sibling_subagent_files_fold_into_sub_bucket() {
        // Newer layout: main transcript at <dir>/<stem>.jsonl, sub-agents under
        // <dir>/<stem>/subagents/*.jsonl. Build that tree in a temp dir.
        let base =
            std::env::temp_dir().join(format!("harness-core-usage-sib-{}", std::process::id()));
        let stem = "sess";
        let sub_dir = base.join(stem).join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let main_path = base.join(format!("{stem}.jsonl"));
        std::fs::write(
            &main_path,
            concat!(
                r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","isSidechain":false,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":100,"output_tokens":100}}}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            sub_dir.join("agent-aaa.jsonl"),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-06-22T10:00:03Z","isSidechain":true,"message":{"model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":40,"output_tokens":60}}}"#,
                "\n",
            ),
        )
        .unwrap();

        let agg = aggregate(main_path.to_str().unwrap()).expect("aggregate");
        assert_eq!(agg.turns, 2); // 1 main + 1 sub
        assert_eq!(agg.agents.get(AGENT_MAIN).unwrap().total_tokens(), 200);
        assert_eq!(agg.agents.get(AGENT_SUB).unwrap().total_tokens(), 100);
        // Grand total spans both models.
        assert!(agg.models.contains_key("claude-opus-4-8"));
        assert!(agg.models.contains_key("claude-sonnet-4-6"));
        // last_ts comes only from the main transcript (sibling files don't move it).
        assert_eq!(agg.last_ts.as_deref(), Some("2026-06-22T10:00:01Z"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn subagent_usage_splits_per_agent_with_meta() {
        // Two sub-agents under <stem>/subagents/, each its own agent-<id>.jsonl
        // + agent-<id>.meta.json sidecar. subagent_usage must key by agent_id
        // (NOT lump them) and carry the description/agentType from the sidecar.
        let base = std::env::temp_dir().join(format!(
            "harness-core-subusage-{}-{}",
            std::process::id(),
            "x"
        ));
        let stem = "sess";
        let sub_dir = base.join(stem).join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let main_path = base.join(format!("{stem}.jsonl"));
        std::fs::write(&main_path, "{\"type\":\"user\",\"message\":{}}\n").unwrap();

        // Agent t1: opus, 100in/200out.
        std::fs::write(
            sub_dir.join("agent-t1aaa.jsonl"),
            concat!(
                r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":100,"output_tokens":200}}}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            sub_dir.join("agent-t1aaa.meta.json"),
            r#"{"agentType":"condukt:condukt-worker","description":"t1","toolUseId":"toolu_1"}"#,
        )
        .unwrap();

        // Agent t2: haiku, 10in/20out, no meta sidecar (description stays None).
        std::fs::write(
            sub_dir.join("agent-t2bbb.jsonl"),
            concat!(
                r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-haiku-4-5","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#,
                "\n",
            ),
        )
        .unwrap();

        let mut subs = known(subagent_usage(main_path.to_str().unwrap()));
        subs.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        assert_eq!(subs.len(), 2, "one entry per sub-agent");

        let t1 = subs.iter().find(|s| s.agent_id == "t1aaa").unwrap();
        assert_eq!(t1.description.as_deref(), Some("t1"));
        assert_eq!(t1.agent_type.as_deref(), Some("condukt:condukt-worker"));
        assert_eq!(t1.turns, 1);
        assert_eq!(t1.total_tokens(), 300);
        assert!(t1.models.contains_key("claude-opus-4-8"));

        let t2 = subs.iter().find(|s| s.agent_id == "t2bbb").unwrap();
        assert_eq!(t2.description, None, "missing sidecar → no description");
        assert_eq!(t2.total_tokens(), 30);
        assert!(t2.models.contains_key("claude-haiku-4-5"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn subagent_usage_empty_without_subagents_dir() {
        let path = write_temp(
            "no_subdir",
            concat!(
                r#"{"type":"assistant","message":{"model":"claude-opus-4-8","content":[],"usage":{"input_tokens":1,"output_tokens":1}}}"#,
                "\n",
            ),
        );
        assert!(known(subagent_usage(path.to_str().unwrap())).is_empty());
        assert!(known(subagent_usage("")).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
