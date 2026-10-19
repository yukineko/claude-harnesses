#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests: each `claim_*` fn pins one README/prose claim
//! about budgetguard to an observed behaviour of the real binary.
//!
//! What this does NOT prove: anything about a real Claude Code host; the
//! missing-transcript / malformed-stdin silent-allow paths are deliberately
//! not pinned.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

struct Env {
    home: TempDir,
    proj: TempDir,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn env() -> Env {
    Env {
        home: tempfile::tempdir().unwrap(),
        proj: tempfile::tempdir().unwrap(),
    }
}

fn run_with(e: &Env, args: &[&str], stdin: &str, extra: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_budgetguard"));
    cmd.args(args)
        .env("HOME", e.home.path())
        .env_remove("BUDGETGUARD_DISABLE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(e.proj.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn write_cfg(e: &Env, toml: &str) {
    std::fs::write(e.proj.path().join("budgetguard.toml"), toml).unwrap();
}

fn transcript(e: &Env, name: &str, model: &str, input: u64, output: u64) -> PathBuf {
    let p = e.proj.path().join(name);
    std::fs::write(
        &p,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-06-22T10:00:01Z\",\"message\":{{\"model\":\"{model}\",\"content\":[{{\"type\":\"text\",\"text\":\"x\"}}],\"usage\":{{\"input_tokens\":{input},\"output_tokens\":{output},\"cache_read_input_tokens\":0}}}}}}\n"
        ),
    )
    .unwrap();
    p
}

fn payload(e: &Env, session: &str, tp: &Path, active: bool) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "transcript_path": tp.to_str().unwrap(),
        "cwd": e.proj.path().to_str().unwrap(),
        "stop_hook_active": active,
    })
    .to_string()
}

fn gate(e: &Env, session: &str, tp: &Path, active: bool) -> Out {
    run_with(e, &["gate"], &payload(e, session, tp, active), &[])
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s.trim()).unwrap_or_else(|err| panic!("stdout not JSON ({err}): {s:?}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ---- packaging ----

#[test]
fn claim_hooks_json_stop_gate() {
    let t = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
        .unwrap();
    let v = json(&t);
    let hooks = v["hooks"].as_object().unwrap();
    assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["Stop"]);
    assert_eq!(hooks["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        hooks["Stop"][0]["hooks"][0]["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/budgetguard gate"
    );
}

#[test]
fn claim_marketplace_entry() {
    let t = std::fs::read_to_string(repo_root().join(".claude-plugin/marketplace.json")).unwrap();
    let v = json(&t);
    let e = v["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "budgetguard")
        .expect("budgetguard entry");
    assert_eq!(e["source"]["path"], "crates/budgetguard");
}

#[test]
fn claim_license_mit() {
    let t =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let v: toml::Value = toml::from_str(&t).unwrap_or_else(|_| panic!("parse"));
    assert_eq!(v["package"]["license"].as_str(), Some("MIT"));
}

// ---- verdicts ----

#[test]
fn claim_verdict_outputs() {
    // $30 opus session.
    let cases: [(&str, &str); 3] = [
        ("[session]\nwarn_usd=100\nblock_usd=200\n", "allow"),
        ("[session]\nwarn_usd=10\nblock_usd=200\n", "warn"),
        ("[session]\nwarn_usd=10\nblock_usd=20\n", "block"),
    ];
    for (cfg, kind) in cases {
        let e = env();
        write_cfg(&e, cfg);
        let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 1_000_000);
        let o = gate(&e, "s-verdict", &tp, false);
        assert_eq!(o.code, 0, "{kind}: {}", o.stderr);
        match kind {
            "allow" => assert_eq!(o.stdout.trim(), "", "allow stdout"),
            "warn" => {
                let v = json(&o.stdout);
                assert!(v["additionalContext"]
                    .as_str()
                    .unwrap()
                    .contains("budgetguard"));
                assert!(v.get("decision").is_none());
            }
            _ => {
                let v = json(&o.stdout);
                assert_eq!(v["decision"], "block");
                let r = v["reason"].as_str().unwrap();
                assert!(r.contains("保存") && r.contains("コミット"), "reason: {r}");
            }
        }
    }
}

// ---- ledger ----

fn ledger_json(e: &Env) -> serde_json::Value {
    let p = e.home.path().join(".budgetguard/state/ledger.json");
    json(&std::fs::read_to_string(p).expect("ledger.json written"))
}

fn only_day(v: &serde_json::Value) -> (String, f64) {
    let days = v["days"].as_object().unwrap();
    assert_eq!(days.len(), 1, "one day key: {v}");
    let (k, d) = days.iter().next().unwrap();
    let total: f64 = d["sessions"]
        .as_object()
        .unwrap()
        .values()
        .map(|x| x.as_f64().unwrap())
        .sum();
    (k.clone(), total)
}

#[test]
fn claim_ledger_latest_cost() {
    let e = env();
    let t1 = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0); // $5
    assert_eq!(gate(&e, "s-led", &t1, false).code, 0);
    let (_, first) = only_day(&ledger_json(&e));
    assert!((first - 5.0).abs() < 1e-9, "first {first}");
    let t2 = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 1_000_000); // $30
    assert_eq!(gate(&e, "s-led", &t2, false).code, 0);
    let (_, second) = only_day(&ledger_json(&e));
    assert!((second - 30.0).abs() < 1e-9, "latest, not sum 35: {second}");
}

#[test]
fn claim_day_key_local_midnight() {
    use chrono::{Duration, FixedOffset, Utc};
    let mut any_differs = false;
    for (tz, hours) in [("Pacific/Kiritimati", 14), ("Etc/GMT+12", -12)] {
        let e = env();
        let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
        let before = Utc::now();
        let o = run_with(
            &e,
            &["gate"],
            &payload(&e, "s-tz", &tp, false),
            &[("TZ", tz)],
        );
        let after = Utc::now();
        assert_eq!(o.code, 0, "{}", o.stderr);
        let (key, _) = only_day(&ledger_json(&e));
        let off = FixedOffset::east_opt(hours * 3600).unwrap();
        let fmt = |t: chrono::DateTime<Utc>| t.with_timezone(&off).format("%Y-%m-%d").to_string();
        assert!(
            key == fmt(before) || key == fmt(after),
            "{tz}: key {key} != local date {}",
            fmt(before)
        );
        let utc = |t: chrono::DateTime<Utc>| t.format("%Y-%m-%d").to_string();
        if key != utc(before) && key != utc(after) {
            any_differs = true;
        }
        let _ = Duration::zero();
    }
    assert!(any_differs, "no TZ produced a local date != UTC date");
}

#[test]
fn claim_ledger_lock_serializes() {
    let e = env();
    let state = e.home.path().join(".budgetguard/state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join("ledger.lock"))
        .unwrap();
    lock.lock().unwrap();
    write_cfg(&e, "[daily]\nblock_usd = 1000\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let o = gate(&e, "s-lock", &tp, false);
    assert!(
        !state.join("ledger.json").exists(),
        "ledger written while lock held"
    );
    assert!(o.stderr.contains("day unknown"), "stderr: {}", o.stderr);
    let v = json(&o.stdout);
    assert_eq!(v["decision"], "block");
    drop(lock);
}

// ---- stop-hook control flow ----

#[test]
fn claim_reentry_allowed() {
    let e = env();
    write_cfg(&e, "[session]\nblock_usd = 1.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let control = gate(&e, "s-re", &tp, false);
    assert_eq!(json(&control.stdout)["decision"], "block");
    let o = gate(&e, "s-re", &tp, true);
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout.trim(), "");
}

#[test]
fn claim_disable_env() {
    let e = env();
    write_cfg(&e, "[session]\nblock_usd = 1.0\nthis is not toml\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let p = payload(&e, "s-dis", &tp, false);
    let control = run_with(&e, &["gate"], &p, &[]);
    assert_eq!(json(&control.stdout)["decision"], "block");
    let o = run_with(&e, &["gate"], &p, &[("BUDGETGUARD_DISABLE", "1")]);
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout, "");
}

#[test]
fn claim_block_records_violation() {
    fn count(e: &Env) -> usize {
        let root = e.home.path().join(".overwatch");
        let mut n = 0;
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|f| f == "violations.jsonl") {
                    let t = std::fs::read_to_string(&p).unwrap();
                    assert!(t.contains("budgetguard"), "{t}");
                    n += t.lines().filter(|l| !l.is_empty()).count();
                }
            }
        }
        n
    }
    let e = env();
    write_cfg(&e, "[session]\nblock_usd = 1000.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    assert_eq!(gate(&e, "s-v", &tp, false).stdout.trim(), "");
    assert_eq!(count(&e), 0, "allow adds none");
    write_cfg(&e, "[session]\nblock_usd = 1.0\n");
    let o = gate(&e, "s-v", &tp, false);
    assert_eq!(json(&o.stdout)["decision"], "block");
    assert_eq!(count(&e), 1);
}

// ---- undetermined paths ----

#[test]
fn claim_session_undetermined_warn_only() {
    let e = env();
    write_cfg(&e, "[session]\nwarn_usd = 1.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-zzz-9", 1_000_000, 0);
    let o = gate(&e, "s-u1", &tp, false);
    assert_eq!(o.code, 0);
    let v = json(&o.stdout);
    assert!(v["additionalContext"]
        .as_str()
        .unwrap()
        .contains("測定できません"));
    assert!(v.get("decision").is_none());
    assert!(o.stderr.contains("session unknown"), "{}", o.stderr);

    write_cfg(&e, "[session]\nwarn_usd = 1.0\nblock_usd = 2.0\n");
    let o = gate(&e, "s-u1", &tp, false);
    assert_eq!(json(&o.stdout)["decision"], "block");
}

fn corrupt_ledger(e: &Env) -> (PathBuf, Vec<u8>) {
    let state = e.home.path().join(".budgetguard/state");
    std::fs::create_dir_all(&state).unwrap();
    let p = state.join("ledger.json");
    let bytes = b"{not json".to_vec();
    std::fs::write(&p, &bytes).unwrap();
    (p, bytes)
}

#[test]
fn claim_day_undetermined_warn_only() {
    let e = env();
    let (lp, bytes) = corrupt_ledger(&e);
    write_cfg(&e, "[daily]\nwarn_usd = 1000.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let o = gate(&e, "s-d1", &tp, false);
    let v = json(&o.stdout);
    assert!(v["additionalContext"]
        .as_str()
        .unwrap()
        .contains("測定できません"));
    assert!(v.get("decision").is_none());
    assert_eq!(std::fs::read(&lp).unwrap(), bytes);

    write_cfg(&e, "[daily]\nblock_usd = 1000.0\n");
    let o = gate(&e, "s-d1", &tp, false);
    assert_eq!(json(&o.stdout)["decision"], "block");
    assert_eq!(std::fs::read(&lp).unwrap(), bytes);
}

#[test]
fn claim_unknown_not_zero() {
    let e = env();
    corrupt_ledger(&e);
    write_cfg(&e, "[daily]\nwarn_usd = 1000.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let o = gate(&e, "s-un", &tp, false);
    assert!(o.stderr.contains("day unknown"), "{}", o.stderr);
    assert!(!o.stderr.contains("day $0"), "{}", o.stderr);
    assert!(o.stderr.contains("session $5.0000"), "{}", o.stderr);
}

// ---- pricing ----

#[test]
fn claim_pricing_table() {
    for (model, cost) in [
        ("claude-fable-1", "60.0000"),
        ("claude-mythos-1", "60.0000"),
        ("claude-opus-4-8", "30.0000"),
        ("claude-sonnet-4-5", "18.0000"),
        ("claude-haiku-4-5", "6.0000"),
    ] {
        let e = env();
        let tp = transcript(&e, "t.jsonl", model, 1_000_000, 1_000_000);
        let o = gate(&e, "s-price", &tp, false);
        assert!(
            o.stderr.contains(&format!("session ${cost}")),
            "{model}: {}",
            o.stderr
        );
    }
}

// ---- cache health ----

fn gauge_record(e: &Env, session: &str, input: u64, cache_read: u64) {
    let dir = e.home.path().join(".gauge/store/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let rec = serde_json::json!({
        "session_id": session, "project": "p", "cwd": "/x",
        "models": {"claude-opus-4-8": {"input": input, "output": 0,
            "cache_write_5m": 0, "cache_write_1h": 0, "cache_read": cache_read}},
        "turns": 1, "updated_at": "2000-01-01T00:00:00Z"
    });
    std::fs::write(dir.join(format!("{session}.json")), rec.to_string()).unwrap();
}

fn cache_run(cfg: &str, input: u64, cache_read: u64) -> Out {
    cache_run_cost(cfg, input, cache_read, 10)
}

fn cache_run_cost(cfg: &str, input: u64, cache_read: u64, transcript_input: u64) -> Out {
    let e = env();
    if !cfg.is_empty() {
        write_cfg(&e, cfg);
    }
    gauge_record(&e, "s-c", input, cache_read);
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", transcript_input, 0);
    gate(&e, "s-c", &tp, false)
}

#[test]
fn claim_cache_health_promoted_warning() {
    let o = cache_run("", 300_000, 0);
    assert_eq!(o.code, 0);
    assert!(o.stderr.contains("cache hit rate"), "{}", o.stderr);
    let v = json(&o.stdout);
    assert!(v["additionalContext"].as_str().unwrap().contains("cache"));
    assert!(v.get("decision").is_none());
}

#[test]
fn claim_cache_health_below_floor_not_promoted() {
    let o = cache_run("", 100_000, 0);
    assert!(o.stderr.contains("cache unknown"), "{}", o.stderr);
    assert_eq!(o.stdout.trim(), "");
    // [cache].min_tokens lowers the floor.
    let o = cache_run("[cache]\nmin_tokens = 50000\n", 100_000, 0);
    assert!(json(&o.stdout)["additionalContext"]
        .as_str()
        .unwrap()
        .contains("cache"));
}

#[test]
fn claim_cache_health_out_of_range_rate_uses_default() {
    // rate = 450000/750000 = 0.6: healthy at default 0.5, degraded if 5.0 were honoured.
    let o = cache_run("[cache]\nmin_rate = 5.0\n", 300_000, 450_000);
    assert_eq!(o.stdout.trim(), "", "{}", o.stderr);
    // rate = 0.4: degraded at default 0.5.
    let o = cache_run("[cache]\nmin_rate = 5.0\n", 300_000, 200_000);
    assert!(json(&o.stdout)["additionalContext"]
        .as_str()
        .unwrap()
        .contains("cache"));
}

#[test]
fn claim_cache_health_never_blocks_louder_wins() {
    let o = cache_run_cost("[session]\nblock_usd = 1.0\n", 300_000, 0, 1_000_000);
    let v = json(&o.stdout);
    assert_eq!(v["decision"], "block");
    assert!(v.get("additionalContext").is_none());
    assert!(o.stderr.contains("cache hit rate"));
}

// ---- CLI: init / install / status / config ----

#[test]
fn claim_init_writes_starter() {
    let e = env();
    let path = e.proj.path().join("budgetguard.toml");
    let o = run_with(&e, &["init"], "", &[]);
    assert_eq!(o.code, 0);
    let first = std::fs::read_to_string(&path).unwrap();
    let _: toml::Value = toml::from_str(&first).unwrap_or_else(|_| panic!("starter parses"));
    std::fs::write(&path, "# mine\n").unwrap();
    let o = run_with(&e, &["init"], "", &[]);
    assert_eq!(o.code, 0);
    assert!(o.stderr.contains("--force"), "{}", o.stderr);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
    let o = run_with(&e, &["init", "--force"], "", &[]);
    assert_eq!(o.code, 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
}

fn settings(e: &Env) -> PathBuf {
    e.home.path().join(".claude/settings.json")
}

fn stop_commands(v: &serde_json::Value) -> Vec<String> {
    v["hooks"]["Stop"]
        .as_array()
        .map(|a| {
            a.iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn claim_install_merges_stop_hook() {
    let e = env();
    std::fs::create_dir_all(e.home.path().join(".claude")).unwrap();
    let pre = r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/other/tool run"}]}]}}"#;
    std::fs::write(settings(&e), pre).unwrap();

    let o = run_with(&e, &["install", "--dry-run"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        std::fs::read_to_string(settings(&e)).unwrap(),
        pre,
        "dry-run wrote"
    );

    let o = run_with(&e, &["install"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = json(&std::fs::read_to_string(settings(&e)).unwrap());
    assert_eq!(v["theme"], "dark");
    let cmds = stop_commands(&v);
    assert!(cmds.contains(&"/other/tool run".to_string()), "{cmds:?}");
    assert!(
        cmds.iter()
            .any(|c| c.contains("budgetguard") && c.ends_with(" gate")),
        "{cmds:?}"
    );

    let installed = std::fs::read_to_string(settings(&e)).unwrap();
    let o = run_with(&e, &["uninstall", "--dry-run"], "", &[]);
    assert_eq!(o.code, 0);
    assert_eq!(std::fs::read_to_string(settings(&e)).unwrap(), installed);

    let o = run_with(&e, &["uninstall"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = json(&std::fs::read_to_string(settings(&e)).unwrap());
    assert_eq!(v["theme"], "dark");
    assert_eq!(stop_commands(&v), vec!["/other/tool run".to_string()]);
}

#[test]
fn claim_status_json_fields() {
    let e = env();
    write_cfg(&e, "[daily]\nwarn_usd = 5.0\nblock_usd = 9.0\n");
    let o = run_with(&e, &["status", "--json"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = json(&o.stdout);
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["daily_block_usd", "daily_warn_usd", "day_usd", "pressure"]
    );
    assert_eq!(v["daily_warn_usd"], 5.0);
    assert_eq!(v["daily_block_usd"], 9.0);

    write_cfg(&e, "not = [toml\n");
    let o = run_with(&e, &["status", "--json"], "", &[]);
    assert_eq!(o.code, 2);
    let v = json(&o.stdout);
    assert_eq!(v["undetermined"], true);
    assert!(v["error"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(v.get("pressure").is_none());
}

#[test]
fn claim_config_lookup_home_and_project_not_merged() {
    let e = env();
    let bg = e.home.path().join(".budgetguard");
    std::fs::create_dir_all(&bg).unwrap();
    std::fs::write(bg.join("config.toml"), "[daily]\nblock_usd = 7.0\n").unwrap();
    let o = run_with(&e, &["status", "--json"], "", &[]);
    assert_eq!(
        json(&o.stdout)["daily_block_usd"],
        7.0,
        "home config applied"
    );

    write_cfg(&e, "[session]\nblock_usd = 3.0\n");
    let o = run_with(&e, &["status", "--json"], "", &[]);
    assert_eq!(
        json(&o.stdout)["daily_block_usd"],
        0.0,
        "project file replaces home file (not merged)"
    );
}

#[test]
fn claim_no_trust_gate() {
    let e = env();
    write_cfg(&e, "[session]\nblock_usd = 1.0\n");
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
    let o = gate(&e, "s-trust", &tp, false);
    assert_eq!(json(&o.stdout)["decision"], "block");
}

#[test]
fn claim_example_toml() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("budgetguard.example.toml");
    let text = std::fs::read_to_string(&ex).unwrap();
    for k in ["enabled", "state_dir", "session", "daily", "price"] {
        assert!(text.contains(k), "example lacks {k}");
    }
    let e = env();
    write_cfg(&e, &text);
    let o = run_with(&e, &["status"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);

    // enabled=false -> no-op on over-budget.
    let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 1_000_000);
    write_cfg(&e, "enabled = false\n[session]\nblock_usd = 1.0\n");
    let o = gate(&e, "s-ex", &tp, false);
    assert_eq!((o.code, o.stdout.as_str()), (0, ""));
    // control: enabled -> block.
    write_cfg(&e, "[session]\nblock_usd = 1.0\n");
    assert_eq!(
        json(&gate(&e, "s-ex", &tp, false).stdout)["decision"],
        "block"
    );

    // state_dir override -> ledger written there, not at the default.
    let sd = e.proj.path().join("custom-state");
    write_cfg(&e, &format!("state_dir = {:?}\n", sd.to_str().unwrap()));
    let o = gate(&e, "s-ex2", &tp, false);
    assert_eq!(o.code, 0);
    assert!(sd.join("ledger.json").exists());
}

// ---- round 2 ----

/// Write a gauge session record with `input` opus input tokens and the given
/// `updated_at`, under the temp HOME's gauge store.
fn gauge_record_at(e: &Env, session: &str, input: u64, updated_at: &str) {
    let dir = e.home.path().join(".gauge/store/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let rec = serde_json::json!({
        "session_id": session, "project": "p", "cwd": "/x",
        "models": {"claude-opus-4-8": {"input": input, "output": 0,
            "cache_write_5m": 0, "cache_write_1h": 0, "cache_read": 0}},
        "turns": 1, "updated_at": updated_at
    });
    std::fs::write(dir.join(format!("{session}.json")), rec.to_string()).unwrap();
}

fn set_mtime(p: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

// 2030-01-01T00:00:00Z
const T2030: u64 = 1_893_456_000;

#[test]
fn claim_cost_source_freshness() {
    // record = 2M opus input = $10; transcript = 1M opus input = $5.
    let cases: [(&str, &str, &str); 3] = [
        ("2031-01-01T00:00:00Z", "session $10.0000", "newer record"),
        (
            "2030-01-01T00:00:00Z",
            "session $10.0000",
            "record == mtime",
        ),
        ("2029-12-31T23:59:59Z", "session $5.0000", "older record"),
    ];
    for (updated_at, want, label) in cases {
        let e = env();
        let tp = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0);
        set_mtime(&tp, T2030);
        gauge_record_at(&e, "s-fresh", 2_000_000, updated_at);
        let o = gate(&e, "s-fresh", &tp, false);
        assert_eq!(o.code, 0, "{label}: {}", o.stderr);
        assert!(
            o.stderr.contains(want),
            "{label}: want {want}: {}",
            o.stderr
        );
    }
}

#[test]
fn claim_day_cumulative() {
    let e = env();
    write_cfg(&e, "[daily]\nblock_usd = 8.0\n");
    let t = transcript(&e, "t.jsonl", "claude-opus-4-8", 1_000_000, 0); // $5
    let o1 = gate(&e, "s-day-a", &t, false);
    assert_eq!(o1.stdout.trim(), "", "first session allowed: {}", o1.stderr);
    let o2 = gate(&e, "s-day-b", &t, false);
    assert_eq!(json(&o2.stdout)["decision"], "block", "{}", o2.stderr);
    let (_, total) = only_day(&ledger_json(&e));
    assert!((total - 10.0).abs() < 1e-9, "day total {total}");
}

#[test]
fn claim_cache_min_tokens_default_boundary() {
    // input + cache_read == 199_999 -> below the default 200_000 floor.
    let o = cache_run("", 199_999, 0);
    assert_eq!(o.stdout.trim(), "", "{}", o.stderr);
    assert!(o.stderr.contains("cache unknown"), "{}", o.stderr);
    // Same total split across input and cache_read (rate 0.25, degraded if judged).
    let o = cache_run("", 150_000, 49_999);
    assert_eq!(o.stdout.trim(), "", "{}", o.stderr);
    // == 200_000 -> judged, degraded, promoted.
    let o = cache_run("", 200_000, 0);
    assert!(
        json(&o.stdout)["additionalContext"]
            .as_str()
            .unwrap()
            .contains("cache"),
        "{}",
        o.stderr
    );
    let o = cache_run("", 150_000, 50_000);
    assert!(
        json(&o.stdout)["additionalContext"].is_string(),
        "{}",
        o.stderr
    );
}

#[test]
fn claim_cache_min_rate_default_and_inrange() {
    // exactly 0.5 with default: not strictly below -> not promoted.
    let o = cache_run("", 300_000, 300_000);
    assert_eq!(o.stdout.trim(), "", "rate 0.5: {}", o.stderr);
    // 0.49 -> promoted.
    let o = cache_run("", 510_000, 490_000);
    assert!(
        json(&o.stdout)["additionalContext"].is_string(),
        "0.49: {}",
        o.stderr
    );
    // in-range min_rate honoured: rate 0.4 is healthy at 0.3 ...
    let o = cache_run("[cache]\nmin_rate = 0.3\n", 300_000, 200_000);
    assert_eq!(o.stdout.trim(), "", "0.3/0.4: {}", o.stderr);
    // ... and degraded at default (control), and at min_rate = 1.0 for rate 0.6.
    let o = cache_run("", 300_000, 200_000);
    assert!(
        json(&o.stdout)["additionalContext"].is_string(),
        "{}",
        o.stderr
    );
    let o = cache_run("[cache]\nmin_rate = 1.0\n", 200_000, 300_000);
    assert!(
        json(&o.stdout)["additionalContext"].is_string(),
        "1.0/0.6: {}",
        o.stderr
    );
    // Out-of-range 0 and negative: observed to fall back to the default (check
    // stays alive: 0.4 promoted, 0.6 not), i.e. they do NOT disable the check.
    for bad in ["0", "0.0", "-1.0"] {
        let cfg = format!("[cache]\nmin_rate = {bad}\n");
        let o = cache_run(&cfg, 300_000, 200_000);
        assert!(
            json(&o.stdout)["additionalContext"].is_string(),
            "{bad}/0.4: {}",
            o.stderr
        );
        let o = cache_run(&cfg, 200_000, 300_000);
        assert_eq!(o.stdout.trim(), "", "{bad}/0.6: {}", o.stderr);
    }
}

#[test]
fn claim_session_undetermined_daily_arms() {
    let e = env();
    let tp = transcript(&e, "t.jsonl", "claude-zzz-9", 1_000_000, 0);
    write_cfg(&e, "[daily]\nblock_usd = 1000.0\n");
    let o = gate(&e, "s-ud", &tp, false);
    assert_eq!(json(&o.stdout)["decision"], "block", "{}", o.stderr);
    assert!(o.stderr.contains("session unknown"), "{}", o.stderr);

    write_cfg(&e, "[daily]\nwarn_usd = 1000.0\n");
    let o = gate(&e, "s-ud", &tp, false);
    let v = json(&o.stdout);
    assert!(v["additionalContext"]
        .as_str()
        .unwrap()
        .contains("測定できません"));
    assert!(v.get("decision").is_none());
    assert!(o.stderr.contains("session unknown"), "{}", o.stderr);

    // control: nothing armed -> allow.
    write_cfg(&e, "");
    let o = gate(&e, "s-ud", &tp, false);
    assert_eq!(o.stdout.trim(), "", "{}", o.stderr);
}

#[test]
fn claim_pricing_split() {
    // (model, input rate, output rate) per 1M tokens.
    for (model, inp, out) in [
        ("claude-fable-1", "10.0000", "50.0000"),
        ("claude-mythos-1", "10.0000", "50.0000"),
        ("claude-opus-4-8", "5.0000", "25.0000"),
        ("claude-sonnet-4-5", "3.0000", "15.0000"),
        ("claude-haiku-4-5", "1.0000", "5.0000"),
    ] {
        let e = env();
        let t_in = transcript(&e, "in.jsonl", model, 1_000_000, 0);
        let o = gate(&e, "s-pi", &t_in, false);
        assert!(
            o.stderr.contains(&format!("session ${inp}")),
            "{model} input: {}",
            o.stderr
        );
        let t_out = transcript(&e, "out.jsonl", model, 0, 1_000_000);
        let o = gate(&e, "s-po", &t_out, false);
        assert!(
            o.stderr.contains(&format!("session ${out}")),
            "{model} output: {}",
            o.stderr
        );
    }
}

#[test]
fn claim_example_commented() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("budgetguard.example.toml");
    let text = std::fs::read_to_string(&ex).unwrap();
    let v: toml::Value = toml::from_str(&text).unwrap_or_else(|_| panic!("example parses"));
    let t = v.as_table().unwrap();
    for k in ["enabled", "session", "daily"] {
        assert!(t.contains_key(k), "active key {k} missing");
    }
    for k in ["state_dir", "price"] {
        assert!(!t.contains_key(k), "{k} must be commented out");
    }
    assert!(text.lines().any(|l| l.starts_with("# state_dir")));
    assert!(text.lines().any(|l| l.trim() == "# [[price]]"));
}

#[test]
fn claim_status_json_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    // Normal case first: no spend -> day_usd 0, pressure false (warn armed).
    let e = env();
    write_cfg(&e, "[daily]\nwarn_usd = 5.0\n");
    let o = run_with(&e, &["status", "--json"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = json(&o.stdout);
    assert_eq!(v["day_usd"], 0.0);
    assert_eq!(v["pressure"], false);

    let cfg = e.proj.path().join("budgetguard.toml");
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Cannot set up the unreadable case (e.g. running as root): fail, never
    // pass silently without having observed the claim.
    assert!(
        std::fs::read(&cfg).is_err(),
        "fixture: config still readable after chmod 000 (running as root?)"
    );
    let o = run_with(&e, &["status", "--json"], "", &[]);
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(o.code, 2, "{}", o.stderr);
    let v = json(&o.stdout);
    assert_eq!(v["undetermined"], true);
    assert!(v["error"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(v.get("pressure").is_none());
}
