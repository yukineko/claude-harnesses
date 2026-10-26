// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for the harness-status README prose. One
//! `claim_<id>` test per claim; each drives the real binary against a sandboxed
//! HOME / cwd (the environment is cleared, so no inherited `HARNESS_*` leaks in).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

const DAY: &str = "2026-09-24";

struct Sb {
    _t: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    tmp: PathBuf,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Sb {
    fn new() -> Sb {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let work = t.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let tmp = t.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        Sb {
            _t: t,
            home,
            work,
            tmp,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_harness-status"));
        c.args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("PATH", "/usr/bin:/bin")
            .env("HARNESS_DATE", DAY)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Out {
        collect(self.cmd(args))
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let mut c = self.cmd(args);
        for (k, v) in env {
            c.env(k, v);
        }
        collect(c)
    }

    fn write(&self, rel_to_home: &str, body: &str) -> PathBuf {
        let p = self.home.join(rel_to_home);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn ledger(&self, v: serde_json::Value) {
        self.write(".budgetguard/state/ledger.json", &v.to_string());
    }

    fn gauge_record(&self, sid: &str, project: &str, turns: u64, last_ts: &str, input: u64) {
        let rec = serde_json::json!({
            "session_id": sid,
            "project": project,
            "cwd": "/cwd",
            "models": { "claude-opus-4-8": { "input": input, "output": 0u64 } },
            "turns": turns,
            "last_ts": last_ts,
            "updated_at": last_ts,
        });
        self.write(
            &format!(".gauge/store/sessions/{sid}.json"),
            &rec.to_string(),
        );
    }

    fn latency(&self, lines: &[(&str, &str, u64)]) {
        let body: String = lines
            .iter()
            .map(|(hook, session, ms)| {
                format!(
                    "{}\n",
                    serde_json::json!({"ts":"2026-09-24T00:00:00+00:00","hook":hook,"session":session,"elapsed_ms":ms})
                )
            })
            .collect();
        self.write(".harness/state/hook-latency.jsonl", &body);
    }

    fn inject(&self, lines: &[(&str, &str, usize)]) {
        let body: String = lines
            .iter()
            .map(|(turn, plugin, chars)| {
                format!(
                    "{}\n",
                    serde_json::json!({"ts":"2026-09-24T00:00:00+00:00","turn_key":turn,"plugin":plugin,"session":"s","chars":chars})
                )
            })
            .collect();
        self.write(".harness/state/inject-metrics.jsonl", &body);
    }

    fn plant_cache(&self, name: &str) -> PathBuf {
        let bin = self
            .home
            .join(".claude/plugins/cache/yukineko")
            .join(name)
            .join("0.1.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(name), b"#!/bin/sh\n").unwrap();
        bin.join(name)
    }
}

fn collect(mut c: Command) -> Out {
    let o = c.output().expect("binary runs");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

type Snapshot = BTreeMap<PathBuf, (Option<Vec<u8>>, Option<SystemTime>)>;

fn snapshot(root: &Path, into: &mut Snapshot) {
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        let meta = std::fs::symlink_metadata(&p).unwrap();
        let mtime = meta.modified().ok();
        if meta.is_dir() {
            into.insert(p.clone(), (None, mtime));
            snapshot(&p, into);
        } else {
            into.insert(
                p.clone(),
                (Some(std::fs::read(&p).unwrap_or_default()), mtime),
            );
        }
    }
}

fn snap_all(sb: &Sb) -> Snapshot {
    let mut s = Snapshot::new();
    snapshot(&sb.home, &mut s);
    snapshot(&sb.work, &mut s);
    snapshot(&sb.tmp, &mut s);
    s
}

fn manifest(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

// ---------------------------------------------------------------------------

#[test]
fn claim_read_only() {
    let sb = Sb::new();
    sb.ledger(serde_json::json!({"days":{DAY:{"sessions":{"a":1.5}}}}));
    sb.gauge_record("sess-read-only-1", "p", 3, "2026-09-24T01:00:00Z", 1000);
    sb.latency(&[("donegate", "sessAAAA1111", 40000)]);
    sb.inject(&[("turnAAAA11112222", "playbook", 25000)]);
    sb.write(
        ".claude/settings.json",
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/no/such/bin/ghost session-start"}]}]}}"#,
    );
    sb.plant_cache("vro");
    let repo = sb.work.join("repo");
    make_fixture_repo(&repo);

    let before = snap_all(&sb);
    let subs: &[&[&str]] = &[
        &[],
        &["budget"],
        &["sessions"],
        &["hooks"],
        &["inject"],
        &["hooks-health"],
        &["path-shadow"],
        &["session-start"],
        &["plugins"],
    ];
    for sub in subs {
        for json in [false, true] {
            for dated in [true, false] {
                for in_repo in [false, true] {
                    if in_repo && sub != &["plugins"] {
                        continue;
                    }
                    let mut args: Vec<&str> = sub.to_vec();
                    if json {
                        args.push("--json");
                    }
                    let mut c = sb.cmd(&args);
                    if !dated {
                        c.env_remove("HARNESS_DATE");
                    }
                    if in_repo {
                        c.current_dir(&repo);
                    }
                    let o = collect(c);
                    if in_repo {
                        // The plugins success path really ran.
                        assert_eq!(o.code, 0, "{args:?}: {}", o.stderr);
                        assert!(o.stdout.contains("alwayson"), "{}", o.stdout);
                    }
                    let after = snap_all(&sb);
                    assert_eq!(
                        before,
                        after,
                        "`harness-status {}` (dated={dated}, in_repo={in_repo}) changed HOME/cwd/TMPDIR (file set, content or mtime)",
                        args.join(" ")
                    );
                }
            }
        }
    }
}

#[test]
fn claim_hooks_json_single_session_start() {
    let v: serde_json::Value = serde_json::from_str(&manifest("hooks/hooks.json")).unwrap();
    let events = v["hooks"].as_object().unwrap();
    assert_eq!(events.keys().collect::<Vec<_>>(), vec!["SessionStart"]);
    let groups = events["SessionStart"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "exactly one SessionStart group");
    let hooks = groups[0]["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 1, "exactly one hook command");
    assert_eq!(
        hooks[0]["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/harness-status session-start"
    );
}

#[test]
fn claim_budget_from_ledger() {
    let sb = Sb::new();
    sb.ledger(serde_json::json!({"days":{
        DAY: {"sessions": {"s1": 1.25, "s2": 2.0}},
        "2026-09-23": {"sessions": {"s3": 9.0, "s4": 9.0, "s5": 9.0}},
    }}));
    let o = sb.run(&["budget"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        o.stdout.trim(),
        format!("Today ({DAY}): $3.2500 across 2 session(s)")
    );
    assert!(!o.stdout.contains("Recent sessions") && !o.stdout.contains("Progress"));
}

#[test]
fn claim_sessions_from_gauge_limit() {
    // FALSE in part: the `sessions` text rows carry id|project|turns|cost but NO
    // tokens; tokens appear in `--json` (total_tokens) and in the bare dashboard.
    let sb = Sb::new();
    sb.gauge_record(
        "sessold1-aaaa",
        "poldest",
        1,
        "2026-09-20T00:00:00Z",
        1_000_000,
    );
    sb.gauge_record(
        "sessmid2-bbbb",
        "pmiddle",
        2,
        "2026-09-21T00:00:00Z",
        2_000_000,
    );
    sb.gauge_record(
        "sessnew3-cccc",
        "pnewer",
        3,
        "2026-09-22T00:00:00Z",
        3_000_000,
    );
    sb.gauge_record(
        "sessnew4-dddd",
        "pnewest",
        4,
        "2026-09-23T00:00:00Z",
        4_000_000,
    );

    let o = sb.run(&["sessions", "--sessions", "2"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "sessnew4 | pnewest | 4 turns | $20.0000",
            "sessnew3 | pnewer | 3 turns | $15.0000"
        ],
        "text rows: id | project | turns | cost (no tokens column)"
    );

    let j = sb.run(&["sessions", "--sessions", "2", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["session_id"], "sessnew4-dddd");
    assert_eq!(v[0]["total_tokens"], 4_000_000);
    assert_eq!(v[0]["turns"], 4);
    assert_eq!(v[0]["cost_usd"], 20.0);

    let d = sb.run(&["--sessions", "2"]);
    let row = d.stdout.lines().find(|l| l.contains("sessnew4")).unwrap();
    assert!(row.contains("4000000") && row.contains("$20.0000"), "{row}");
    assert!(!d.stdout.contains("sessmid2"));
}

/// User ruling 2026-10-27: the progress-file surface was removed. Even with a
/// `.claude/progress.md` present, the `progress` subcommand no longer exists
/// and neither the text dashboard nor `--json` carries a progress section.
#[test]
fn claim_no_progress_surface() {
    let sb = Sb::new();
    std::fs::create_dir_all(sb.work.join(".claude")).unwrap();
    std::fs::write(sb.work.join(".claude/progress.md"), "line-1\nline-2\n").unwrap();
    sb.ledger(serde_json::json!({"days":{DAY:{"sessions":{"a":1.0}}}}));

    let o = sb.run(&["progress"]);
    assert_ne!(
        o.code, 0,
        "the progress subcommand must be gone: {}",
        o.stdout
    );

    let full = sb.run(&[]);
    assert_eq!(full.code, 0, "{}", full.stderr);
    assert!(!full.stdout.contains("Progress file") && !full.stdout.contains("line-1"));

    let j = sb.run(&["--json"]);
    assert_eq!(j.code, 0, "{}", j.stderr);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert!(v.get("progress").is_none(), "no progress key: {v}");
}

#[test]
fn claim_hooks_latency_per_session() {
    let sb = Sb::new();
    sb.latency(&[
        ("donegate", "sessAAAA1111", 300),
        ("reviewgate", "sessAAAA1111", 400),
        ("donegate", "sessBBBB2222", 50),
        ("donegate", "sessBBBB2222", 25),
    ]);
    let o = sb.run(&["hooks"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let mut lines: Vec<&str> = o.stdout.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        vec![
            "sessAAAA | 700ms across 2 hooks",
            "sessBBBB | 75ms across 1 hooks"
        ]
    );
}

#[test]
fn claim_inject_per_turn() {
    let sb = Sb::new();
    sb.inject(&[
        ("turnAAAA00000001", "playbook", 100),
        ("turnAAAA00000001", "runbook", 250),
        ("turnBBBB00000002", "ctxrot", 40),
        ("turnBBBB00000002", "ctxrot", 2),
    ]);
    let o = sb.run(&["inject"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let mut lines: Vec<&str> = o.stdout.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        vec![
            "turnAAAA | 350 chars across 2 injectors",
            "turnBBBB | 42 chars across 1 injectors"
        ]
    );
}

#[test]
fn claim_hooks_budget_default_and_env() {
    let sb = Sb::new();
    sb.latency(&[
        ("donegate", "sessAAAA1111", 25000),
        ("reviewgate", "sessAAAA1111", 15000),
        ("donegate", "sessBBBB2222", 20000),
    ]);
    let flags = |o: &Out| -> Vec<String> {
        o.stdout
            .lines()
            .filter(|l| l.starts_with('⚠'))
            .map(String::from)
            .collect()
    };
    let a_default = "⚠ session sessAAAA Stop-hook total 40000ms exceeds budget 30000ms";

    let o = sb.run(&["hooks"]);
    assert_eq!(flags(&o), vec![a_default.to_string()], "{}", o.stdout);

    let o = sb.run_env(&["hooks"], &[("HARNESS_HOOK_LATENCY_BUDGET_MS", "50000")]);
    assert!(flags(&o).is_empty(), "{}", o.stdout);

    let o = sb.run_env(&["hooks"], &[("HARNESS_HOOK_LATENCY_BUDGET_MS", "garbage")]);
    assert_eq!(flags(&o), vec![a_default.to_string()], "{}", o.stdout);

    let o = sb.run_env(&["hooks"], &[("HARNESS_HOOK_LATENCY_BUDGET_MS", "10000")]);
    assert_eq!(
        flags(&o).len(),
        2,
        "both exceed a 10000ms budget: {}",
        o.stdout
    );
}

#[test]
fn claim_inject_budget_default_and_env() {
    let sb = Sb::new();
    sb.inject(&[
        ("turnAAAA00000001", "playbook", 15000),
        ("turnAAAA00000001", "runbook", 10000),
        ("turnBBBB00000002", "ctxrot", 10000),
    ]);
    let flags = |o: &Out| -> Vec<String> {
        o.stdout
            .lines()
            .filter(|l| l.starts_with('⚠'))
            .map(String::from)
            .collect()
    };
    let o = sb.run(&["inject"]);
    assert_eq!(
        flags(&o),
        vec!["⚠ turn turnAAAA injection total 25000 chars exceeds budget 20000".to_string()],
        "{}",
        o.stdout
    );
    let o = sb.run_env(&["inject"], &[("HARNESS_INJECT_BUDGET_CHARS", "30000")]);
    assert!(flags(&o).is_empty(), "{}", o.stdout);
    let o = sb.run_env(&["inject"], &[("HARNESS_INJECT_BUDGET_CHARS", "5000")]);
    assert_eq!(flags(&o).len(), 2, "{}", o.stdout);
}

/// The `additionalContext` is emitted NESTED under `hookSpecificOutput`
/// (hookEventName SessionStart) -- a top-level key is never delivered to the
/// model by Claude Code (GH 1343); this pins what the binary actually prints.
fn session_start_ctx(o: &Out) -> String {
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", o.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{v}"
    );
    assert!(v.get("additionalContext").is_none(), "{v}");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no hookSpecificOutput.additionalContext in {v}"))
        .to_string()
}

#[test]
fn claim_session_start_shadow_injects() {
    let sb = Sb::new();
    sb.write(".claude/settings.json", "{}");
    let cache = sb.plant_cache("vshadow");
    let stray = sb.home.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("vshadow"), b"#!/bin/sh\n").unwrap();
    let path = format!("{}:/usr/bin:/bin", stray.display());

    // Control: without the stray copy on PATH nothing is reported.
    let c = sb.run(&["session-start"]);
    assert_eq!(c.code, 0);
    assert!(c.stdout.trim().is_empty(), "control: {:?}", c.stdout);

    let o = sb.run_env(&["session-start"], &[("PATH", &path)]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let ctx = session_start_ctx(&o);
    assert!(ctx.contains("vshadow"), "{ctx}");
    assert!(
        ctx.contains(&stray.join("vshadow").display().to_string()),
        "{ctx}"
    );
    assert!(ctx.contains(&cache.display().to_string()), "{ctx}");
}

#[test]
#[cfg(unix)]
fn claim_session_start_undetermined_injects() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let sb = Sb::new();
    sb.write(".claude/settings.json", "{}");
    sb.plant_cache("vokplug");
    sb.plant_cache("vlockedplug");
    let locked = sb.home.join(".claude/plugins/cache/yukineko/vlockedplug");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _r = Restore(locked.clone());
    assert!(
        std::fs::read_dir(&locked).is_err(),
        "chmod 000 did not deny access; cannot build this fixture"
    );

    let o = sb.run(&["session-start"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let ctx = session_start_ctx(&o);
    assert!(
        ctx.contains("PATH-shadow scan が完了しませんでした"),
        "{ctx}"
    );
    assert!(
        ctx.contains("vlockedplug"),
        "names the unreadable dir: {ctx}"
    );
}

#[test]
fn claim_full_dashboard() {
    let sb = Sb::new();
    let o = sb.run(&[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let headers: Vec<&str> = o.stdout.lines().filter(|l| l.starts_with("── ")).collect();
    let titles: Vec<String> = headers
        .iter()
        .map(|h| h.trim_end_matches('─').trim().to_string())
        .collect();
    assert_eq!(
        titles,
        vec![
            "── Budget (budgetguard)",
            "── Recent sessions (gauge)",
            "── Stop-hook latency (donegate/reviewgate/propguard)",
            "── UserPromptSubmit injection (aggregate budget)",
            "── Hook binary health (settings.json)",
            "── PATH shadowing (stray binaries)",
        ]
    );
}

fn make_fixture_repo(root: &Path) {
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w(".claude-plugin/marketplace.json", "{}");
    for (name, hook) in [
        ("alwayson", Some("Stop")),
        ("eventy", Some("PreCompact")),
        ("manualp", None),
    ] {
        w(
            &format!("crates/{name}/.claude-plugin/plugin.json"),
            &format!(r#"{{"name":"{name}"}}"#),
        );
        if let Some(ev) = hook {
            w(
                &format!("crates/{name}/hooks/hooks.json"),
                &format!(r#"{{"hooks":{{"{ev}":[{{"hooks":[]}}]}}}}"#),
            );
        }
    }
}

#[test]
fn claim_plugins_cli() {
    let sb = Sb::new();
    let repo = sb.work.join("repo");
    make_fixture_repo(&repo);
    let mut c = sb.cmd(&["plugins"]);
    c.current_dir(repo.join("crates"));
    let o = collect(c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let expect = "ALWAYS-ON (1)\n  alwayson  —  Stop\n\nEVENT-SCOPED (1)\n  eventy  —  PreCompact\n\nMANUAL (1)\n  manualp  —  CLI only\n\n";
    assert_eq!(o.stdout, expect);

    // Outside any checkout the scan cannot run: observed behaviour is a
    // non-zero exit with an explanatory stderr (not a silent empty success).
    let o = sb.run(&["plugins"]);
    assert_eq!(o.code, 1, "stdout={:?} stderr={:?}", o.stdout, o.stderr);
    assert!(o.stderr.contains("could not scan"), "{}", o.stderr);
    assert!(o.stdout.is_empty(), "{:?}", o.stdout);
}

#[test]
fn claim_hooks_health_cli() {
    let sb = Sb::new();
    let present = Path::new(env!("CARGO_BIN_EXE_harness-status"))
        .display()
        .to_string();
    sb.write(
        ".claude/settings.json",
        &serde_json::json!({"hooks":{"Stop":[{"hooks":[
            {"type":"command","command":"/no/such/dir/ghostbin run"},
            {"type":"command","command": format!("{present} session-start")},
        ]}]}})
        .to_string(),
    );
    let o = sb.run(&["hooks-health"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        o.stdout.lines().collect::<Vec<_>>(),
        vec!["⚠ missing hook binary: Stop (/no/such/dir/ghostbin): /no/such/dir/ghostbin run"]
    );
    // Control: only a present binary registered.
    sb.write(
        ".claude/settings.json",
        &serde_json::json!({"hooks":{"Stop":[{"hooks":[
            {"type":"command","command": format!("{present} session-start")},
        ]}]}})
        .to_string(),
    );
    let o = sb.run(&["hooks-health"]);
    assert_eq!(o.stdout.trim(), "[all registered hook binaries present]");
}

#[test]
fn claim_json_any_subcommand() {
    let sb = Sb::new();
    sb.ledger(serde_json::json!({"days":{DAY:{"sessions":{"a":1.0}}}}));
    sb.gauge_record("sess-json-0001", "p", 1, "2026-09-24T00:00:00Z", 10);
    sb.latency(&[("donegate", "sessAAAA1111", 1)]);
    sb.inject(&[("turnAAAA00000001", "playbook", 1)]);
    sb.write(".claude/settings.json", "{}");
    let repo = sb.work.join("repo");
    make_fixture_repo(&repo);

    for sub in [
        vec![],
        vec!["budget"],
        vec!["sessions"],
        vec!["hooks"],
        vec!["inject"],
        vec!["hooks-health"],
        vec!["path-shadow"],
    ] {
        let mut args = sub.clone();
        args.push("--json");
        let o = sb.run(&args);
        assert_eq!(o.code, 0, "{args:?}: {}", o.stderr);
        serde_json::from_str::<serde_json::Value>(&o.stdout)
            .unwrap_or_else(|e| panic!("{args:?} stdout is not JSON ({e}): {:?}", o.stdout));
    }
    let mut c = sb.cmd(&["plugins", "--json"]);
    c.current_dir(&repo);
    let o = collect(c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["counts"]["total"], 3);
}

#[test]
fn claim_not_installed() {
    let sb = Sb::new();
    let o = sb.run(&[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stdout
            .contains("ledger.json not found — budgetguard not installed?"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("No session records found — gauge not installed?"),
        "{}",
        o.stdout
    );
}

#[test]
#[cfg(unix)]
fn claim_unknown_exit1() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let lock = |p: &Path| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            std::fs::read_dir(p).is_err(),
            "chmod 000 did not deny access"
        );
        Restore(p.to_path_buf())
    };

    // --- sessions: control (empty but readable store) -----------------------
    let sb = Sb::new();
    std::fs::create_dir_all(sb.home.join(".gauge/store/sessions")).unwrap();
    let c = sb.run(&["sessions"]);
    assert_eq!(c.code, 0);
    assert!(!c.stdout.contains("unknown") && !c.stderr.contains("unknown"));

    // --- sessions: unreadable store -----------------------------------------
    let _r = lock(&sb.home.join(".gauge/store/sessions"));
    let o = sb.run(&["sessions"]);
    assert_eq!(o.code, 1);
    // ACTUAL: the text form goes to STDERR (stdout is empty), not stdout.
    assert!(o.stdout.is_empty(), "{:?}", o.stdout);
    assert!(
        o.stderr
            .starts_with("sessions: unknown — gauge's session store could not be read:"),
        "{}",
        o.stderr
    );
    let o = sb.run(&["sessions", "--json"]);
    assert_eq!(o.code, 1);
    let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["status"], "unknown");
    assert!(v["reason"].as_str().unwrap().contains("could not be read"));
    // The bare dashboard says `unknown —` on stdout and still exits 0.
    let d = sb.run(&[]);
    assert!(
        d.stdout
            .contains("unknown — gauge's session store could not be read"),
        "{}",
        d.stdout
    );
    drop(_r);

    // --- path-shadow: control (readable cache) ------------------------------
    let sb = Sb::new();
    sb.plant_cache("vokplug");
    sb.plant_cache("vlockedplug");
    let c = sb.run(&["path-shadow"]);
    assert_eq!(c.code, 0);
    assert!(c.stdout.contains("no PATH-shadowed"));

    // --- path-shadow: unreadable plugin dir ---------------------------------
    let _r = lock(&sb.home.join(".claude/plugins/cache/yukineko/vlockedplug"));
    // ACTUAL (differs from the claim): stderr `harness-status: PATH-shadow scan
    // could not be completed: …`, NOT `unknown — …`; exit 1; and with --json
    // there is NO {"status":"unknown"} object on stdout.
    let o = sb.run(&["path-shadow"]);
    assert_eq!(o.code, 1);
    assert!(
        o.stderr
            .starts_with("harness-status: PATH-shadow scan could not be completed:")
            && o.stderr.contains("vlockedplug"),
        "{}",
        o.stderr
    );
    let o = sb.run(&["path-shadow", "--json"]);
    assert_eq!(o.code, 1);
    // Not pinned: stdout stays empty here (no {"status":"unknown"} object) --
    // reported as an open finding, deliberately not frozen as the spec.
    assert!(o.stderr.contains("could not be completed"), "{}", o.stderr);
    // The bare dashboard does print `unknown —` for path-shadow.
    let d = sb.run(&[]);
    assert!(
        d.stdout
            .contains("unknown — PATH-shadow scan could not be completed"),
        "{}",
        d.stdout
    );
    let dj = sb.run(&["--json"]);
    let v: serde_json::Value = serde_json::from_str(&dj.stdout).unwrap();
    assert_eq!(v["path_shadow"]["status"], "unknown");
}

/// Independent civil-date computation (Hinnant's days-to-civil), deliberately
/// not the binary's loop.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn utc_today() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    civil_from_days((secs / 86_400) as i64)
}

#[test]
fn claim_date_system_clock() {
    let sb = Sb::new();
    let d0 = utc_today();
    let tomorrow = civil_from_days(
        (SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86_400) as i64
            + 1,
    );
    sb.ledger(serde_json::json!({"days":{
        d0.clone(): {"sessions": {"s1": 7.5}},
        tomorrow: {"sessions": {"s2": 99.0}},
        DAY: {"sessions": {"s3": 55.0}},
    }}));
    let mut c = sb.cmd(&["budget"]);
    c.env_remove("HARNESS_DATE");
    let o = collect(c);
    let d1 = utc_today();
    assert_eq!(o.code, 0, "{}", o.stderr);
    // Tolerate a UTC-midnight rollover between the two clock reads.
    assert!(
        o.stdout.trim() == format!("Today ({d0}): $7.5000 across 1 session(s)")
            || (d1 != d0 && o.stdout.starts_with(&format!("Today ({d1})"))),
        "stdout={:?} expected date {d0}",
        o.stdout
    );
}

#[test]
fn claim_license_mit() {
    let member: toml::Value = manifest("Cargo.toml").parse().unwrap();
    assert_eq!(
        member["package"]["license"]["workspace"].as_bool(),
        Some(true)
    );
    let root: toml::Value = manifest("../../Cargo.toml").parse().unwrap();
    assert_eq!(
        root["workspace"]["package"]["license"].as_str(),
        Some("MIT")
    );
}

#[test]
fn claim_path_shadow_cli() {
    let sb = Sb::new();
    let cache = sb.plant_cache("vshadow");
    let stray = sb.home.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    let path = format!("{}:/usr/bin:/bin", stray.display());
    let stray_bin = stray.join("vshadow").display().to_string();

    // Control: nothing earlier on PATH shadows the cache copy.
    let c = sb.run_env(&["path-shadow"], &[("PATH", &path)]);
    assert_eq!(c.code, 0, "{}", c.stderr);
    assert_eq!(c.stdout.trim(), "[no PATH-shadowed plugin binaries]");

    std::fs::write(stray.join("vshadow"), b"#!/bin/sh\n").unwrap();
    let o = sb.run_env(&["path-shadow"], &[("PATH", &path)]);
    // Observed: a detected shadow still exits 0.
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        o.stdout.trim(),
        format!(
            "⚠ shadowed binary: vshadow — {stray_bin} shadows plugin cache {}",
            cache.display()
        )
    );

    let j = sb.run_env(&["path-shadow", "--json"], &[("PATH", &path)]);
    assert_eq!(j.code, 0, "{}", j.stderr);
    let v: serde_json::Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v[0]["name"], "vshadow");
    assert_eq!(v[0]["shadowing_path"], stray_bin.as_str());
    assert_eq!(v[0]["cache_path"], cache.display().to_string().as_str());
}
