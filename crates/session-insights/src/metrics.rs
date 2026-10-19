//! Per-session rollup: a small JSON file under the state dir, updated in place on
//! each tool call and turn. Derives a size class (XS–XL) and a work category from
//! the recorded tool mix — the deterministic analogue of Devin's Session Insights.

use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub last_at: String,
    /// Assistant turns completed (Stop events).
    #[serde(default)]
    pub turns: u64,
    /// Total non-ignored tool events recorded.
    #[serde(default)]
    pub tool_events: u64,
    /// Per-tool counts.
    #[serde(default)]
    pub tools: BTreeMap<String, u64>,
    /// Distinct files touched (Edit/Write/Read targets).
    #[serde(default)]
    pub files: Vec<String>,
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

fn path_for(cfg: &Config, session: &str) -> PathBuf {
    cfg.state_dir.join(format!("{session}.json"))
}

pub fn load(cfg: &Config, session: &str) -> Session {
    harness_core::store::load_json(&path_for(cfg, session))
}

pub fn save(cfg: &Config, session: &str, s: &Session) {
    let p = path_for(cfg, session);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(p, text);
    }
}

impl Session {
    /// Ensure identity/started fields are set (idempotent).
    ///
    /// `session_id`/`project`/`cwd` are session identity: they are pinned on the
    /// first call (session start) and never overwritten afterwards. This keeps
    /// the project name stable even if the working directory changes mid-session
    /// (e.g. a `cd` into a subdir), which would otherwise make the record note
    /// name flip between tool events. Only `last_at` is refreshed every call.
    pub fn ensure(&mut self, session_id: &str, project: &str, cwd: &str) {
        if self.started_at.is_empty() {
            self.started_at = now();
            self.session_id = session_id.to_string();
            self.project = project.to_string();
            self.cwd = cwd.to_string();
        }
        self.last_at = now();
    }

    pub fn record_tool(&mut self, tool: &str, target: Option<String>) {
        self.tool_events += 1;
        *self.tools.entry(tool.to_string()).or_insert(0) += 1;
        if let Some(f) = target {
            if !self.files.contains(&f) {
                self.files.push(f);
            }
        }
        self.last_at = now();
    }

    pub fn record_turn(&mut self) {
        self.turns += 1;
        self.last_at = now();
    }

    /// XS / S / M / L / XL from total tool events against the thresholds.
    pub fn size(&self, thresholds: &[usize; 4]) -> &'static str {
        let n = self.tool_events as usize;
        if n < thresholds[0] {
            "XS"
        } else if n < thresholds[1] {
            "S"
        } else if n < thresholds[2] {
            "M"
        } else if n < thresholds[3] {
            "L"
        } else {
            "XL"
        }
    }

    /// Coarse work category from the dominant tool group.
    pub fn category(&self) -> &'static str {
        let group =
            |names: &[&str]| -> u64 { names.iter().filter_map(|n| self.tools.get(*n)).sum() };
        let coding = group(&["Edit", "Write", "MultiEdit", "NotebookEdit"]);
        let ops = group(&["Bash"]);
        let research = group(&["Read", "Grep", "Glob", "WebFetch", "WebSearch"]);
        let total = coding + ops + research;
        if total == 0 {
            return "empty";
        }
        let max = coding.max(ops).max(research);
        // Require a clear plurality, else "mixed".
        if max * 2 < total {
            return "mixed";
        }
        if max == coding {
            "coding"
        } else if max == ops {
            "ops"
        } else {
            "research"
        }
    }

    /// Top tools by count, descending.
    pub fn top_tools(&self, n: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.tools.iter().map(|(k, c)| (k.clone(), *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    pub fn render_report(&self, cfg: &Config) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "session {}  [{}]\n  project: {}\n  size: {}   category: {}\n  turns: {}   tool events: {}   files: {}\n",
            short(&self.session_id),
            self.started_at,
            self.project,
            self.size(&cfg.size_thresholds),
            self.category(),
            self.turns,
            self.tool_events,
            self.files.len(),
        ));
        let tops = self.top_tools(6);
        if !tops.is_empty() {
            let parts: Vec<String> = tops.iter().map(|(t, c)| format!("{t} {c}")).collect();
            s.push_str(&format!("  top tools: {}\n", parts.join(", ")));
        }
        s
    }
}

pub fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

/// The result of scanning the state dir: the rollups that loaded, and every
/// `<sid>.json` that could NOT be read or parsed. `unreadable` non-empty means
/// `sessions` is a PARTIAL list — callers must not present it as complete.
#[derive(Debug, Default)]
pub struct Loaded {
    pub sessions: Vec<Session>,
    /// `(path, reason)` for each session file that failed to read/parse.
    pub unreadable: Vec<(PathBuf, String)>,
}

impl Loaded {
    /// One-line disclosure of the unreadable files, for stderr.
    pub fn unreadable_summary(&self) -> String {
        let parts: Vec<String> = self
            .unreadable
            .iter()
            .map(|(p, why)| format!("{} ({why})", p.display()))
            .collect();
        format!(
            "{} session file(s) could not be read: {}",
            self.unreadable.len(),
            parts.join("; ")
        )
    }
}

/// All session rollups on disk, newest first. A missing state dir is a
/// legitimate absence (no session recorded yet) and yields an empty `Known`;
/// any OTHER `read_dir` error (permission denied, IO) is a cannot-determine
/// — it must not be silently read as "no sessions", so it comes back
/// `Undetermined` instead. A single `<sid>.json` that cannot be read or parsed
/// is NOT dropped silently either: it is listed in [`Loaded::unreadable`].
pub fn load_all(cfg: &Config) -> Determination<Loaded> {
    let mut out = Loaded::default();
    let entries = match std::fs::read_dir(&cfg.state_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(out);
        }
        Err(e) => return Determination::undetermined(format!("{}: {e}", cfg.state_dir.display())),
    };
    for e in entries {
        // A failed directory entry names no file, so it cannot be attributed
        // to one session: the whole scan is a cannot-determine.
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                return Determination::undetermined(format!(
                    "{}: directory entry could not be read: {err}",
                    cfg.state_dir.display()
                ))
            }
        };
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Session>(&t).map_err(|e| e.to_string()));
        match parsed {
            Ok(s) => out.sessions.push(s),
            Err(why) => out.unreadable.push((p, why)),
        }
    }
    out.sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    Determination::Known(out)
}

/// The newest session. If any session file is unreadable, which one is newest
/// cannot be determined (the unreadable one may be it), so this is
/// `Undetermined` rather than a possibly-wrong answer.
pub fn latest(cfg: &Config) -> Determination<Option<Session>> {
    match load_all(cfg) {
        Determination::Known(l) if !l.unreadable.is_empty() => {
            Determination::undetermined(l.unreadable_summary())
        }
        Determination::Known(l) => Determination::Known(l.sessions.into_iter().next()),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// The newest session whose id starts with `prefix`. An unreadable file whose
/// name (`<sid>.json`) also matches the prefix makes the answer undetermined;
/// unreadable files that cannot match do not.
pub fn find(cfg: &Config, prefix: &str) -> Determination<Option<Session>> {
    match load_all(cfg) {
        Determination::Known(mut l) => {
            l.unreadable.retain(|(p, _)| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_none_or(|stem| stem.starts_with(prefix))
            });
            if !l.unreadable.is_empty() {
                return Determination::undetermined(l.unreadable_summary());
            }
            Determination::Known(
                l.sessions
                    .into_iter()
                    .find(|s| s.session_id.starts_with(prefix)),
            )
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn size_classes() {
        let mut s = Session::default();
        let t = &cfg().size_thresholds;
        assert_eq!(s.size(t), "XS");
        s.tool_events = 5;
        assert_eq!(s.size(t), "S");
        s.tool_events = 40;
        assert_eq!(s.size(t), "L");
        s.tool_events = 100;
        assert_eq!(s.size(t), "XL");
    }

    #[test]
    fn ensure_pins_project_on_first_call() {
        let mut s = Session::default();
        s.ensure("sess-1", "harness", "/Users/x/src/harness");
        let started = s.started_at.clone();
        assert_eq!(s.project, "harness");
        assert_eq!(s.cwd, "/Users/x/src/harness");
        // A later tool event from a changed cwd must NOT rewrite project/cwd.
        s.ensure(
            "sess-1",
            "session-insights",
            "/Users/x/src/harness/crates/session-insights",
        );
        assert_eq!(
            s.project, "harness",
            "project must stay pinned to session start"
        );
        assert_eq!(
            s.cwd, "/Users/x/src/harness",
            "cwd must stay pinned to session start"
        );
        assert_eq!(s.session_id, "sess-1");
        assert_eq!(s.started_at, started, "started_at must not change");
    }

    #[test]
    fn category_plurality() {
        let mut s = Session::default();
        s.tools.insert("Edit".into(), 8);
        s.tools.insert("Bash".into(), 1);
        assert_eq!(s.category(), "coding");
        s.tools.clear();
        s.tools.insert("Edit".into(), 3);
        s.tools.insert("Bash".into(), 3);
        s.tools.insert("Read".into(), 3);
        assert_eq!(s.category(), "mixed");
    }

    #[test]
    fn missing_state_dir_yields_known_empty_not_undetermined() {
        let mut c = cfg();
        c.state_dir = std::path::PathBuf::from("/no/such/session-insights/state/dir/at/all");
        assert!(matches!(
            load_all(&c),
            Determination::Known(l) if l.sessions.is_empty() && l.unreadable.is_empty()
        ));
    }

    fn state_with_corrupt(tag: &str) -> Config {
        let dir = std::env::temp_dir().join(format!(
            "session-insights-metrics-corrupt-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = cfg();
        c.state_dir = dir;
        let mut good = Session::default();
        good.ensure("good-1", "p", "/p");
        save(&c, "good-1", &good);
        std::fs::write(c.state_dir.join("bad-2.json"), "{ not json").unwrap();
        c
    }

    #[test]
    fn corrupt_session_file_is_listed_as_unreadable() {
        let c = state_with_corrupt("all");
        let r = load_all(&c);
        let _ = std::fs::remove_dir_all(&c.state_dir);
        match r {
            Determination::Known(l) => {
                assert_eq!(l.sessions.len(), 1);
                assert_eq!(l.unreadable.len(), 1, "{:?}", l.unreadable);
                assert!(l.unreadable[0].0.ends_with("bad-2.json"));
            }
            other => panic!("expected Known, got {other:?}"),
        }
    }

    /// The corrupt file may be the newest session, so `latest` cannot answer.
    #[test]
    fn latest_with_a_corrupt_file_is_undetermined() {
        let c = state_with_corrupt("latest");
        let r = latest(&c);
        let _ = std::fs::remove_dir_all(&c.state_dir);
        assert!(
            matches!(&r, Determination::Undetermined(w) if w.as_str().contains("bad-2.json")),
            "{r:?}"
        );
    }

    /// `find` is undetermined only when the corrupt file's id could match.
    #[test]
    fn find_is_undetermined_only_for_a_matching_corrupt_file() {
        let c = state_with_corrupt("find");
        let matching = find(&c, "bad");
        let other = find(&c, "good");
        let _ = std::fs::remove_dir_all(&c.state_dir);
        assert!(
            matches!(&matching, Determination::Undetermined(w) if w.as_str().contains("bad-2.json")),
            "{matching:?}"
        );
        assert!(
            matches!(&other, Determination::Known(Some(s)) if s.session_id == "good-1"),
            "{other:?}"
        );
    }

    /// CA-session-insights-metrics-01: a state dir that EXISTS but is
    /// unreadable (permission denied) must resolve to `Undetermined`, not
    /// the same `Known(vec![])` as a legitimately-absent dir — collapsing
    /// both into "no sessions" makes an incomplete read look complete.
    #[test]
    #[cfg(unix)]
    fn unreadable_existing_state_dir_is_undetermined_not_known_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "session-insights-metrics-unreadable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&dir, perms.clone()).unwrap();

        let mut c = cfg();
        c.state_dir = dir.clone();
        let result = load_all(&c);

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&dir, perms);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            matches!(result, Determination::Undetermined(_)),
            "an unreadable existing state dir must be Undetermined, got {result:?}"
        );
    }
}
