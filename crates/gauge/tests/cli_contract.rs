// Integration test file: unwrap/expect/panic are fine here (the workspace
// clippy denies are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! One test per prose claim in `crates/gauge/README.md` / `README.ja.md`.
//!
//! Every test drives the real `gauge` binary under a fresh temp HOME (and a
//! temp cwd) and asserts the specific outcome the prose states.
//!
//! What this file does NOT prove:
//! * `claim_budgetguard_unmeasured` is deliberately absent: that behaviour lives
//!   in budgetguard and is covered there (see the note at the bottom).
//! * Nothing here exercises a real Claude Code session, the cross-built
//!   `bin/gauge-*` binaries, or `make`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

// ---------------------------------------------------------------- apparatus

struct Env {
    _guard: tempfile::TempDir,
    /// Canonical temp root; also `$HOME`.
    home: PathBuf,
}

impl Env {
    fn new() -> Env {
        let g = tempfile::tempdir().unwrap();
        // Canonical so `current_dir()` inside the child agrees with what we
        // compare against (macOS /var -> /private/var).
        let home = g.path().canonicalize().unwrap();
        Env { _guard: g, home }
    }

    /// A project directory under HOME (its basename is the project name).
    fn proj(&self, name: &str) -> PathBuf {
        let p = self.home.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn store_sessions(&self) -> PathBuf {
        self.home.join(".gauge/store/sessions")
    }

    fn session_file(&self, sid: &str) -> PathBuf {
        self.store_sessions().join(format!("{sid}.json"))
    }

    fn session_file_count(&self) -> usize {
        match std::fs::read_dir(self.store_sessions()) {
            Ok(rd) => rd.count(),
            // Only a store that does not exist yet means "no sessions"; any
            // other read error must not be counted as an empty store.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => panic!("apparatus: cannot read store sessions dir: {e}"),
        }
    }

    fn record_json(&self, sid: &str) -> Value {
        let t = std::fs::read_to_string(self.session_file(sid))
            .unwrap_or_else(|e| panic!("no session record for {sid}: {e}"));
        serde_json::from_str(&t).unwrap()
    }

    /// Run gauge in `cwd` with HOME = this env's home.
    fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        stdin: &str,
        envs: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gauge"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("GAUGE_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(stdin.as_bytes());
        }
        let o = child.wait_with_output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }

    fn gauge(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.run(cwd, args, "", &[])
    }

    /// Run the `record` Stop hook for `sid` with `lines` as the transcript.
    fn record(&self, proj: &Path, sid: &str, lines: &[String]) -> (i32, String, String) {
        self.record_env(proj, sid, lines, &[])
    }

    fn record_env(
        &self,
        proj: &Path,
        sid: &str,
        lines: &[String],
        envs: &[(&str, &str)],
    ) -> (i32, String, String) {
        let tp = self.home.join(format!("transcript-{sid}.jsonl"));
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(&tp, body).unwrap();
        let payload = json!({
            "hook_event_name": "Stop",
            "session_id": sid,
            "cwd": proj.to_str().unwrap(),
            "transcript_path": tp.to_str().unwrap(),
        })
        .to_string();
        self.run(proj, &["record"], &payload, envs)
    }

    /// `session --json --session sid`, parsed.
    fn session_json(&self, cwd: &Path, sid: &str) -> Value {
        let (rc, out, err) = self.gauge(cwd, &["session", "--json", "--session", sid]);
        assert_eq!(rc, 0, "session --json failed: {err}");
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
    }
}

/// One assistant transcript line.
fn asst(ts: Option<&str>, model: &str, usage: Value, tools: &[&str]) -> String {
    let content: Vec<Value> = tools
        .iter()
        .map(|t| json!({"type": "tool_use", "name": t}))
        .collect();
    let mut v = json!({
        "type": "assistant",
        "message": {"role": "assistant", "model": model, "content": content, "usage": usage},
    });
    if let Some(ts) = ts {
        v["timestamp"] = json!(ts);
    }
    v.to_string()
}

fn input_only(ts: &str, model: &str, n: u64) -> String {
    asst(
        Some(ts),
        model,
        json!({"input_tokens": n, "output_tokens": 0}),
        &[],
    )
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn cost_of(v: &Value) -> f64 {
    v["cost_usd"]
        .as_f64()
        .unwrap_or_else(|| panic!("cost_usd not a number: {v}"))
}

const M: u64 = 1_000_000;

// ------------------------------------------------------------------- record

#[test]
fn claim_record_cumulative_single_file() {
    let e = Env::new();
    let p = e.proj("proj");
    let t = |i: u32| format!("2026-09-01T10:00:0{i}Z");
    let mut lines = vec![input_only(&t(1), "claude-opus-4-8", 100)];
    let (rc, _, err) = e.record(&p, "S1", &lines);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(e.record_json("S1")["turns"], 1);

    // The transcript grows; the hook runs again.
    lines.push(input_only(&t(2), "claude-opus-4-8", 200));
    lines.push(input_only(&t(3), "claude-opus-4-8", 400));
    let (rc, _, err) = e.record(&p, "S1", &lines);
    assert_eq!(rc, 0, "{err}");

    assert_eq!(e.session_file_count(), 1, "exactly one session file");
    let r = e.record_json("S1");
    assert_eq!(r["turns"], 3);
    assert_eq!(r["models"]["claude-opus-4-8"]["input"], 700);
    assert_eq!(r["last_ts"], t(3));
}

#[test]
fn claim_record_token_kinds_per_model() {
    let e = Env::new();
    let p = e.proj("proj");
    let lines = vec![
        asst(
            Some("2026-09-01T10:00:01Z"),
            "claude-opus-4-8",
            json!({"input_tokens": 1000, "output_tokens": 2000, "cache_read_input_tokens": 300,
                   "cache_creation": {"ephemeral_5m_input_tokens": 40, "ephemeral_1h_input_tokens": 50}}),
            &[],
        ),
        asst(
            Some("2026-09-01T10:00:02Z"),
            "claude-sonnet-4-5",
            json!({"input_tokens": 7, "output_tokens": 8, "cache_read_input_tokens": 9,
                   "cache_creation": {"ephemeral_5m_input_tokens": 11, "ephemeral_1h_input_tokens": 12}}),
            &[],
        ),
    ];
    let (rc, _, err) = e.record(&p, "S", &lines);
    assert_eq!(rc, 0, "{err}");
    let r = e.record_json("S");
    let o = &r["models"]["claude-opus-4-8"];
    assert_eq!(o["input"], 1000);
    assert_eq!(o["output"], 2000);
    assert_eq!(o["cache_read"], 300);
    assert_eq!(o["cache_write_5m"], 40);
    assert_eq!(o["cache_write_1h"], 50);
    let s = &r["models"]["claude-sonnet-4-5"];
    assert_eq!(s["input"], 7);
    assert_eq!(s["output"], 8);
    assert_eq!(s["cache_read"], 9);
    assert_eq!(s["cache_write_5m"], 11);
    assert_eq!(s["cache_write_1h"], 12);
    // and `session --json` shows the same per-model counts
    let sj = e.session_json(&p, "S");
    assert_eq!(sj["models"]["claude-opus-4-8"]["cache_write_1h"], 50);
    assert_eq!(sj["models"]["claude-sonnet-4-5"]["cache_write_5m"], 11);
}

#[test]
fn claim_record_turns_and_tool_counts() {
    let e = Env::new();
    let p = e.proj("proj");
    let u = json!({"input_tokens": 1, "output_tokens": 1});
    let lines = vec![
        asst(
            Some("2026-09-01T10:00:01Z"),
            "claude-opus-4-8",
            u.clone(),
            &["Bash", "Edit"],
        ),
        asst(
            Some("2026-09-01T10:00:02Z"),
            "claude-opus-4-8",
            u.clone(),
            &["Bash"],
        ),
        asst(
            Some("2026-09-01T10:00:03Z"),
            "claude-opus-4-8",
            u.clone(),
            &[],
        ),
        asst(
            Some("2026-09-01T10:00:04Z"),
            "claude-opus-4-8",
            u,
            &["Bash"],
        ),
    ];
    let (rc, _, err) = e.record(&p, "S", &lines);
    assert_eq!(rc, 0, "{err}");
    let r = e.record_json("S");
    assert_eq!(r["turns"], 4);
    assert_eq!(r["tools"]["Bash"], 3);
    assert_eq!(r["tools"]["Edit"], 1);
    assert_eq!(r["tools"].as_object().unwrap().len(), 2);
}

#[test]
fn claim_record_first_last_duration() {
    let e = Env::new();
    let p = e.proj("proj");
    let lines = vec![
        input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1),
        input_only("2026-09-01T10:00:30Z", "claude-opus-4-8", 1),
        input_only("2026-09-01T10:01:05Z", "claude-opus-4-8", 1),
    ];
    let (rc, _, err) = e.record(&p, "S", &lines);
    assert_eq!(rc, 0, "{err}");
    let r = e.record_json("S");
    assert_eq!(r["first_ts"], "2026-09-01T10:00:00Z");
    assert_eq!(r["last_ts"], "2026-09-01T10:01:05Z");
    let (rc, out, err) = e.gauge(&p, &["session", "--session", "S"]);
    assert_eq!(rc, 0, "{err}");
    let line = out
        .lines()
        .find(|l| l.starts_with("duration"))
        .unwrap_or_else(|| panic!("no duration line in: {out}"));
    assert_eq!(
        line.split_whitespace().collect::<Vec<_>>(),
        ["duration", "1m", "5s"]
    );
}

// ------------------------------------------------------------------- report

/// Text of one report section: from `header` up to the next blank line.
fn section<'a>(out: &'a str, header: &str) -> Vec<&'a str> {
    let mut lines = out.lines();
    for l in lines.by_ref() {
        if l.trim() == header {
            break;
        }
    }
    lines.take_while(|l| !l.trim().is_empty()).collect()
}

fn fields(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

#[test]
fn claim_report_rollup_project_model_day() {
    let e = Env::new();
    let alpha = e.proj("alpha");
    let beta = e.proj("beta");
    // alpha: opus on 09-01 ($5), sonnet on 09-02 ($3); beta: opus on 09-02 ($5)
    e.record(
        &alpha,
        "A",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    e.record(
        &alpha,
        "B",
        &[input_only("2026-09-02T10:00:00Z", "claude-sonnet-4-5", M)],
    );
    e.record(
        &beta,
        "C",
        &[input_only("2026-09-02T11:00:00Z", "claude-opus-4-8", M)],
    );
    assert_eq!(e.session_file_count(), 3);

    let (rc, out, err) = e.gauge(&e.home, &["report"]);
    assert_eq!(rc, 0, "{err}");

    let head = out.lines().next().unwrap();
    assert!(
        head.contains("3 セッション") && head.contains("3 turns"),
        "{head}"
    );
    let total = out.lines().nth(1).unwrap();
    assert!(total.contains("合計コスト $13.00"), "{total}");

    // by project
    let proj = section(&out, "プロジェクト別");
    let a = proj
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "alpha")
        .unwrap();
    assert_eq!(a, ["alpha", "$8.00", "2.00M", "2", "sess"]);
    let b = proj
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "beta")
        .unwrap();
    assert_eq!(b, ["beta", "$5.00", "1.00M", "1", "sess"]);
    assert_eq!(proj.len(), 2);

    // by model
    let models = section(&out, "モデル別");
    let o = models
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "claude-opus-4-8")
        .unwrap();
    assert_eq!(o[1], "$10.00");
    let s = models
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "claude-sonnet-4-5")
        .unwrap();
    assert_eq!(s[1], "$3.00");
    assert_eq!(models.len(), 2);

    // by day
    let days = section(&out, "日別 (直近14日)");
    let d1 = days
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "2026-09-01")
        .unwrap();
    assert_eq!(d1[1], "$5.00");
    let d2 = days
        .iter()
        .map(|l| fields(l))
        .find(|f| f[0] == "2026-09-02")
        .unwrap();
    assert_eq!(d2[1], "$8.00");
    assert_eq!(days.len(), 2);
}

#[test]
fn claim_report_project_filter() {
    let e = Env::new();
    let alpha = e.proj("alpha");
    let myrepo = e.proj("myrepo");
    e.record(
        &alpha,
        "A",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    e.record(
        &myrepo,
        "B",
        &[input_only("2026-09-01T10:00:00Z", "claude-sonnet-4-5", M)],
    );
    let (rc, out, err) = e.gauge(&e.home, &["report", "--project", "myrepo"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        out.lines().next().unwrap().contains("1 セッション"),
        "{out}"
    );
    assert!(out.contains("合計コスト $3.00"), "{out}");
    assert!(out.contains("myrepo"), "{out}");
    assert!(!out.contains("alpha"), "alpha must be filtered out: {out}");
}

#[test]
fn claim_report_since_filter() {
    let e = Env::new();
    let p = e.proj("proj");
    e.record(
        &p,
        "before",
        &[input_only("2026-05-31T23:00:00Z", "claude-opus-4-8", M)],
    );
    e.record(
        &p,
        "on",
        &[input_only("2026-06-01T00:30:00Z", "claude-sonnet-4-5", M)],
    );
    e.record(
        &p,
        "after",
        &[input_only("2026-06-10T10:00:00Z", "claude-haiku-4-5", M)],
    );
    let (rc, out, err) = e.gauge(&e.home, &["report", "--since", "2026-06-01"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        out.lines().next().unwrap().contains("2 セッション"),
        "{out}"
    );
    // sonnet 3 + haiku 1; the earlier opus ($5) is excluded
    assert!(out.contains("合計コスト $4.00"), "{out}");
    assert!(!out.contains("2026-05-31"), "{out}");
    assert!(
        out.contains("2026-06-01") && out.contains("2026-06-10"),
        "{out}"
    );
}

// ------------------------------------------------------------------ session

#[test]
fn claim_session_most_recent() {
    let e = Env::new();
    let p = e.proj("proj");
    // Names chosen so the newest sorts alphabetically FIRST: recency, not name.
    e.record(
        &p,
        "zzz-old",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1)],
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    e.record(
        &p,
        "aaa-new",
        &[input_only("2026-08-01T10:00:00Z", "claude-opus-4-8", 1)],
    );
    let (rc, out, err) = e.gauge(&p, &["session"]);
    assert_eq!(rc, 0, "{err}");
    let first = out.lines().next().unwrap();
    assert_eq!(fields(first), ["session", "aaa-new"], "{out}");
}

#[test]
fn claim_session_json_shape() {
    let e = Env::new();
    let p = e.proj("proj");
    e.record(
        &p,
        "one",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    e.record(
        &p,
        "two",
        &[input_only("2026-09-02T10:00:00Z", "claude-sonnet-4-5", M)],
    );
    // `one` is NOT the most recent; asking for it by id must return it.
    let v = e.session_json(&p, "one");
    assert_eq!(v["session_id"], "one");
    assert!(approx(cost_of(&v), 5.0), "{v}");
    assert!(v["models"]["claude-opus-4-8"].is_object());
    assert!(v["agents"]["main"].is_object());
    assert!(approx(
        v["agents"]["main"]["cost_usd"].as_f64().unwrap(),
        5.0
    ));
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["agents", "cost_usd", "models", "session_id"]);
}

// ---------------------------------------------------------------- subagents

/// Lay out `~/.claude/projects/p/<sid>.jsonl` plus sub-agent transcripts.
fn subagent_fixture(e: &Env, sid: &str, agents: &[(&str, Vec<String>)]) {
    let proj = e.home.join(".claude/projects/p");
    let subdir = proj.join(sid).join("subagents");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(
        proj.join(format!("{sid}.jsonl")),
        input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1) + "\n",
    )
    .unwrap();
    for (id, lines) in agents {
        std::fs::write(
            subdir.join(format!("agent-{id}.jsonl")),
            lines.join("\n") + "\n",
        )
        .unwrap();
        std::fs::write(
            subdir.join(format!("agent-{id}.meta.json")),
            json!({"agentType": "worker", "description": format!("task {id}")}).to_string(),
        )
        .unwrap();
    }
}

fn subagents_json(e: &Env, sid: &str) -> Vec<Value> {
    let (rc, out, err) = e.gauge(&e.home, &["subagents", "--session", sid, "--json"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|x| panic!("{x}: {out}"));
    v.as_array()
        .unwrap_or_else(|| panic!("not a JSON array: {out}"))
        .clone()
}

#[test]
fn claim_subagents_live_per_task() {
    let e = Env::new();
    subagent_fixture(
        &e,
        "sid1",
        &[
            (
                "X",
                vec![input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
            ),
            (
                "Y",
                vec![input_only("2026-09-01T10:00:00Z", "claude-sonnet-4-5", M)],
            ),
        ],
    );
    // No `record` has run: the store does not exist.
    assert_eq!(e.session_file_count(), 0);
    let arr = subagents_json(&e, "sid1");
    assert_eq!(arr.len(), 2);
    let x = arr.iter().find(|a| a["agent_id"] == "X").unwrap();
    let y = arr.iter().find(|a| a["agent_id"] == "Y").unwrap();
    assert!(approx(cost_of(x), 5.0), "{x}");
    assert!(approx(cost_of(y), 3.0), "{y}");
    assert_eq!(x["description"], "task X");
    assert_eq!(x["turns"], 1);
    assert_eq!(
        e.session_file_count(),
        0,
        "reading must not need/create a record"
    );
}

#[test]
fn claim_subagents_json_array_null_times() {
    let e = Env::new();
    let no_ts = asst(
        None,
        "claude-opus-4-8",
        json!({"input_tokens": 10, "output_tokens": 1}),
        &[],
    );
    subagent_fixture(
        &e,
        "sid2",
        &[
            ("notime", vec![no_ts]),
            (
                "timed",
                vec![
                    input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1),
                    input_only("2026-09-01T10:05:30Z", "claude-opus-4-8", 1),
                ],
            ),
        ],
    );
    let arr = subagents_json(&e, "sid2");
    assert_eq!(arr.len(), 2);
    let nt = arr.iter().find(|a| a["agent_id"] == "notime").unwrap();
    for k in ["first_activity_at", "last_activity_at", "elapsed_secs"] {
        assert!(
            nt.get(k).is_some_and(Value::is_null),
            "{k} must be null, got {nt}"
        );
    }
    // control: the same fields are real values when timestamps exist
    let t = arr.iter().find(|a| a["agent_id"] == "timed").unwrap();
    assert_eq!(t["first_activity_at"], "2026-09-01T10:00:00Z");
    assert_eq!(t["elapsed_secs"], 330);
}

// ---------------------------------------------------------- disable / config

#[test]
fn claim_gauge_disable_env() {
    let e = Env::new();
    let p = e.proj("proj");
    let lines = [input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1)];
    let (rc, _, err) = e.record_env(&p, "off", &lines, &[("GAUGE_DISABLE", "1")]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        !e.session_file("off").exists(),
        "GAUGE_DISABLE=1 must write nothing"
    );
    assert_eq!(e.session_file_count(), 0);
    // control: same payload without the env writes a record
    let (rc, _, err) = e.record(&p, "on", &lines);
    assert_eq!(rc, 0, "{err}");
    assert!(e.session_file("on").exists());
}

#[test]
fn claim_records_without_config() {
    let e = Env::new();
    let p = e.proj("proj");
    assert!(!p.join("gauge.toml").exists());
    assert!(!e.home.join(".gauge/config.toml").exists());
    let (rc, _, err) = e.record(
        &p,
        "S",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 5)],
    );
    assert_eq!(rc, 0, "{err}");
    assert!(e.session_file("S").exists());
    assert_eq!(e.record_json("S")["models"]["claude-opus-4-8"]["input"], 5);
}

// ------------------------------------------------------------ hooks/install

#[test]
fn claim_hooks_json_stop_record() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("hooks/hooks.json")).unwrap())
            .unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    assert_eq!(
        hooks.keys().collect::<Vec<_>>(),
        ["Stop"],
        "only a Stop hook"
    );
    let stop = hooks["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1);
    let cmds = stop[0]["hooks"].as_array().unwrap();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["type"], "command");
    assert_eq!(cmds[0]["command"], "${CLAUDE_PLUGIN_ROOT}/bin/gauge record");
    assert!(
        root.join("bin/gauge").is_file(),
        "bundled launcher bin/gauge must exist"
    );
}

fn foreign_settings() -> String {
    serde_json::to_string_pretty(&json!({
        "theme": "dark",
        "hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "beacon notify"}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": "other check"}]}]
        }
    }))
    .unwrap()
}

fn settings_path(e: &Env) -> PathBuf {
    e.home.join(".claude/settings.json")
}

fn write_settings(e: &Env, text: &str) {
    std::fs::create_dir_all(e.home.join(".claude")).unwrap();
    std::fs::write(settings_path(e), text).unwrap();
}

fn stop_commands(e: &Env) -> Vec<String> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(settings_path(e)).unwrap()).unwrap();
    v["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap().iter())
        .map(|h| h["command"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn claim_install_merges_stop_hook() {
    let e = Env::new();
    write_settings(&e, &foreign_settings());
    let (rc, _, err) = e.gauge(&e.home, &["install"]);
    assert_eq!(rc, 0, "{err}");
    let cmds = stop_commands(&e);
    assert!(
        cmds.contains(&"beacon notify".to_string()),
        "foreign hook preserved: {cmds:?}"
    );
    let ours: Vec<_> = cmds
        .iter()
        .filter(|c| c.ends_with("gauge record"))
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "exactly one gauge record Stop hook: {cmds:?}"
    );
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(settings_path(&e)).unwrap()).unwrap();
    assert_eq!(v["theme"], "dark");
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "other check"
    );
}

#[test]
fn claim_uninstall_removes_hook() {
    let e = Env::new();
    write_settings(&e, &foreign_settings());
    assert_eq!(e.gauge(&e.home, &["install"]).0, 0);
    assert!(stop_commands(&e).iter().any(|c| c.contains("gauge")));
    let (rc, _, err) = e.gauge(&e.home, &["uninstall"]);
    assert_eq!(rc, 0, "{err}");
    let cmds = stop_commands(&e);
    assert_eq!(cmds, ["beacon notify"], "gauge gone, foreign kept");
}

fn dir_listing(e: &Env) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(e.home.join(".claude"))
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn claim_install_dry_run() {
    let e = Env::new();
    write_settings(&e, &foreign_settings());
    let before = std::fs::read(settings_path(&e)).unwrap();
    let listing = dir_listing(&e);

    let (rc, out, err) = e.gauge(&e.home, &["install", "--dry-run"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        out.contains("dry run") && out.contains("gauge"),
        "shows would-be settings: {out}"
    );
    assert_eq!(
        std::fs::read(settings_path(&e)).unwrap(),
        before,
        "install --dry-run wrote"
    );
    assert_eq!(dir_listing(&e), listing, "install --dry-run created files");

    // uninstall --dry-run on a settings file that DOES contain the hook
    assert_eq!(e.gauge(&e.home, &["install"]).0, 0);
    let installed = std::fs::read(settings_path(&e)).unwrap();
    let listing = dir_listing(&e);
    let (rc, out, err) = e.gauge(&e.home, &["uninstall", "--dry-run"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("dry run"), "{out}");
    assert_eq!(
        std::fs::read(settings_path(&e)).unwrap(),
        installed,
        "uninstall --dry-run wrote"
    );
    assert_eq!(
        dir_listing(&e),
        listing,
        "uninstall --dry-run created files"
    );
}

// ------------------------------------------------------------- status / init

#[test]
fn claim_status_fields() {
    let e = Env::new();
    let p = e.proj("proj");
    std::fs::write(p.join("gauge.toml"), "enabled = true\n").unwrap();
    e.record(
        &p,
        "A",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1)],
    );
    e.record(
        &p,
        "B",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", 1)],
    );
    let (rc, out, err) = e.gauge(&p, &["status"]);
    assert_eq!(rc, 0, "{err}");
    let get = |key: &str| -> String {
        out.lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("no `{key}` line in: {out}"))
            .trim()
            .to_string()
    };
    assert_eq!(get("config:"), p.join("gauge.toml").display().to_string());
    assert_eq!(
        get("state_dir:"),
        e.home.join(".gauge/store").display().to_string()
    );
    assert_eq!(get("sessions:"), "2");
}

#[test]
fn claim_init_force() {
    let e = Env::new();
    let p = e.proj("proj");
    let target = p.join("gauge.toml");
    let (rc, _, err) = e.gauge(&p, &["init"]);
    assert_eq!(rc, 0, "{err}");
    let starter = std::fs::read_to_string(&target).unwrap();
    assert!(starter.contains("enabled = true"), "{starter}");

    std::fs::write(&target, "# mine\n").unwrap();
    let (rc, _, err) = e.gauge(&p, &["init"]);
    assert_eq!(rc, 1, "second init without --force must fail");
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# mine\n",
        "must not overwrite"
    );

    let (rc, _, err) = e.gauge(&p, &["init", "--force"]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        starter,
        "--force overwrites"
    );
}

// ------------------------------------------------------------------ pricing

#[test]
fn claim_builtin_rates() {
    let e = Env::new();
    let p = e.proj("proj");
    let cases = [
        ("claude-opus-4-8", 5.0, 25.0),
        ("claude-sonnet-4-5", 3.0, 15.0),
        ("claude-haiku-4-5", 1.0, 5.0),
        ("claude-fable-1", 10.0, 50.0),
        ("claude-mythos-1", 10.0, 50.0),
    ];
    for (model, rin, rout) in cases {
        let sid_in = format!("in-{model}");
        let sid_out = format!("out-{model}");
        e.record(&p, &sid_in, &[input_only("2026-09-01T10:00:00Z", model, M)]);
        e.record(
            &p,
            &sid_out,
            &[asst(
                Some("2026-09-01T10:00:00Z"),
                model,
                json!({"input_tokens": 0, "output_tokens": M}),
                &[],
            )],
        );
        let ci = cost_of(&e.session_json(&p, &sid_in));
        let co = cost_of(&e.session_json(&p, &sid_out));
        assert!(approx(ci, rin), "{model} input: want {rin}, got {ci}");
        assert!(approx(co, rout), "{model} output: want {rout}, got {co}");
    }
}

#[test]
fn claim_cache_multipliers() {
    let e = Env::new();
    let p = e.proj("proj");
    let w5 = asst(
        Some("2026-09-01T10:00:00Z"),
        "claude-opus-4-8",
        json!({"input_tokens": 0, "output_tokens": 0,
               "cache_creation": {"ephemeral_5m_input_tokens": M, "ephemeral_1h_input_tokens": 0}}),
        &[],
    );
    let w1 = asst(
        Some("2026-09-01T10:00:00Z"),
        "claude-opus-4-8",
        json!({"input_tokens": 0, "output_tokens": 0,
               "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": M}}),
        &[],
    );
    let rd = asst(
        Some("2026-09-01T10:00:00Z"),
        "claude-opus-4-8",
        json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": M}),
        &[],
    );
    e.record(&p, "w5", &[w5]);
    e.record(&p, "w1", &[w1]);
    e.record(&p, "rd", &[rd]);
    let c5 = cost_of(&e.session_json(&p, "w5"));
    let c1 = cost_of(&e.session_json(&p, "w1"));
    let cr = cost_of(&e.session_json(&p, "rd"));
    assert!(approx(c5, 6.25), "5m write: {c5}");
    assert!(approx(c1, 10.0), "1h write: {c1}");
    assert!(approx(cr, 0.5), "read: {cr}");
}

#[test]
fn claim_unpriced_json_null_undetermined() {
    let e = Env::new();
    let p = e.proj("proj");
    e.record(
        &p,
        "S",
        &[input_only("2026-09-01T10:00:00Z", "claude-newfamily-1", M)],
    );
    let v = e.session_json(&p, "S");
    assert!(v["cost_usd"].is_null(), "cost_usd must be JSON null: {v}");
    let why = v
        .get("cost_undetermined")
        .unwrap_or_else(|| panic!("cost_undetermined missing: {v}"));
    assert!(
        why.as_str()
            .is_some_and(|s| s.contains("claude-newfamily-1")),
        "{why}"
    );
    // control: a priced session has a number and no cost_undetermined
    e.record(
        &p,
        "P",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    let ok = e.session_json(&p, "P");
    assert!(approx(cost_of(&ok), 5.0));
    assert!(ok.get("cost_undetermined").is_none());
}

fn toml_rate(rows: &[(&str, f64, f64)]) -> String {
    rows.iter()
        .map(|(pat, i, o)| {
            format!("[[pricing]]\npattern = \"{pat}\"\ninput = {i:?}\noutput = {o:?}\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn claim_pricing_pattern_first_match() {
    let model = "claude-opus-4-8";
    // Both rows substring-match; the first listed wins, in either order.
    for (rows, want) in [
        (vec![("opus", 1.0, 2.0), ("claude", 100.0, 200.0)], 1.0),
        (vec![("claude", 100.0, 200.0), ("opus", 1.0, 2.0)], 100.0),
    ] {
        let e = Env::new();
        let p = e.proj("proj");
        std::fs::write(p.join("gauge.toml"), toml_rate(&rows)).unwrap();
        e.record(&p, "S", &[input_only("2026-09-01T10:00:00Z", model, M)]);
        let c = cost_of(&e.session_json(&p, "S"));
        assert!(approx(c, want), "rows {rows:?}: want {want}, got {c}");
    }
}

#[test]
fn claim_report_reprices_history() {
    let e = Env::new();
    let p = e.proj("proj");
    e.record(
        &p,
        "S",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    let stored = std::fs::read(e.session_file("S")).unwrap();
    let (rc, out, err) = e.gauge(&p, &["report"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("合計コスト $5.00"), "{out}");

    std::fs::write(p.join("gauge.toml"), toml_rate(&[("opus", 1.0, 2.0)])).unwrap();
    let (rc, out, err) = e.gauge(&p, &["report"]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        out.contains("合計コスト $1.00"),
        "history not re-priced: {out}"
    );
    assert_eq!(
        std::fs::read(e.session_file("S")).unwrap(),
        stored,
        "stored tokens untouched"
    );
}

#[test]
fn claim_config_lookup_order() {
    let model = "claude-opus-4-8";
    let line = || [input_only("2026-09-01T10:00:00Z", model, M)];
    let home_cfg = |e: &Env, text: &str| {
        std::fs::create_dir_all(e.home.join(".gauge")).unwrap();
        std::fs::write(e.home.join(".gauge/config.toml"), text).unwrap();
    };

    // both present, different rates -> project wins
    let e = Env::new();
    let p = e.proj("proj");
    std::fs::write(p.join("gauge.toml"), toml_rate(&[("opus", 1.0, 1.0)])).unwrap();
    home_cfg(&e, &toml_rate(&[("opus", 2.0, 2.0)]));
    e.record(&p, "S", &line());
    assert!(
        approx(cost_of(&e.session_json(&p, "S")), 1.0),
        "project must win"
    );

    // only home -> home rate
    let e = Env::new();
    let p = e.proj("proj");
    home_cfg(&e, &toml_rate(&[("opus", 2.0, 2.0)]));
    e.record(&p, "S", &line());
    assert!(approx(cost_of(&e.session_json(&p, "S")), 2.0), "home rate");

    // neither -> built-in
    let e = Env::new();
    let p = e.proj("proj");
    e.record(&p, "S", &line());
    assert!(approx(cost_of(&e.session_json(&p, "S")), 5.0), "built-in");

    // first file that exists wins: layers are not merged. The project file
    // has no opus row, so the home file's opus row must NOT apply.
    let e = Env::new();
    let p = e.proj("proj");
    std::fs::write(p.join("gauge.toml"), toml_rate(&[("sonnet", 9.0, 9.0)])).unwrap();
    home_cfg(&e, &toml_rate(&[("opus", 2.0, 2.0)]));
    e.record(&p, "S", &line());
    assert!(
        approx(cost_of(&e.session_json(&p, "S")), 5.0),
        "layers must not merge"
    );
}

#[test]
fn claim_bad_config_refuses() {
    let defects: &[(&str, &str)] = &[
        (
            "unknown table",
            "[[pricng]]\npattern = \"opus\"\ninput = 1.0\noutput = 1.0\n",
        ),
        ("unknown key", "enabeld = true\n"),
        (
            "unknown row key",
            "[[pricing]]\npattern = \"opus\"\ninput = 1.0\noutput = 1.0\ninputt = 1.0\n",
        ),
        (
            "missing output",
            "[[pricing]]\npattern = \"opus\"\ninput = 1.0\n",
        ),
        (
            "missing input",
            "[[pricing]]\npattern = \"opus\"\noutput = 1.0\n",
        ),
        (
            "missing pattern",
            "[[pricing]]\ninput = 1.0\noutput = 1.0\n",
        ),
        (
            "negative rate",
            "[[pricing]]\npattern = \"opus\"\ninput = -1.0\noutput = 1.0\n",
        ),
        (
            "nan rate",
            "[[pricing]]\npattern = \"opus\"\ninput = nan\noutput = 1.0\n",
        ),
        (
            "inf rate",
            "[[pricing]]\npattern = \"opus\"\ninput = 1.0\noutput = inf\n",
        ),
        ("unparseable", "this is = = not toml\n"),
    ];
    for (name, text) in defects {
        let e = Env::new();
        let p = e.proj("proj");
        // Establish a store while the config is still valid, and prove every
        // read command exits 0 then, so a later exit 1 is caused by the defect.
        e.record(
            &p,
            "pre",
            &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
        );
        for cmd in [
            vec!["report"],
            vec!["session"],
            vec!["subagents"],
            vec!["status"],
        ] {
            let (rc, _, err) = e.gauge(&p, &cmd);
            assert_eq!(
                rc, 0,
                "[{name}] control {cmd:?} should pass with no config: {err}"
            );
        }

        std::fs::write(p.join("gauge.toml"), text).unwrap();
        for cmd in [
            vec!["report"],
            vec!["session"],
            vec!["subagents"],
            vec!["status"],
        ] {
            let (rc, out, err) = e.gauge(&p, &cmd);
            assert_eq!(
                rc, 1,
                "[{name}] {cmd:?} must exit exactly 1 (out={out:?} err={err:?})"
            );
            assert!(
                err.contains("gauge.toml"),
                "[{name}] {cmd:?} stderr must name the file: {err}"
            );
        }

        let before = e.session_file_count();
        let (rc, _, err) = e.record(
            &p,
            "post",
            &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
        );
        // The exit code is deliberately NOT asserted: the README makes no claim
        // about it, and freezing "config error => exit 0" would pin a possible
        // fail-open as spec (lead-brief ruling). Reported as an open finding.
        let _ = rc;
        assert!(
            !e.session_file("post").exists(),
            "[{name}] record must write nothing"
        );
        assert_eq!(e.session_file_count(), before, "[{name}]");
        assert!(
            err.contains("not recording") && err.contains("gauge.toml"),
            "[{name}] record must say why on stderr: {err:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn claim_unreadable_config_refuses() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let p = e.proj("proj");
    e.record(
        &p,
        "pre",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    let cfg = p.join("gauge.toml");
    std::fs::write(&cfg, "enabled = true\n").unwrap();
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Apparatus check: chmod must actually make the file unreadable (it does
    // nothing for root); fail loudly instead of passing vacuously.
    assert!(
        std::fs::read_to_string(&cfg).is_err(),
        "apparatus: gauge.toml is still readable after chmod 000 (running as root?)"
    );
    let mut results = Vec::new();
    for cmd in [
        vec!["report"],
        vec!["session"],
        vec!["subagents"],
        vec!["status"],
    ] {
        results.push((cmd.clone(), e.gauge(&p, &cmd)));
    }
    let before = e.session_file_count();
    let (rrc, _, rerr) = e.record(
        &p,
        "post",
        &[input_only("2026-09-01T10:00:00Z", "claude-opus-4-8", M)],
    );
    // restore so the tempdir can be cleaned up
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o644)).unwrap();
    for (cmd, (rc, out, err)) in results {
        assert_eq!(rc, 1, "{cmd:?} must exit 1 (out={out:?} err={err:?})");
        assert!(
            err.contains("gauge.toml") && err.contains("could not be read"),
            "{cmd:?} stderr must name the file and the reason: {err:?}"
        );
    }
    // Exit code deliberately not asserted (see claim_bad_config_refuses).
    let _ = rrc;
    assert!(
        !e.session_file("post").exists(),
        "record must write nothing"
    );
    assert_eq!(e.session_file_count(), before);
    assert!(
        rerr.contains("not recording") && rerr.contains("gauge.toml"),
        "record must say why on stderr: {rerr:?}"
    );
}

// ----------------------------------------------------------------- packaging

#[test]
fn claim_bundled_bins_gitignored() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for bin in ["bin/gauge-darwin-arm64", "bin/gauge-linux-x86_64"] {
        let st = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(["check-ignore", "-q", bin])
            .current_dir(root)
            .status()
            .unwrap();
        // 0 = ignored, 1 = not ignored, anything else = git failed
        assert_eq!(
            st.code(),
            Some(0),
            "{bin} must be git-ignored (never committed)"
        );
    }
    // The tracked part of bin/ is only the hand-written launcher.
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["ls-files", "bin"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "bin/gauge");
}

#[test]
fn claim_target_dir_is_workspace_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let target = PathBuf::from(v["target_directory"].as_str().unwrap());
    let ws = PathBuf::from(v["workspace_root"].as_str().unwrap());
    assert_eq!(
        target,
        ws.join("target"),
        "cargo builds into the workspace-root target/"
    );
    assert_ne!(target, root.join("target"));
    // The Makefile copies from a path relative to crates/gauge ...
    let mk = std::fs::read_to_string(root.join("Makefile")).unwrap();
    assert!(mk.contains("cp target/release/gauge bin/gauge-darwin-"));
    // ... which therefore is not where the release binary lands.
    assert!(
        !root.join("target/release/gauge").exists(),
        "crates/gauge/target/release/gauge exists, so the Makefile path would resolve"
    );
}

// budgetguard_unmeasured: no test here. The behaviour (a fresh gauge record
// holding an unpriced model makes budgetguard's session cost Undetermined ->
// Block "cost-undetermined") is covered by the existing unit test
// crates/budgetguard/src/gate.rs::tests::
// fresh_gauge_record_with_unpriced_model_is_undetermined_and_blocks.
