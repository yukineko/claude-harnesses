//! Plugin activation-scope stratification: scan every crate in the monorepo and
//! classify it by *how loud* its activation surface is, so the always-on hook /
//! injection budget (ADR 0001) stays legible at a glance.
//!
//! Three scopes (see [`Scope`]):
//! - **AlwaysOn** — registers a hook on a high-frequency lifecycle event that
//!   fires basically every turn/session.
//! - **EventScoped** — registers hooks ONLY on lower-frequency/conditional events.
//! - **Manual** — registers NO hooks; reachable only via a skill (slash command)
//!   or direct CLI (e.g. harness-status itself).
//!
//! What counts as a plugin: a crate dir is treated as a plugin iff it has any of
//! `.claude-plugin/`, `hooks/`, `skills/`, or `agents/`. Plain library crates
//! (e.g. `harness-core`) have none of these and are EXCLUDED — they present no
//! plugin surface and don't belong in an activation-scope taxonomy.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The set of lifecycle hook events that fire on essentially every turn or
/// session, making any plugin that registers one of them "always-on".
const ALWAYS_ON_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SubagentStop",
];

/// Activation scope of a plugin, ordered loudest → quietest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Registers a hook on a high-frequency lifecycle event (fires ~every turn).
    AlwaysOn,
    /// Registers hooks only on lower-frequency/conditional events.
    EventScoped,
    /// Registers no hooks; activated via skill (slash command) or direct CLI.
    Manual,
}

/// Classify a plugin purely from its surface:
/// - any always-on event present → [`Scope::AlwaysOn`],
/// - else any hook event present → [`Scope::EventScoped`],
/// - else → [`Scope::Manual`] (regardless of `has_skills`/`has_agents`, which
///   only inform the human-readable *trigger* description).
pub fn classify(hook_events: &[String], _has_skills: bool, _has_agents: bool) -> Scope {
    if hook_events
        .iter()
        .any(|e| ALWAYS_ON_EVENTS.contains(&e.as_str()))
    {
        Scope::AlwaysOn
    } else if !hook_events.is_empty() {
        Scope::EventScoped
    } else {
        Scope::Manual
    }
}

/// A single classified plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub scope: Scope,
    pub hook_events: Vec<String>,
    pub has_skills: bool,
    pub has_agents: bool,
    pub trigger: String,
}

/// Grouped classification result for the whole repo, with counts.
#[derive(Debug, Serialize)]
pub struct PluginReport {
    pub always_on: Vec<PluginInfo>,
    pub event_scoped: Vec<PluginInfo>,
    pub manual: Vec<PluginInfo>,
    pub counts: Counts,
}

#[derive(Debug, Serialize)]
pub struct Counts {
    pub always_on: usize,
    pub event_scoped: usize,
    pub manual: usize,
    pub total: usize,
}

/// Walk up from `start` to find the repo root: the first ancestor that contains
/// both a `crates/` dir and `.claude-plugin/marketplace.json`. Falls back to
/// `start` itself if no such ancestor is found.
pub fn find_repo_root(start: &Path) -> PathBuf {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join("crates").is_dir() && dir.join(".claude-plugin/marketplace.json").is_file() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    start.to_path_buf()
}

/// True if `dir` exists and contains at least one entry.
fn dir_nonempty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// Parse a Claude Code `hooks.json` and collect the event-name keys (the top-level
/// `"hooks"` object's keys, e.g. `"Stop"`, `"UserPromptSubmit"`, `"PreCompact"`).
/// Returns a sorted, de-duplicated list. Malformed/missing files yield an empty
/// list (never panics).
fn read_hook_events(hooks_json: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(hooks_json) else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let mut events: BTreeSet<String> = BTreeSet::new();
    if let Some(map) = val.get("hooks").and_then(|h| h.as_object()) {
        for k in map.keys() {
            events.insert(k.clone());
        }
    }
    events.into_iter().collect()
}

/// Read the `name` field from `.claude-plugin/plugin.json`, if present.
fn plugin_json_name(plugin_json: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(plugin_json).ok()?;
    let val = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    val.get("name")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
}

/// Build the short human trigger description for a plugin.
fn trigger_for(
    scope: Scope,
    name: &str,
    hook_events: &[String],
    has_skills: bool,
    has_agents: bool,
) -> String {
    match scope {
        Scope::AlwaysOn | Scope::EventScoped => hook_events.join(", "),
        Scope::Manual => {
            if has_skills && has_agents {
                "skill+agent".to_string()
            } else if has_skills {
                format!("skill (/{name})")
            } else if has_agents {
                "agent".to_string()
            } else {
                "CLI only".to_string()
            }
        }
    }
}

/// Scan `repo_root/crates/*/` and classify each plugin dir. Non-plugin dirs
/// (plain libraries with no `.claude-plugin/`, `hooks/`, `skills/`, or `agents/`)
/// are excluded. Returns plugins in filesystem-iteration order.
///
/// A `crates/` dir that cannot be read (permission denied, IO error) is a
/// cannot-determine and is surfaced as `Err`, not silently read as "0
/// plugins" — the CLI caller must not report an empty scan as if it were a
/// complete one.
pub fn scan(repo_root: &Path) -> Result<Vec<PluginInfo>, std::io::Error> {
    let crates_dir = repo_root.join("crates");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&crates_dir)?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let has_plugin_meta = dir.join(".claude-plugin").is_dir();
        let hooks_json = dir.join("hooks/hooks.json");
        let has_hooks_dir = dir.join("hooks").is_dir();
        let has_skills = dir_nonempty(&dir.join("skills"));
        let has_agents = dir_nonempty(&dir.join("agents"));

        // A crate is a plugin iff it exposes any activation surface.
        if !has_plugin_meta && !has_hooks_dir && !has_skills && !has_agents {
            continue;
        }

        let hook_events = read_hook_events(&hooks_json);
        let dir_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = plugin_json_name(&dir.join(".claude-plugin/plugin.json"))
            .unwrap_or_else(|| dir_name.clone());

        let scope = classify(&hook_events, has_skills, has_agents);
        let trigger = trigger_for(scope, &name, &hook_events, has_skills, has_agents);

        out.push(PluginInfo {
            name,
            scope,
            hook_events,
            has_skills,
            has_agents,
            trigger,
        });
    }
    Ok(out)
}

/// Scan + group into a [`PluginReport`], with names sorted alphabetically within
/// each group.
pub fn report(repo_root: &Path) -> Result<PluginReport, std::io::Error> {
    let plugins = scan(repo_root)?;
    let mut always_on = Vec::new();
    let mut event_scoped = Vec::new();
    let mut manual = Vec::new();
    for p in plugins {
        match p.scope {
            Scope::AlwaysOn => always_on.push(p),
            Scope::EventScoped => event_scoped.push(p),
            Scope::Manual => manual.push(p),
        }
    }
    always_on.sort_by(|a, b| a.name.cmp(&b.name));
    event_scoped.sort_by(|a, b| a.name.cmp(&b.name));
    manual.sort_by(|a, b| a.name.cmp(&b.name));
    let counts = Counts {
        always_on: always_on.len(),
        event_scoped: event_scoped.len(),
        manual: manual.len(),
        total: always_on.len() + event_scoped.len() + manual.len(),
    };
    Ok(PluginReport {
        always_on,
        event_scoped,
        manual,
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn classify_always_on_on_high_frequency_event() {
        assert_eq!(
            classify(&["Stop".to_string()], false, false),
            Scope::AlwaysOn
        );
        assert_eq!(
            classify(
                &["UserPromptSubmit".to_string(), "PreCompact".to_string()],
                false,
                false
            ),
            Scope::AlwaysOn,
            "any always-on event wins even mixed with a low-frequency one"
        );
    }

    #[test]
    fn classify_event_scoped_on_only_low_frequency_events() {
        assert_eq!(
            classify(&["PreCompact".to_string()], false, false),
            Scope::EventScoped
        );
        assert_eq!(
            classify(
                &["SessionEnd".to_string(), "Notification".to_string()],
                true,
                true
            ),
            Scope::EventScoped,
            "skills/agents do not upgrade an event-scoped plugin"
        );
    }

    #[test]
    fn classify_manual_when_no_hooks() {
        assert_eq!(classify(&[], true, false), Scope::Manual);
        assert_eq!(classify(&[], false, true), Scope::Manual);
        assert_eq!(
            classify(&[], false, false),
            Scope::Manual,
            "no hooks + no skills + no agents (CLI-only) is still Manual"
        );
    }

    // Build a fake crate layout under a temp dir and assert scan() behavior.
    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn scan_picks_up_events_skills_agents_and_excludes_bare_lib() {
        let tmp = std::env::temp_dir().join(format!("hs-plugins-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let crates = tmp.join("crates");

        // AlwaysOn plugin with a Stop hook and a plugin.json name override.
        write(
            &crates.join("alpha/hooks/hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[]}],"PreCompact":[{"hooks":[]}]}}"#,
        );
        write(
            &crates.join("alpha/.claude-plugin/plugin.json"),
            r#"{"name":"alpha-renamed"}"#,
        );

        // EventScoped plugin: only a low-frequency event.
        write(
            &crates.join("beta/hooks/hooks.json"),
            r#"{"hooks":{"SessionEnd":[{"hooks":[]}]}}"#,
        );

        // Manual plugin: a skill, no hooks.
        fs::create_dir_all(crates.join("gamma/skills/gamma")).unwrap();
        fs::create_dir_all(crates.join("gamma/.claude-plugin")).unwrap();
        write(
            &crates.join("gamma/.claude-plugin/plugin.json"),
            r#"{"name":"gamma"}"#,
        );

        // Manual CLI-only plugin: has .claude-plugin but no hooks/skills/agents.
        write(
            &crates.join("delta/.claude-plugin/plugin.json"),
            r#"{"name":"delta"}"#,
        );

        // Bare library: no plugin surface at all — must be EXCLUDED.
        write(&crates.join("libcore/src/lib.rs"), "// nothing\n");

        // Plugin with agents dir only.
        fs::create_dir_all(crates.join("epsilon/agents")).unwrap();
        write(&crates.join("epsilon/agents/w.md"), "agent\n");

        let plugins = scan(&tmp).expect("tmp crates dir is readable");
        let by_name = |n: &str| plugins.iter().find(|p| p.name == n).cloned();

        assert!(by_name("libcore").is_none(), "bare lib excluded");
        assert_eq!(plugins.len(), 5, "5 plugin dirs, lib excluded");

        let alpha = by_name("alpha-renamed").expect("name from plugin.json");
        assert_eq!(alpha.scope, Scope::AlwaysOn);
        assert!(alpha.hook_events.contains(&"Stop".to_string()));
        assert!(alpha.hook_events.contains(&"PreCompact".to_string()));

        let beta = by_name("beta").expect("dir-name fallback");
        assert_eq!(beta.scope, Scope::EventScoped);
        assert_eq!(beta.trigger, "SessionEnd");

        let gamma = by_name("gamma").unwrap();
        assert_eq!(gamma.scope, Scope::Manual);
        assert!(gamma.has_skills);
        assert_eq!(gamma.trigger, "skill (/gamma)");

        let delta = by_name("delta").unwrap();
        assert_eq!(delta.scope, Scope::Manual);
        assert_eq!(delta.trigger, "CLI only");

        let epsilon = by_name("epsilon").unwrap();
        assert_eq!(epsilon.scope, Scope::Manual);
        assert!(epsilon.has_agents);
        assert_eq!(epsilon.trigger, "agent");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_repo_root_walks_up_to_marketplace() {
        let tmp = std::env::temp_dir().join(format!("hs-root-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("crates/foo/src")).unwrap();
        write(&tmp.join(".claude-plugin/marketplace.json"), "{}");
        let start = tmp.join("crates/foo/src");
        assert_eq!(find_repo_root(&start), tmp);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn report_groups_and_counts_are_consistent() {
        // Use the real repo root discovered from CWD when running tests, but keep
        // the assertion structural so it survives plugin churn.
        let root = find_repo_root(&std::env::current_dir().unwrap());
        let r = report(&root).expect("repo crates dir is readable");
        assert_eq!(
            r.counts.total,
            r.counts.always_on + r.counts.event_scoped + r.counts.manual
        );
        assert_eq!(r.counts.always_on, r.always_on.len());
        assert_eq!(r.counts.event_scoped, r.event_scoped.len());
        assert_eq!(r.counts.manual, r.manual.len());
        // Names sorted within each group.
        let sorted = |v: &[PluginInfo]| v.windows(2).all(|w| w[0].name <= w[1].name);
        assert!(sorted(&r.always_on));
        assert!(sorted(&r.event_scoped));
        assert!(sorted(&r.manual));
    }

    /// CA-harness-status-plugins-01: an unreadable (but existing) `crates/`
    /// dir must surface as `Err`, not the same empty `Vec`/`PluginReport` a
    /// legitimately-plugin-less repo produces — the old
    /// `let Ok(..) else { return out }` collapsed both into "0 plugins",
    /// which reads as a complete scan even when it never ran.
    #[test]
    #[cfg(unix)]
    fn unreadable_crates_dir_is_err_not_empty_scan() {
        use std::os::unix::fs::PermissionsExt;
        let tmp =
            std::env::temp_dir().join(format!("hs-plugins-unreadable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let crates_dir = tmp.join("crates");
        fs::create_dir_all(&crates_dir).unwrap();
        let mut perms = fs::metadata(&crates_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&crates_dir, perms.clone()).unwrap();

        let scan_result = scan(&tmp);
        let report_result = report(&tmp);

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&crates_dir, perms);
        let _ = fs::remove_dir_all(&tmp);

        assert!(
            scan_result.is_err(),
            "an unreadable crates/ dir must be Err, got {scan_result:?}"
        );
        assert!(
            report_result.is_err(),
            "report() must propagate scan()'s Err, got {report_result:?}"
        );
    }
}
