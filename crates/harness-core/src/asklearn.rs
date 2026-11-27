//! Ask learning: the pure, shared half of "turn the Asks a human already
//! answered into blastguard Allow/Deny decisions".
//!
//! Spec: `docs/specs/blastguard-ask-learning.md` (user ruling 2026-11-28).
//!
//! * The unit of learning is a [`Shape`]: `rule_id` + statically resolved
//!   [`TargetClass`] + the command's verbs.
//! * [`learn`] folds [`OutcomeRecord`]s into a [`LearnedTable`]. It is a pure,
//!   deterministic function (same multiset of records, any order, gives a
//!   byte-identical serialization; no timestamp is stored in the table).
//! * [`read_table`] reads the table back with a three-valued answer
//!   ([`TableRead`]): an unreadable, unparsable or wrong-version file is
//!   `Undetermined`, never an empty table and never an Allow (CLAUDE.md §3).
//!
//! Who uses it: the learner (`overwatch ask-learn`) writes the table;
//! blastguard only READS it, and only for a call it would otherwise `Ask`
//! about. This module never decides a verdict: it describes what humans
//! answered. The hard guards (never override a Deny, never Allow system harm
//! or a System / ProtectedConfig target) are enforced here ([`learn`],
//! [`lookup`]) AND again in blastguard itself, because the table file is
//! untrusted input to the verdict side.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Env override of the learned table path (tests, operators).
pub const TABLE_VAR: &str = "BLASTGUARD_LEARNED_TABLE";

/// The on-disk table version this build reads and writes.
pub const TABLE_VERSION: u32 = 1;

/// Approvals needed for a learned Allow.
pub const MIN_APPROVALS: u64 = 3;
/// Distinct sessions needed for a learned Allow.
pub const MIN_SESSIONS: u64 = 2;

/// A sample command is truncated to this many characters in the table.
pub const SAMPLE_CHARS: usize = 300;
/// At most this many samples per entry.
pub const MAX_SAMPLES: usize = 3;

/// The verb recorded for a command word that could not be resolved
/// statically. A shape holding it is never learned as Allow (a shape that
/// names "some command" cannot be generalised), but a Deny can still learn.
pub const UNRESOLVED_VERB: &str = "?";

/// Where the shape's statically resolved target sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TargetClass {
    /// Inside a git linked worktree / the session worktree.
    Worktree,
    /// Inside a session scratchpad.
    Scratchpad,
    /// Gate / hook / policy / shell-rc config. Never auto-Allowed.
    ProtectedConfig,
    /// A system directory. Never auto-Allowed.
    System,
    /// Any other path under `$HOME`.
    Home,
    /// The target could not be statically resolved.
    Undetermined,
    /// The call has no write target.
    None,
}

impl TargetClass {
    /// The class segment of the shape key. Distinct, separator-free.
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetClass::Worktree => "worktree",
            TargetClass::Scratchpad => "scratchpad",
            TargetClass::ProtectedConfig => "protected-config",
            TargetClass::System => "system",
            TargetClass::Home => "home",
            TargetClass::Undetermined => "undetermined",
            TargetClass::None => "none",
        }
    }

    /// Inverse of [`TargetClass::as_str`].
    pub fn parse(s: &str) -> Option<TargetClass> {
        Some(match s {
            "worktree" => TargetClass::Worktree,
            "scratchpad" => TargetClass::Scratchpad,
            "protected-config" => TargetClass::ProtectedConfig,
            "system" => TargetClass::System,
            "home" => TargetClass::Home,
            "undetermined" => TargetClass::Undetermined,
            "none" => TargetClass::None,
            _ => return Option::None,
        })
    }

    /// Classes that are never learned as Allow (deny learning still applies).
    pub fn never_allowed(&self) -> bool {
        matches!(self, TargetClass::System | TargetClass::ProtectedConfig)
    }
}

/// The learning unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shape {
    pub rule_id: String,
    pub target_class: TargetClass,
    pub verbs: Vec<String>,
}

fn clean_segment(s: &str) -> String {
    s.chars()
        .map(|c| if matches!(c, '|' | ',') { '_' } else { c })
        .collect()
}

impl Shape {
    /// Canonical key `"<rule_id>|<class>|<verb,verb,...>"`: verbs sorted and
    /// deduplicated; a `|` or `,` inside a verb or rule id is replaced so the
    /// key always splits into exactly three segments.
    pub fn key(&self) -> String {
        let verbs: BTreeSet<String> = self.verbs.iter().map(|v| clean_segment(v)).collect();
        format!(
            "{}|{}|{}",
            clean_segment(&self.rule_id),
            self.target_class.as_str(),
            verbs.into_iter().collect::<Vec<_>>().join(",")
        )
    }

    /// Parse a key produced by [`Shape::key`].
    pub fn from_key(key: &str) -> Option<Shape> {
        let mut it = key.split('|');
        let rule = it.next()?;
        let class = TargetClass::parse(it.next()?)?;
        let verbs = it.next()?;
        if it.next().is_some() {
            return Option::None;
        }
        Some(Shape {
            rule_id: rule.to_string(),
            target_class: class,
            verbs: verbs.split(',').map(str::to_string).collect(),
        })
    }

    /// True when a command word could not be resolved ([`UNRESOLVED_VERB`])
    /// or there is no verb at all.
    pub fn has_unresolved_verb(&self) -> bool {
        self.verbs.is_empty()
            || self
                .verbs
                .iter()
                .any(|v| v == UNRESOLVED_VERB || v.is_empty())
    }

    /// Whether a learned Allow may ever apply to this shape, independent of
    /// any table content.
    pub fn allow_permitted(&self) -> bool {
        !self.target_class.never_allowed() && !self.has_unresolved_verb()
    }
}

/// What the human answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    Declined,
}

/// One answered Ask.
#[derive(Debug, Clone)]
pub struct OutcomeRecord {
    pub shape: Shape,
    pub session_id: String,
    pub outcome: Outcome,
    pub ts: u64,
    pub sample_command: String,
    /// The analyser reported system harm for this call. Never learned Allow.
    pub system_harm: bool,
}

/// `allow` or `deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Deny,
}

/// One table entry. Field order is the serialization order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub shape: String,
    pub decision: Verdict,
    pub approvals: u64,
    pub declines: u64,
    pub sessions: u64,
    pub samples: Vec<String>,
}

/// The learned table. Sorted by shape key; no timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedTable {
    pub version: u32,
    pub entries: Vec<Entry>,
}

impl LearnedTable {
    pub fn empty() -> LearnedTable {
        LearnedTable {
            version: TABLE_VERSION,
            entries: Vec::new(),
        }
    }
}

/// What the table says about a shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LearnedDecision {
    Allow {
        shape: String,
        approvals: u64,
        sessions: u64,
        declines: u64,
    },
    Deny {
        shape: String,
        approvals: u64,
        sessions: u64,
        declines: u64,
    },
}

#[derive(Default)]
struct Acc {
    approvals: u64,
    declines: u64,
    sessions: BTreeSet<String>,
    approved_sessions: BTreeSet<String>,
    harm: bool,
    samples: BTreeSet<String>,
}

/// Fold records into a table. Deterministic: independent of record order.
///
/// * any decline -> `deny`;
/// * otherwise >= [`MIN_APPROVALS`] approvals from >= [`MIN_SESSIONS`]
///   distinct sessions, no system-harm record, and a shape for which
///   [`Shape::allow_permitted`] holds -> `allow`;
/// * otherwise no entry.
pub fn learn(records: &[OutcomeRecord]) -> LearnedTable {
    let mut by_shape: BTreeMap<String, (Shape, Acc)> = BTreeMap::new();
    for r in records {
        let (_, acc) = by_shape
            .entry(r.shape.key())
            .or_insert_with(|| (r.shape.clone(), Acc::default()));
        acc.sessions.insert(r.session_id.clone());
        acc.harm |= r.system_harm;
        let sample: String = r.sample_command.chars().take(SAMPLE_CHARS).collect();
        acc.samples.insert(sample);
        match r.outcome {
            Outcome::Approved => {
                acc.approvals += 1;
                acc.approved_sessions.insert(r.session_id.clone());
            }
            Outcome::Declined => acc.declines += 1,
        }
    }
    let mut entries = Vec::new();
    for (key, (shape, acc)) in by_shape {
        let decision = if acc.declines >= 1 {
            Verdict::Deny
        } else if acc.approvals >= MIN_APPROVALS
            && acc.approved_sessions.len() as u64 >= MIN_SESSIONS
            && !acc.harm
            && shape.allow_permitted()
        {
            Verdict::Allow
        } else {
            continue;
        };
        entries.push(Entry {
            shape: key,
            decision,
            approvals: acc.approvals,
            declines: acc.declines,
            sessions: acc.sessions.len() as u64,
            samples: acc.samples.into_iter().take(MAX_SAMPLES).collect(),
        });
    }
    LearnedTable {
        version: TABLE_VERSION,
        entries,
    }
}

/// Look `shape` up. An `allow` entry for a shape that may never be Allowed
/// ([`Shape::allow_permitted`]) is ignored: the table is untrusted input.
pub fn lookup(table: &LearnedTable, shape: &Shape) -> Option<LearnedDecision> {
    let key = shape.key();
    let e = table.entries.iter().find(|e| e.shape == key)?;
    match e.decision {
        Verdict::Allow if !shape.allow_permitted() => Option::None,
        Verdict::Allow => Some(LearnedDecision::Allow {
            shape: key,
            approvals: e.approvals,
            sessions: e.sessions,
            declines: e.declines,
        }),
        Verdict::Deny => Some(LearnedDecision::Deny {
            shape: key,
            approvals: e.approvals,
            sessions: e.sessions,
            declines: e.declines,
        }),
    }
}

/// Three-valued read of the table file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableRead {
    /// No file. Nothing learned yet.
    Absent,
    /// Unreadable, unparsable or wrong version. NOT an empty table, and never
    /// an Allow; the string says why.
    Undetermined(String),
    Known(LearnedTable),
}

/// Parse table text. `Err` carries the reason.
pub fn parse_table(text: &str) -> Result<LearnedTable, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    match v.get("version").and_then(serde_json::Value::as_u64) {
        Some(n) if n == u64::from(TABLE_VERSION) => {}
        Some(n) => return Err(format!("unsupported table version {n}")),
        Option::None => return Err("no numeric `version`".to_string()),
    }
    serde_json::from_value::<LearnedTable>(v).map_err(|e| format!("malformed table: {e}"))
}

/// Read the table at `path`.
pub fn read_table(path: &Path) -> TableRead {
    match std::fs::read_to_string(path) {
        Ok(text) => match parse_table(&text) {
            Ok(t) => TableRead::Known(t),
            Err(why) => TableRead::Undetermined(format!("{}: {why}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TableRead::Absent,
        Err(e) => TableRead::Undetermined(format!("{}: {e}", path.display())),
    }
}

/// The table path: `BLASTGUARD_LEARNED_TABLE` (non-blank), else
/// `~/.blastguard/learned/table.json`.
pub fn table_path() -> PathBuf {
    if let Ok(p) = std::env::var(TABLE_VAR) {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    learned_dir().join("table.json")
}

/// `~/.blastguard/learned` (status.json / promotions.jsonl live here too).
pub fn learned_dir() -> PathBuf {
    crate::config::base_dir("blastguard").join("learned")
}
