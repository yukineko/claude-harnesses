//! The world-facing half of `flow stop-gate`: driver-registry scan,
//! subprocess probes, and the pure parsers for their outputs.
//!
//! Every subprocess is judged by its exit status first (CLAUDE.md §3); a spawn
//! failure (binary absent), a signal, a timeout or unreadable pipes are
//! `Unknown`, never an empty answer.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harness_core::boundary;
use harness_core::verdict::Determination;
use serde_json::Value;

use crate::decide::{Circuit, Holder, Liveness, Obs, Probes, Task};

/// Per-subprocess bound. Five probes at most per project (driver status, next,
/// claims, is-claimed, circuit); the hook timeout is 60s, so 5 x 10s stays
/// under it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

fn det<T>(d: Determination<T>) -> Obs<T> {
    match d {
        Determination::Known(v) => Obs::Known(v),
        Determination::Undetermined(why) => Obs::Unknown(why.as_str().to_string()),
    }
}

/// Filename of a session's driver record — the same slug as
/// `crates/backlog/src/driver.rs` `session_slug`.
pub fn session_slug(session_id: &str) -> String {
    format!(
        "{:016x}",
        harness_core::hash::fnv1a64(session_id.as_bytes())
    )
}

/// Result of scanning the driver registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registry {
    /// The projects this session is registered for (empty = not a driver).
    Projects(Vec<String>),
    Unknown(String),
}

/// Scan `<drivers_root>/*/<slug>.driver` without spawning anything.
///
/// `drivers_root` absent → not a driver. A bucket that is not a directory is
/// skipped (it cannot hold a record). Any other IO failure, or a matching
/// record that is unreadable / unparseable / has no `project` string, is
/// `Unknown`.
pub fn scan_registry(drivers_root: &Path, session_id: &str) -> Registry {
    let buckets = match boundary::read_dir_entries_checked(drivers_root) {
        Determination::Known(None) => return Registry::Projects(Vec::new()),
        Determination::Known(Some(b)) => b,
        Determination::Undetermined(why) => return Registry::Unknown(why.as_str().to_string()),
    };
    let file = format!("{}.driver", session_slug(session_id));
    let mut projects = Vec::new();
    for bucket in buckets {
        match std::fs::symlink_metadata(&bucket) {
            Ok(m) if !m.is_dir() && !m.file_type().is_symlink() => continue,
            Ok(_) => {}
            Err(e) => {
                return Registry::Unknown(format!("cannot stat {}: {e}", bucket.display()));
            }
        }
        let path = bucket.join(&file);
        match boundary::read_to_string(&path) {
            Determination::Known(None) => {}
            Determination::Known(Some(text)) => match parse_record(&text) {
                Some(p) => projects.push(p),
                None => {
                    return Registry::Unknown(format!(
                        "driver record {} is not a JSON object with a \"project\" string",
                        path.display()
                    ))
                }
            },
            Determination::Undetermined(why) => {
                return Registry::Unknown(why.as_str().to_string());
            }
        }
    }
    Registry::Projects(projects)
}

/// Whether ANY `*.driver` record exists under the registry (used when the
/// session id is unknown: no records at all means nobody is a driver).
pub fn any_driver_record(drivers_root: &Path) -> Obs<bool> {
    let buckets = match boundary::read_dir_entries_checked(drivers_root) {
        Determination::Known(None) => return Obs::Known(false),
        Determination::Known(Some(b)) => b,
        Determination::Undetermined(why) => return Obs::Unknown(why.as_str().to_string()),
    };
    for bucket in buckets {
        match std::fs::symlink_metadata(&bucket) {
            Ok(m) if !m.is_dir() && !m.file_type().is_symlink() => continue,
            Ok(_) => {}
            Err(e) => return Obs::Unknown(format!("cannot stat {}: {e}", bucket.display())),
        }
        match boundary::read_dir_entries(&bucket) {
            Determination::Known(entries) => {
                if entries
                    .iter()
                    .any(|e| e.extension().is_some_and(|x| x == "driver"))
                {
                    return Obs::Known(true);
                }
            }
            Determination::Undetermined(why) => return Obs::Unknown(why.as_str().to_string()),
        }
    }
    Obs::Known(false)
}

fn parse_record(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    let p = v.get("project")?.as_str()?;
    (!p.is_empty()).then(|| p.to_string())
}

/// `backlog driver status --project P` stdout (exit already checked == 0).
pub fn parse_liveness(stdout: &str, session: &str) -> Obs<Liveness> {
    let v: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => return Obs::Unknown(format!("output is not JSON ({e})")),
    };
    match v.get("undetermined").and_then(Value::as_bool) {
        Some(false) => {}
        Some(true) => {
            let why = v
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Obs::Unknown(format!("it reported \"undetermined\": true ({why})"));
        }
        None => return Obs::Unknown("output has no boolean \"undetermined\" field".into()),
    }
    let listed = |key: &str| -> Option<bool> {
        let arr = v.get(key)?.as_array()?;
        let mut found = false;
        for d in arr {
            match d.get("session_id").and_then(Value::as_str) {
                Some(s) if s == session => found = true,
                Some(_) => {}
                None => return None,
            }
        }
        Some(found)
    };
    match (listed("drivers"), listed("stale")) {
        (Some(true), _) => Obs::Known(Liveness::Live),
        (Some(false), Some(true)) => Obs::Known(Liveness::Stale),
        (Some(false), Some(false)) => Obs::Known(Liveness::Absent),
        _ => Obs::Unknown(
            "output lacks well-formed \"drivers\"/\"stale\" arrays of records with session_id"
                .into(),
        ),
    }
}

/// `backlog next --project P` stdout (exit already checked == 0).
pub fn parse_next(stdout: &str) -> Obs<Option<Task>> {
    let t = stdout.trim();
    if t == "no pending tasks" {
        return Obs::Known(None);
    }
    let v: Value = match serde_json::from_str(t) {
        Ok(v) => v,
        Err(_) => {
            let head: String = t.chars().take(200).collect();
            return Obs::Unknown(format!(
                "output is neither `no pending tasks` nor a JSON task: {head:?}"
            ));
        }
    };
    let id = v.get("id").and_then(Value::as_str);
    match id {
        Some(id) if !id.is_empty() => Obs::Known(Some(Task {
            id: id.to_string(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // Absent / non-string / empty → None: the claim cannot be looked
            // up, so the task stays remaining work (decide::filter_pending).
            hashkey: v
                .get("hashkey")
                .and_then(Value::as_str)
                .filter(|h| !h.trim().is_empty())
                .map(str::to_string),
        })),
        _ => Obs::Unknown("JSON output is not a task object with an \"id\"".into()),
    }
}

/// `condukt state claims` stdout (exit already checked == 0): keys of the
/// entries whose `run_id` is `flow-<session>`.
pub fn parse_claims(stdout: &str, session: &str) -> Obs<Vec<String>> {
    let v: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => return Obs::Unknown(format!("output is not JSON ({e})")),
    };
    let Some(obj) = v.as_object() else {
        return Obs::Unknown("output is not a JSON object".into());
    };
    let run = format!("flow-{session}");
    let mut held = Vec::new();
    for (k, entry) in obj {
        match entry.get("run_id").and_then(Value::as_str) {
            Some(r) if r == run => held.push(k.clone()),
            Some(_) => {}
            None => return Obs::Unknown(format!("claim {k:?} has no \"run_id\" string")),
        }
    }
    Obs::Known(held)
}

/// `condukt state is-claimed --hashkey H` exit code + stdout.
///
/// The exit code is condukt's contract (0 = claimed, 1 = not claimed,
/// 3 = registry unreadable, 2 = invalid input); the JSON must agree with it.
/// Exit 0 needs `"claimed": true` and a non-empty `holder_run` string (the
/// holder decides whether the task is another session's); exit 1 needs
/// `"claimed": false`. Anything else is `Unknown`.
pub fn parse_holder(code: i32, stdout: &str) -> Obs<Holder> {
    let v: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    let claimed = v
        .as_ref()
        .and_then(|v| v.get("claimed"))
        .and_then(Value::as_bool);
    let holder = v
        .as_ref()
        .and_then(|v| v.get("holder_run"))
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty());
    match (code, claimed, holder) {
        (0, Some(true), Some(run)) => Obs::Known(Holder::Run(run.to_string())),
        (1, Some(false), _) => Obs::Known(Holder::Unclaimed),
        (c, claimed, holder) => Obs::Unknown(format!(
            "exit {c} with claimed {claimed:?} and holder_run {holder:?} (expected exit 0 + \
             \"claimed\": true + a holder_run string, or exit 1 + \"claimed\": false)"
        )),
    }
}

/// `condukt circuit check` exit code + stdout.
pub fn parse_circuit(code: i32, stdout: &str) -> Obs<Circuit> {
    let v: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    let verdict = v
        .as_ref()
        .and_then(|v| v.get("verdict"))
        .and_then(Value::as_str);
    match (code, verdict) {
        (0, Some("continue")) => Obs::Known(Circuit::Continue),
        (1, Some("trip")) => {
            let reason = v
                .as_ref()
                .and_then(|v| v.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("unspecified")
                .to_string();
            Obs::Known(Circuit::Trip(reason))
        }
        (c, verdict) => Obs::Unknown(format!(
            "exit {c} with verdict {verdict:?} (expected exit 0 + \"continue\" or exit 1 + \"trip\")"
        )),
    }
}

/// Real probes: spawn `backlog` / `condukt`, each located by
/// [`harness_core::plugin_bin::resolve`] (plugin cache first, `$PATH` second).
///
/// A bare-name spawn is not used: hook processes do not carry the plugin `bin/`
/// dirs on `$PATH`, so `Command::new("backlog")` failed with
/// "No such file or directory" while the same binary ran from the Bash tool.
pub struct Real {
    pub session: String,
}

/// Turn a sibling plugin's [`harness_core::plugin_bin::resolve`] answer into the
/// path to spawn. Only `Known(Some)` yields a path. `Known(None)` (not installed
/// here) and `Undetermined` both become `Unknown`: a probe whose program could
/// not be located never ran, so it has not observed "no work" (CLAUDE.md §3).
fn locate(name: &str, found: Determination<Option<PathBuf>>) -> Obs<PathBuf> {
    match found {
        Determination::Known(Some(path)) => Obs::Known(path),
        Determination::Known(None) => Obs::Unknown(format!(
            "cannot locate {name}: not in the plugin cache and not on $PATH"
        )),
        Determination::Undetermined(why) => {
            Obs::Unknown(format!("cannot locate {name}: {}", why.as_str()))
        }
    }
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Obs<boundary::CommandOutput> {
    let exe = match locate(program, harness_core::plugin_bin::resolve(program)) {
        Obs::Known(exe) => exe,
        Obs::Unknown(why) => return Obs::Unknown(why),
    };
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(cwd);
    det(boundary::run_with_timeout(&mut cmd, PROBE_TIMEOUT))
}

fn on_success(o: Obs<boundary::CommandOutput>) -> Obs<String> {
    match o {
        Obs::Known(out) => det(out.stdout_on_success()),
        Obs::Unknown(w) => Obs::Unknown(w),
    }
}

fn project_dir(project: &str) -> PathBuf {
    PathBuf::from(project)
}

impl Probes for Real {
    fn liveness(&mut self, project: &str) -> Obs<Liveness> {
        let o = run(
            "backlog",
            &["driver", "status", "--project", project],
            &project_dir(project),
        );
        match on_success(o) {
            Obs::Known(s) => parse_liveness(&s, &self.session),
            Obs::Unknown(w) => Obs::Unknown(w),
        }
    }

    fn next(&mut self, project: &str) -> Obs<Option<Task>> {
        let o = run(
            "backlog",
            &["next", "--project", project],
            &project_dir(project),
        );
        match on_success(o) {
            Obs::Known(s) => parse_next(&s),
            Obs::Unknown(w) => Obs::Unknown(w),
        }
    }

    fn claims(&mut self, project: &str) -> Obs<Vec<String>> {
        let o = run("condukt", &["state", "claims"], &project_dir(project));
        match on_success(o) {
            Obs::Known(s) => parse_claims(&s, &self.session),
            Obs::Unknown(w) => Obs::Unknown(w),
        }
    }

    fn holder(&mut self, project: &str, hashkey: &str) -> Obs<Holder> {
        let o = run(
            "condukt",
            &["state", "is-claimed", "--hashkey", hashkey],
            &project_dir(project),
        );
        match o {
            Obs::Known(out) => {
                let code = out.code();
                match det(out.stdout_allowing(&[0, 1])) {
                    Obs::Known(s) => parse_holder(code, &s),
                    Obs::Unknown(w) => Obs::Unknown(w),
                }
            }
            Obs::Unknown(w) => Obs::Unknown(w),
        }
    }

    fn circuit(&mut self, project: &str) -> Obs<Circuit> {
        let run_id = format!("flow-{}", self.session);
        let o = run(
            "condukt",
            &[
                "circuit",
                "check",
                "--run",
                &run_id,
                "--session",
                &self.session,
                "--no-compaction-axis",
            ],
            &project_dir(project),
        );
        match o {
            Obs::Known(out) => {
                let code = out.code();
                match det(out.stdout_allowing(&[0, 1])) {
                    Obs::Known(s) => parse_circuit(code, &s),
                    Obs::Unknown(w) => Obs::Unknown(w),
                }
            }
            Obs::Unknown(w) => Obs::Unknown(w),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlocatable_sibling_is_unknown_never_a_spawn() {
        // Not installed and cannot-look are both unknown: neither ran, so
        // neither may read as an observed "no work".
        assert!(matches!(
            locate("backlog", Determination::Known(None)),
            Obs::Unknown(_)
        ));
        assert!(matches!(
            locate("backlog", Determination::undetermined("cache unreadable")),
            Obs::Unknown(_)
        ));
        match locate(
            "backlog",
            Determination::Known(Some(PathBuf::from("/x/backlog"))),
        ) {
            Obs::Known(p) => assert_eq!(p, PathBuf::from("/x/backlog")),
            Obs::Unknown(w) => panic!("a located binary must be spawned, got unknown: {w}"),
        }
    }

    #[test]
    fn slug_matches_backlog_format() {
        assert_eq!(session_slug("x").len(), 16);
    }

    #[test]
    fn liveness_parsing() {
        let live = r#"{"undetermined":false,"drivers":[{"session_id":"s"}],"stale":[]}"#;
        assert_eq!(parse_liveness(live, "s"), Obs::Known(Liveness::Live));
        let stale = r#"{"undetermined":false,"drivers":[],"stale":[{"session_id":"s"}]}"#;
        assert_eq!(parse_liveness(stale, "s"), Obs::Known(Liveness::Stale));
        assert_eq!(parse_liveness(stale, "o"), Obs::Known(Liveness::Absent));
        assert!(matches!(
            parse_liveness(r#"{"undetermined":true,"drivers":[],"stale":[]}"#, "s"),
            Obs::Unknown(_)
        ));
        assert!(matches!(
            parse_liveness(r#"{"drivers":[],"stale":[]}"#, "s"),
            Obs::Unknown(_)
        ));
        assert!(matches!(
            parse_liveness(r#"{"undetermined":false,"drivers":[]}"#, "s"),
            Obs::Unknown(_)
        ));
        assert!(matches!(parse_liveness("nope", "s"), Obs::Unknown(_)));
    }

    #[test]
    fn next_parsing() {
        assert_eq!(parse_next("no pending tasks\n"), Obs::Known(None));
        assert_eq!(
            parse_next(r#"{"id":"a","title":"b"}"#),
            Obs::Known(Some(Task {
                id: "a".into(),
                title: "b".into(),
                hashkey: None,
            }))
        );
        assert!(matches!(parse_next(""), Obs::Unknown(_)));
        assert!(matches!(parse_next("{oops"), Obs::Unknown(_)));
        assert!(matches!(parse_next(r#"{"title":"b"}"#), Obs::Unknown(_)));
    }

    #[test]
    fn claims_parsing() {
        let c = r#"{"k1":{"run_id":"flow-s"},"k2":{"run_id":"flow-o"},"k3":{"run_id":"run-1"}}"#;
        assert_eq!(parse_claims(c, "s"), Obs::Known(vec!["k1".to_string()]));
        assert_eq!(parse_claims("{}", "s"), Obs::Known(vec![]));
        assert!(matches!(parse_claims("[]", "s"), Obs::Unknown(_)));
        assert!(matches!(parse_claims("<<x>>", "s"), Obs::Unknown(_)));
        assert!(matches!(parse_claims(r#"{"k":{}}"#, "s"), Obs::Unknown(_)));
    }

    #[test]
    fn circuit_parsing_requires_code_and_verdict_to_agree() {
        assert_eq!(
            parse_circuit(0, r#"{"verdict":"continue"}"#),
            Obs::Known(Circuit::Continue)
        );
        assert_eq!(
            parse_circuit(1, r#"{"verdict":"trip","reason":"failure_streak"}"#),
            Obs::Known(Circuit::Trip("failure_streak".into()))
        );
        assert!(matches!(parse_circuit(1, ""), Obs::Unknown(_)));
        assert!(matches!(
            parse_circuit(0, r#"{"verdict":"trip"}"#),
            Obs::Unknown(_)
        ));
        assert!(matches!(
            parse_circuit(2, r#"{"verdict":"continue"}"#),
            Obs::Unknown(_)
        ));
        assert!(matches!(parse_circuit(0, "%%%"), Obs::Unknown(_)));
    }

    #[test]
    fn registry_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drivers");
        assert_eq!(scan_registry(&root, "s"), Registry::Projects(vec![]));
        assert_eq!(any_driver_record(&root), Obs::Known(false));
        let bucket = root.join("b");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(root.join("stray-file"), "").unwrap();
        assert_eq!(scan_registry(&root, "s"), Registry::Projects(vec![]));
        std::fs::write(
            bucket.join(format!("{}.driver", session_slug("s"))),
            r#"{"project":"/p"}"#,
        )
        .unwrap();
        assert_eq!(
            scan_registry(&root, "s"),
            Registry::Projects(vec!["/p".into()])
        );
        assert_eq!(scan_registry(&root, "o"), Registry::Projects(vec![]));
        assert_eq!(any_driver_record(&root), Obs::Known(true));
        std::fs::write(bucket.join(format!("{}.driver", session_slug("g"))), "{").unwrap();
        assert!(matches!(scan_registry(&root, "g"), Registry::Unknown(_)));
        std::fs::create_dir_all(bucket.join(format!("{}.driver", session_slug("d")))).unwrap();
        assert!(matches!(scan_registry(&root, "d"), Registry::Unknown(_)));
    }
}
