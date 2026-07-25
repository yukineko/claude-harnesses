//! Append-only episode store: each line is one routing outcome (JSONL).
//!
//! Fail-soft throughout — a malformed line is skipped, a missing file reads as
//! empty, so a corrupt store never breaks routing or a turn.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One routing outcome: a task's features, the model that ran it, and whether it
/// passed verification (plus cost). The k-NN policy learns from these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Episode {
    /// Unix seconds when recorded (0 if unknown).
    #[serde(default)]
    pub ts: u64,
    pub title: String,
    #[serde(default)]
    pub touched_files: Vec<String>,
    #[serde(default)]
    pub class: String,
    pub model: String,
    #[serde(default = "default_role")]
    pub role: String,
    pub pass: bool,
    #[serde(default)]
    pub cost_usd: f64,
    /// Human correction of the verifier's self-label, if any. `Some(true)` =
    /// human says good, `Some(false)` = human says bad. `None` = unlabeled, so
    /// the verifier's `pass` stands. Overrides `pass` in policy aggregation —
    /// human teacher signal de-biases the verifier's self-pass feedback loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_label: Option<bool>,
    /// Who applied `human_label` (e.g. "human"). Provenance only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labeled_by: Option<String>,
    /// Fingerprint of the SKILL.md corpus active when this outcome was recorded,
    /// so outcomes can be stratified by skill version (a silent SKILL.md edit
    /// otherwise makes behaviour drift unattributable). `None` = not captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_fingerprint: Option<String>,
    /// Measured wall-clock duration (seconds) of the worker/verifier task, when
    /// the caller (condukt) knows it. Tri-state: `None` = **unmeasured** (never
    /// conflated with a measured `0.0`), `Some(x)` = a real measurement `x > 0`.
    ///
    /// Back-compat with the ~1112-line legacy JSONL store is explicit: the OLD
    /// writer always serialized this field, writing `0.0` for an unmeasured
    /// task, so a legacy line carries `duration_secs: 0.0` for "unmeasured".
    /// [`de_duration_secs`] therefore maps BOTH an absent field (via
    /// `#[serde(default)]`) AND a present `0.0` (and any non-positive/`null`) to
    /// `None`; only a present strictly-positive number is a measurement. The NEW
    /// writer OMITS the field entirely for `None` (`skip_serializing_if`) rather
    /// than writing `0.0`. Measurement-only, never consulted by routing/scoring
    /// (`policy::route`/`decide_bandit` are unchanged by this field).
    #[serde(
        default,
        deserialize_with = "de_duration_secs",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_secs: Option<f64>,
    /// Which delegation strategy produced this episode (`"fork"` / `"inline"`),
    /// when the caller manually recorded it. `None` = not a delegation-strategy
    /// comparison episode (ordinary worker/verifier record). Measurement-only —
    /// never consulted by `policy::route`/`decide_bandit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<String>,
    /// The routing `Decision`'s `basis` ("learned"|"prior"|"gated") that put
    /// this task on `model`, when the caller (condukt) carried it through from
    /// `route.json`. `None` when not supplied (manual `record`, or a caller
    /// predating this field). Measurement-only — never consulted by
    /// `policy::route`/`decide_bandit`; kept so routing decisions can later be
    /// correlated against `effective_pass()` to check whether the stated basis
    /// actually predicted the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_basis: Option<String>,
    /// The routing `Decision`'s `confidence` ("high"|"low") at the time this
    /// task was routed. Same provenance/measurement-only caveats as
    /// `route_basis`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_confidence: Option<String>,
    /// The routing `Decision`'s free-text `rationale` at the time this task was
    /// routed. Same provenance/measurement-only caveats as `route_basis`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_rationale: Option<String>,
    /// `git diff --stat` insertion/deletion counts for the task's commit(s),
    /// when the caller could measure them before the branch was merged/removed.
    /// Measurement-only — never consulted by routing/scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    /// Token usage of the worker/verifier subagent transcript, when the caller
    /// resolved it (e.g. condukt via `gauge subagents --json` exact agent-id
    /// match, mirroring cost resolution). Measurement-only — never consulted
    /// by routing/scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_output: Option<u64>,
    /// The mode axis ("fast"|"normal"|"high") this episode was routed under,
    /// when the caller (condukt) passed `record --mode`. `None` means **"not
    /// recorded"** — an older episode, or one whose caller didn't pass
    /// `--mode` — and must NEVER be read as "it was `normal`"; those are
    /// different facts (an absent record is not evidence of a value).
    /// Deliberately no `default = "normal"`: that would fabricate a fact.
    /// Measurement-only — never consulted by `policy::route`/`decide_bandit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

fn default_role() -> String {
    "worker".to_string()
}

/// Deserialize `Episode::duration_secs`, collapsing the legacy on-disk encoding
/// onto the tri-state. The OLD writer always serialized the field, writing
/// `0.0` for a task whose duration was never measured; the NEW writer omits it
/// (`skip_serializing_if`). So a present `0.0` (legacy unmeasured), a present
/// `null`, or any non-positive value ALL mean "unmeasured" and map to `None`;
/// only a present strictly-positive number is a real measurement. An entirely
/// absent field is handled by `#[serde(default)]` (which does not call this) and
/// also yields `None`. This is the single point that guarantees not-measured is
/// never read back as measured-as-zero.
fn de_duration_secs<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<f64>::deserialize(d)?;
    Ok(v.filter(|x| *x > 0.0))
}

impl Episode {
    /// The effective pass signal the policy should learn from: the human label
    /// when present, otherwise the verifier's self-reported `pass`.
    pub fn effective_pass(&self) -> bool {
        self.human_label.unwrap_or(self.pass)
    }
}

/// Overwrite the episode store with `eps` (load → modify → save). The store is
/// normally append-only; this is the one explicit admin rewrite (used by
/// `label`, mirroring `dedup`). Writes via a temp file + rename so a crash
/// mid-write can't truncate the store.
pub fn save_all(path: &Path, eps: &[Episode]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        for ep in eps {
            let line = serde_json::to_string(ep).unwrap_or_default();
            writeln!(f, "{line}")?;
        }
    }
    std::fs::rename(&tmp, path)
}

/// Load all episodes, skipping any malformed line.
pub fn load(path: &Path) -> Vec<Episode> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Episode>(l).ok())
        .collect()
}

/// Append one episode as a JSON line, creating parent dirs as needed.
///
/// Writes the body and its trailing newline from a single buffer (one
/// `write_all` call) rather than `writeln!` (which emits two separate
/// `write()` syscalls on an unbuffered `File`). Under `O_APPEND`, two
/// concurrent writers doing body-then-`\n` in two syscalls can interleave as
/// `bodyA · bodyB · \nA · \nB`, corrupting the JSONL log (see
/// `harness_core::append::append_line`, the canonical single-write pattern
/// this mirrors to preserve the `io::Result` signature callers depend on).
pub fn append(path: &Path, ep: &Episode) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(ep).unwrap_or_default();
    // Body + '\n' in ONE buffer → ONE write() syscall → atomic under O_APPEND.
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(&line);
    buf.push('\n');
    f.write_all(buf.as_bytes())
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether any episode of `class` was recorded within the last `within`
/// seconds of `now`. Pure/deterministic (no clock reads) so callers like
/// `audit-recent` can be unit-tested without touching the filesystem or
/// wall-clock time. Used to let a self-report ("I recorded it") be checked
/// against the actual store instead of trusted blindly.
pub fn recorded_within(episodes: &[Episode], class: &str, within: u64, now: u64) -> bool {
    episodes
        .iter()
        .any(|e| e.class == class && now.saturating_sub(e.ts) <= within)
}

/// Return a stable hex content-hash for an Episode (all fields, via canonical JSON).
/// Two Episode values are duplicates iff their content_hash_episode values match.
pub fn content_hash_episode(ep: &Episode) -> String {
    // Serialize in a field-order-stable way: sort keys by serialising to Value then
    // using to_string (serde_json always serialises struct fields in declaration order).
    let canonical = serde_json::to_string(ep).unwrap_or_default();
    hex_sha256(canonical.as_bytes())
}

/// Return a stable hex content-hash for a Playbook entry.
pub fn content_hash_playbook(pb: &Playbook) -> String {
    let canonical = serde_json::to_string(pb).unwrap_or_default();
    hex_sha256(canonical.as_bytes())
}

fn hex_sha256(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Summary of one import/dedup operation on a single store file.
#[derive(Debug, Default)]
pub struct ImportSummary {
    pub read: usize,
    pub new: usize,
    pub skipped: usize,
}

/// Merge episodes from `src` into `dst`, skipping content-identical records.
/// When `dry_run` is true, nothing is written. Returns a summary.
pub fn import_episodes(src: &Path, dst: &Path, dry_run: bool) -> std::io::Result<ImportSummary> {
    let existing = load(dst);
    let existing_hashes: HashSet<String> = existing.iter().map(content_hash_episode).collect();

    let Ok(text) = std::fs::read_to_string(src) else {
        return Ok(ImportSummary::default());
    };
    let src_eps: Vec<Episode> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Episode>(l).ok())
        .collect();

    let mut summary = ImportSummary {
        read: src_eps.len(),
        ..Default::default()
    };
    for ep in &src_eps {
        if existing_hashes.contains(&content_hash_episode(ep)) {
            summary.skipped += 1;
        } else {
            summary.new += 1;
            if !dry_run {
                append(dst, ep)?;
            }
        }
    }
    Ok(summary)
}

/// Merge playbooks from `src` into `dst`, skipping content-identical records.
/// When `dry_run` is true, nothing is written. Returns a summary.
pub fn import_playbooks(src: &Path, dst: &Path, dry_run: bool) -> std::io::Result<ImportSummary> {
    let existing = load_playbooks(dst);
    let existing_hashes: HashSet<String> = existing.iter().map(content_hash_playbook).collect();

    let Ok(text) = std::fs::read_to_string(src) else {
        return Ok(ImportSummary::default());
    };
    let src_pbs: Vec<Playbook> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Playbook>(l).ok())
        .collect();

    let mut summary = ImportSummary {
        read: src_pbs.len(),
        ..Default::default()
    };
    for pb in &src_pbs {
        if existing_hashes.contains(&content_hash_playbook(pb)) {
            summary.skipped += 1;
        } else {
            summary.new += 1;
            if !dry_run {
                append_playbook(dst, pb)?;
            }
        }
    }
    Ok(summary)
}

/// Rewrite `path` in place, removing duplicate Episode records (first-seen wins).
/// Uses an atomic write (temp file + rename) to avoid corruption on error.
pub fn dedup_episodes(path: &Path) -> std::io::Result<ImportSummary> {
    let eps = load(path);
    let total = eps.len();
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<&Episode> = Vec::new();
    for ep in &eps {
        if seen.insert(content_hash_episode(ep)) {
            unique.push(ep);
        }
    }
    let skipped = total - unique.len();
    if skipped > 0 {
        atomic_write_jsonl(path, &unique, serde_json::to_string)?;
    }
    Ok(ImportSummary {
        read: total,
        new: unique.len(),
        skipped,
    })
}

/// Rewrite `path` in place, removing duplicate Playbook records (first-seen wins).
pub fn dedup_playbooks(path: &Path) -> std::io::Result<ImportSummary> {
    let pbs = load_playbooks(path);
    let total = pbs.len();
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<&Playbook> = Vec::new();
    for pb in &pbs {
        if seen.insert(content_hash_playbook(pb)) {
            unique.push(pb);
        }
    }
    let skipped = total - unique.len();
    if skipped > 0 {
        atomic_write_jsonl(path, &unique, serde_json::to_string)?;
    }
    Ok(ImportSummary {
        read: total,
        new: unique.len(),
        skipped,
    })
}

/// Write `items` as JSONL to a temp file next to `path`, then rename atomically.
fn atomic_write_jsonl<T, F>(path: &Path, items: &[T], serialize: F) -> std::io::Result<()>
where
    F: Fn(&T) -> Result<String, serde_json::Error>,
{
    // Place the temp file in the same directory so rename is same-fs.
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(
        ".fugu-router-tmp-{}.jsonl",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        for item in items {
            let line = serialize(item).unwrap_or_default();
            writeln!(f, "{line}")?;
        }
        f.flush()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// A verified task's procedure record — stored separately from Episodes so
/// routing statistics stay unaffected by the larger procedure text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    #[serde(default)]
    pub ts: u64,
    pub title: String,
    #[serde(default)]
    pub touched_files: Vec<String>,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub done_criteria: String,
    #[serde(default)]
    pub notes: String,
}

/// Load all playbook entries, skipping any malformed line.
pub fn load_playbooks(path: &Path) -> Vec<Playbook> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Playbook>(l).ok())
        .collect()
}

/// Append one playbook entry as a JSON line, creating parent dirs as needed.
///
/// Mirrors [`append`]'s single-`write_all` pattern (body + `\n` in one buffer,
/// one `write()` syscall) instead of `writeln!`'s two syscalls, which can
/// interleave under `O_APPEND` with a concurrent writer. Serialization
/// failure is propagated as an `io::Result` error instead of silently writing
/// an empty line (the previous `unwrap_or_default()` behavior).
pub fn append_playbook(path: &Path, pb: &Playbook) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(pb)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(&line);
    buf.push('\n');
    f.write_all(buf.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ep(title: &str, model: &str) -> Episode {
        Episode {
            ts: 1,
            title: title.into(),
            touched_files: vec!["src/lib.rs".into()],
            class: "parallel".into(),
            model: model.into(),
            role: "worker".into(),
            pass: true,
            cost_usd: 0.01,
            human_label: None,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: None,
            delegation: None,
            ..Default::default()
        }
    }

    #[test]
    fn effective_pass_prefers_human_label() {
        let mut ep = sample_ep("t", "sonnet"); // verifier pass: true
        assert!(ep.effective_pass());
        ep.human_label = Some(false); // human overrides good → bad
        assert!(!ep.effective_pass());
        ep.pass = false;
        ep.human_label = Some(true); // human rescues a failed episode
        assert!(ep.effective_pass());
    }

    #[test]
    fn recorded_within_finds_matching_class_inside_window() {
        let mut ep = sample_ep("flow batch", "sonnet");
        ep.class = "flow-delegation".into();
        ep.ts = 1000;
        assert!(recorded_within(&[ep], "flow-delegation", 60, 1030));
    }

    #[test]
    fn recorded_within_misses_when_window_too_short() {
        let mut ep = sample_ep("flow batch", "sonnet");
        ep.class = "flow-delegation".into();
        ep.ts = 1000;
        assert!(!recorded_within(&[ep], "flow-delegation", 10, 1030));
    }

    #[test]
    fn recorded_within_ignores_other_classes() {
        let ep = sample_ep("t", "sonnet"); // class: "parallel"
        assert!(!recorded_within(
            &[ep],
            "flow-delegation",
            u64::MAX,
            1_000_000
        ));
    }

    #[test]
    fn save_all_rewrites_store_with_label() {
        let dir = std::env::temp_dir().join(format!("fugu-saveall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("episodes.jsonl");
        let _ = std::fs::remove_file(&path);
        let mut a = sample_ep("alpha", "sonnet");
        let b = sample_ep("beta", "haiku");
        append(&path, &a).unwrap();
        append(&path, &b).unwrap();
        a.human_label = Some(false);
        a.labeled_by = Some("human".into());
        save_all(&path, &[a, b]).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].human_label, Some(false));
        assert_eq!(loaded[0].labeled_by.as_deref(), Some("human"));
        assert!(loaded[1].human_label.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn sample_pb(title: &str) -> Playbook {
        Playbook {
            ts: 1,
            title: title.into(),
            touched_files: vec!["src/lib.rs".into()],
            class: "parallel".into(),
            done_criteria: "tests pass".into(),
            notes: "".into(),
        }
    }

    #[test]
    fn content_hash_episode_identical_records_match() {
        let ep1 = sample_ep("add auth", "sonnet");
        let ep2 = sample_ep("add auth", "sonnet");
        assert_eq!(content_hash_episode(&ep1), content_hash_episode(&ep2));
    }

    #[test]
    fn content_hash_episode_distinct_records_differ() {
        let ep1 = sample_ep("add auth", "sonnet");
        let ep2 = sample_ep("add billing", "sonnet");
        assert_ne!(content_hash_episode(&ep1), content_hash_episode(&ep2));
        // model difference also distinguishes
        let ep3 = sample_ep("add auth", "opus");
        assert_ne!(content_hash_episode(&ep1), content_hash_episode(&ep3));
    }

    #[test]
    fn import_episodes_deduplicates() {
        let dir = std::env::temp_dir().join("fugu-router-import-ep-test");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("src.jsonl");
        let dst = dir.join("dst.jsonl");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);

        let ep_a = sample_ep("add auth", "sonnet");
        let ep_b = sample_ep("add billing", "haiku");

        // dst already has ep_a
        append(&dst, &ep_a).unwrap();
        // src has ep_a (duplicate) + ep_b (new)
        append(&src, &ep_a).unwrap();
        append(&src, &ep_b).unwrap();

        let summary = import_episodes(&src, &dst, false).unwrap();
        assert_eq!(summary.read, 2);
        assert_eq!(summary.new, 1);
        assert_eq!(summary.skipped, 1);

        let loaded = load(&dst);
        assert_eq!(loaded.len(), 2, "dst should have exactly 2 unique episodes");
        assert!(loaded.iter().any(|e| e.title == "add auth"));
        assert!(loaded.iter().any(|e| e.title == "add billing"));
    }

    #[test]
    fn import_episodes_dry_run_writes_nothing() {
        let dir = std::env::temp_dir().join("fugu-router-import-dry-test");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("src.jsonl");
        let dst = dir.join("dst.jsonl");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);

        let ep_a = sample_ep("add auth", "sonnet");
        let ep_b = sample_ep("fix login", "haiku");
        append(&src, &ep_a).unwrap();
        append(&src, &ep_b).unwrap();

        let summary = import_episodes(&src, &dst, true).unwrap();
        assert_eq!(summary.new, 2);
        // dry run: dst should still be empty / not exist
        assert!(load(&dst).is_empty());
    }

    #[test]
    fn dedup_episodes_removes_duplicates_preserves_order() {
        let dir = std::env::temp_dir().join("fugu-router-dedup-ep-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("episodes.jsonl");
        let _ = std::fs::remove_file(&path);

        let ep_a = sample_ep("add auth", "sonnet");
        let ep_b = sample_ep("add billing", "haiku");
        append(&path, &ep_a).unwrap();
        append(&path, &ep_b).unwrap();
        append(&path, &ep_a).unwrap(); // duplicate

        let summary = dedup_episodes(&path).unwrap();
        assert_eq!(summary.read, 3);
        assert_eq!(summary.new, 2);
        assert_eq!(summary.skipped, 1);

        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].title, "add auth");
        assert_eq!(loaded[1].title, "add billing");
    }

    #[test]
    fn dedup_playbooks_removes_duplicates() {
        let dir = std::env::temp_dir().join("fugu-router-dedup-pb-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("playbooks.jsonl");
        let _ = std::fs::remove_file(&path);

        let pb_a = sample_pb("add auth");
        let pb_b = sample_pb("add billing");
        append_playbook(&path, &pb_a).unwrap();
        append_playbook(&path, &pb_b).unwrap();
        append_playbook(&path, &pb_a).unwrap(); // duplicate

        let summary = dedup_playbooks(&path).unwrap();
        assert_eq!(summary.read, 3);
        assert_eq!(summary.new, 2);
        assert_eq!(summary.skipped, 1);

        let loaded = load_playbooks(&path);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn playbook_roundtrip() {
        let dir = std::env::temp_dir().join("fugu-router-playbook-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("playbooks.jsonl");
        let _ = std::fs::remove_file(&path);
        let pb = Playbook {
            ts: 42,
            title: "add auth endpoint".into(),
            touched_files: vec!["src/auth.rs".into()],
            class: "serial".into(),
            done_criteria: "cargo test passes".into(),
            notes: "use bcrypt".into(),
        };
        append_playbook(&path, &pb).unwrap();
        // malformed line must be skipped
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"garbage\n")
            .unwrap();
        append_playbook(&path, &pb).unwrap();
        let loaded = load_playbooks(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].title, "add auth endpoint");
        assert_eq!(loaded[0].done_criteria, "cargo test passes");
    }

    #[test]
    fn load_playbooks_missing_file_returns_empty() {
        let path = std::path::PathBuf::from("/tmp/nonexistent_playbooks_12345.jsonl");
        assert!(load_playbooks(&path).is_empty());
    }

    #[test]
    fn skips_malformed_lines() {
        let dir = std::env::temp_dir().join("fugu-router-store-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("episodes.jsonl");
        let _ = std::fs::remove_file(&path);
        let ep = Episode {
            ts: 1,
            title: "add login endpoint".into(),
            touched_files: vec!["src/auth/login.ts".into()],
            class: "parallel".into(),
            model: "sonnet".into(),
            role: "worker".into(),
            pass: true,
            cost_usd: 0.12,
            human_label: None,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: None,
            delegation: None,
            ..Default::default()
        };
        append(&path, &ep).unwrap();
        // a junk line must not break the load
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        append(&path, &ep).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].model, "sonnet");
    }

    /// Regression guard: concurrent `append()` calls (mirroring separate
    /// subagent processes each with their own `O_APPEND` handle) must never
    /// interleave a body and a trailing newline from two different writers
    /// onto the same physical line. With the old `writeln!` (body then `\n`
    /// as two syscalls) this could concatenate two JSON objects onto one
    /// line; with the single-buffer `write_all` every line stays exactly one
    /// parseable JSON object and the record count is exact.
    #[test]
    fn concurrent_append_never_interleaves_records() {
        let dir = std::env::temp_dir().join(format!(
            "fugu-router-concurrent-append-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("episodes.jsonl");
        let _ = std::fs::remove_file(&path);

        const THREADS: usize = 8;
        const PER_THREAD: usize = 250;

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let path = path.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        // Vary payload length so a torn write would land at
                        // different offsets, not just a fixed boundary.
                        let pad = "x".repeat(i % 64);
                        let ep = Episode {
                            ts: i as u64,
                            title: format!("t{t}-i{i}-{pad}"),
                            touched_files: vec![],
                            class: "parallel".into(),
                            model: "sonnet".into(),
                            role: "worker".into(),
                            pass: true,
                            cost_usd: 0.0,
                            human_label: None,
                            labeled_by: None,
                            skill_fingerprint: None,
                            duration_secs: None,
                            delegation: None,
                            ..Default::default()
                        };
                        append(&path, &ep).unwrap();
                    }
                });
            }
        });

        let text = std::fs::read_to_string(&path).unwrap();
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("line {n} is not a single JSON object: {e} — {l:.120}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "every appended record must survive as exactly one parseable line"
        );

        // load() must also see every record, none merged or dropped.
        let loaded = load(&path);
        assert_eq!(loaded.len(), THREADS * PER_THREAD);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Same regression guard as `concurrent_append_never_interleaves_records`,
    /// but for `append_playbook` — it used to write via `writeln!` (two
    /// syscalls under `O_APPEND`), the same interleaving hazard fixed for
    /// `append`. Single-buffer `write_all` keeps every line one parseable
    /// JSON object with an exact record count.
    #[test]
    fn concurrent_append_playbook_never_interleaves_records() {
        let dir = std::env::temp_dir().join(format!(
            "fugu-router-concurrent-append-playbook-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("playbooks.jsonl");
        let _ = std::fs::remove_file(&path);

        const THREADS: usize = 8;
        const PER_THREAD: usize = 250;

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let path = path.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        let pad = "x".repeat(i % 64);
                        let pb = Playbook {
                            ts: i as u64,
                            title: format!("t{t}-i{i}-{pad}"),
                            touched_files: vec![],
                            class: "parallel".into(),
                            done_criteria: "criteria".into(),
                            notes: String::new(),
                        };
                        append_playbook(&path, &pb).unwrap();
                    }
                });
            }
        });

        let text = std::fs::read_to_string(&path).unwrap();
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("line {n} is not a single JSON object: {e} — {l:.120}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "every appended playbook record must survive as exactly one parseable line"
        );

        let loaded = load_playbooks(&path);
        assert_eq!(loaded.len(), THREADS * PER_THREAD);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skill_fingerprint_roundtrips_and_omits_when_none() {
        // Some(..) survives a serialize → deserialize round-trip.
        let mut ep = sample_ep("add auth", "sonnet");
        ep.skill_fingerprint = Some("deadbeefcafef00d".into());
        let line = serde_json::to_string(&ep).unwrap();
        assert!(line.contains("\"skill_fingerprint\":\"deadbeefcafef00d\""));
        let back: Episode = serde_json::from_str(&line).unwrap();
        assert_eq!(back.skill_fingerprint.as_deref(), Some("deadbeefcafef00d"));

        // None: the key is skipped entirely (skip_serializing_if), and an OLD
        // episode JSON without the field still parses (serde default).
        let none_ep = sample_ep("add billing", "haiku");
        let none_line = serde_json::to_string(&none_ep).unwrap();
        assert!(!none_line.contains("skill_fingerprint"));
        let back_none: Episode = serde_json::from_str(&none_line).unwrap();
        assert!(back_none.skill_fingerprint.is_none());
    }

    #[test]
    fn duration_secs_tristate_roundtrip_and_legacy_zero_is_unmeasured() {
        // A real positive measurement round-trips as Some(x).
        let mut ep = sample_ep("add auth", "sonnet");
        ep.duration_secs = Some(42.5);
        let line = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&line).unwrap();
        assert_eq!(back.duration_secs, Some(42.5));

        // The NEW writer OMITS the field for an unmeasured (None) episode
        // rather than writing 0.0.
        let mut none_ep = sample_ep("no duration", "sonnet");
        none_ep.duration_secs = None;
        let none_line = serde_json::to_string(&none_ep).unwrap();
        assert!(
            !none_line.contains("duration_secs"),
            "unmeasured episode must omit the field, got: {none_line}"
        );
        let back_none: Episode = serde_json::from_str(&none_line).unwrap();
        assert_eq!(back_none.duration_secs, None);

        // An OLD line predating the field (absent key) → unmeasured (None).
        let old_absent = r#"{"ts":1,"title":"legacy","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#;
        let back_absent: Episode = serde_json::from_str(old_absent).unwrap();
        assert_eq!(
            back_absent.duration_secs, None,
            "an absent duration_secs must read as unmeasured"
        );

        // A LEGACY line that explicitly wrote 0.0 for an unmeasured task MUST
        // also read as unmeasured (None), NOT as a measured 0.0 — the whole
        // point of the tri-state (not-measured != measured-as-zero).
        let legacy_zero = r#"{"ts":1,"title":"legacy","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0,"duration_secs":0.0}"#;
        let back_zero: Episode = serde_json::from_str(legacy_zero).unwrap();
        assert_eq!(
            back_zero.duration_secs, None,
            "a legacy present 0.0 must read as unmeasured, never Some(0.0)"
        );

        // A present positive value still round-trips from disk.
        let legacy_pos = r#"{"ts":1,"title":"m","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0,"duration_secs":7.5}"#;
        let back_pos: Episode = serde_json::from_str(legacy_pos).unwrap();
        assert_eq!(back_pos.duration_secs, Some(7.5));
    }

    #[test]
    fn delegation_roundtrips_and_defaults_to_none_on_old_lines() {
        let mut ep = sample_ep("run condukt via fork", "sonnet");
        ep.delegation = Some("fork".to_string());
        let line = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&line).unwrap();
        assert_eq!(back.delegation.as_deref(), Some("fork"));

        // An OLD episode JSON line recorded before this field existed has no
        // "delegation" key at all — it must still parse, defaulting to None
        // rather than failing to deserialize.
        let old_line = r#"{"ts":1,"title":"legacy task","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#;
        let back_old: Episode = serde_json::from_str(old_line).unwrap();
        assert!(back_old.delegation.is_none());
    }

    /// The `mode` field must round-trip, default to `None` on an old JSON
    /// line that predates it, AND — the fact this field exists to
    /// protect — an absent `mode` must never be conflated with `Some("normal")`.
    #[test]
    fn mode_roundtrips_and_absence_is_not_confused_with_normal() {
        let mut ep = sample_ep("wire the login endpoint", "sonnet");
        ep.mode = Some("high".to_string());
        let line = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&line).unwrap();
        assert_eq!(back.mode.as_deref(), Some("high"));

        // An OLD episode JSON line recorded before this field existed has no
        // "mode" key at all — it must parse to None ("not recorded"), which
        // is a distinct fact from Some("normal") ("recorded as normal").
        let old_line = r#"{"ts":1,"title":"legacy task","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#;
        let back_old: Episode = serde_json::from_str(old_line).unwrap();
        assert!(back_old.mode.is_none());
        assert_ne!(back_old.mode.as_deref(), Some("normal"));
    }

    #[test]
    fn mode_omitted_from_serialized_json_when_none() {
        let ep = sample_ep("ordinary worker task", "sonnet");
        assert!(ep.mode.is_none());
        let line = serde_json::to_string(&ep).unwrap();
        assert!(
            !line.contains("\"mode\""),
            "unset mode must be skipped from serialization, got: {line}"
        );
    }

    #[test]
    fn delegation_omitted_from_serialized_json_when_none() {
        let ep = sample_ep("ordinary worker task", "sonnet");
        assert!(ep.delegation.is_none());
        let line = serde_json::to_string(&ep).unwrap();
        assert!(
            !line.contains("delegation"),
            "unset delegation must be skipped from serialization (skip_serializing_if), got: {line}"
        );
    }

    /// Routing-provenance + lines-changed + token-usage fields round-trip, and
    /// an OLD episode JSON line recorded before these fields existed still
    /// parses (defaulting to None) rather than failing to deserialize.
    #[test]
    fn routing_provenance_and_measurement_fields_roundtrip_and_default_on_old_lines() {
        let mut ep = sample_ep("wire the login endpoint", "sonnet");
        ep.route_basis = Some("learned".to_string());
        ep.route_confidence = Some("high".to_string());
        ep.route_rationale = Some("Thompson(cost-adj): sonnet cleared 80%".to_string());
        ep.lines_added = Some(42);
        ep.lines_removed = Some(7);
        ep.tokens_input = Some(12_345);
        ep.tokens_output = Some(678);
        let line = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&line).unwrap();
        assert_eq!(back.route_basis.as_deref(), Some("learned"));
        assert_eq!(back.route_confidence.as_deref(), Some("high"));
        assert_eq!(
            back.route_rationale.as_deref(),
            Some("Thompson(cost-adj): sonnet cleared 80%")
        );
        assert_eq!(back.lines_added, Some(42));
        assert_eq!(back.lines_removed, Some(7));
        assert_eq!(back.tokens_input, Some(12_345));
        assert_eq!(back.tokens_output, Some(678));

        let old_line = r#"{"ts":1,"title":"legacy task","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#;
        let back_old: Episode = serde_json::from_str(old_line).unwrap();
        assert!(back_old.route_basis.is_none());
        assert!(back_old.route_confidence.is_none());
        assert!(back_old.route_rationale.is_none());
        assert!(back_old.lines_added.is_none());
        assert!(back_old.lines_removed.is_none());
        assert!(back_old.tokens_input.is_none());
        assert!(back_old.tokens_output.is_none());
    }

    #[test]
    fn routing_provenance_and_measurement_fields_omitted_from_json_when_none() {
        let ep = sample_ep("ordinary worker task", "sonnet");
        let line = serde_json::to_string(&ep).unwrap();
        for key in [
            "route_basis",
            "route_confidence",
            "route_rationale",
            "lines_added",
            "lines_removed",
            "tokens_input",
            "tokens_output",
        ] {
            assert!(
                !line.contains(key),
                "unset {key} must be skipped from serialization, got: {line}"
            );
        }
    }
}
