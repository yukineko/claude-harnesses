//! Claim-reproduction tests for the prose in `crates/autoflow/README.md` /
//! `README.ja.md`. One `claim_<id>` test per claim; each drives the real
//! `autoflow` binary (temp `HOME`, temp repo, stub `backlog` on an isolated
//! `PATH`) or reads the packaged files, and asserts the specific outcome the
//! prose states.
//!
//! `CLAUDE_CODE_SESSION_ID` is removed from the child environment on purpose:
//! `harness_core::repeat` keys its "second identical block is waived" ledger on
//! it, and an unset id is `Undetermined` there, i.e. the block stands. That keeps
//! every Stop in a multi-Stop scenario at full strength.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;
use serde_json::{json, Value};

const RUN_ID: &str = "run-20260101-000000-1";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    run_dir: PathBuf,
    bin_dir: PathBuf,
    session: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let run_dir = home.join(".condukt").join("state").join(project_key(&repo));
        std::fs::create_dir_all(&run_dir).unwrap();
        let bin_dir = tmp.path().join("stub-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        Env {
            _tmp: tmp,
            home,
            repo,
            run_dir,
            bin_dir,
            session: format!("sess-{tag}"),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.home
            .join(".autoflow")
            .join("state")
            .join(format!("{}.json", self.session))
    }

    fn write_state_raw(&self, body: &str) {
        let p = self.state_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    fn write_phase(&self, phase: &str) {
        self.write_state_raw(&format!(r#"{{"phase":"{phase}"}}"#));
    }

    /// Seed a `continuing` session with a given previous pending size / streak.
    fn write_continuing(&self, prev: u32, streak: u32) {
        self.write_state_raw(&format!(
            r#"{{"phase":"continuing","condukt_prev_pending":{prev},"condukt_no_progress_streak":{streak}}}"#
        ));
    }

    fn phase(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.state_path()).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        Some(v.get("phase")?.as_str()?.to_string())
    }

    fn run_path(&self) -> PathBuf {
        self.run_dir.join(format!("{RUN_ID}.json"))
    }

    fn write_run_state(&self, tasks: Value) {
        std::fs::write(
            self.run_path(),
            json!({"run_id": RUN_ID, "goal": "g", "tasks": tasks}).to_string(),
        )
        .unwrap();
    }

    /// `n` tasks, all `pending`.
    fn pending(&self, n: usize) {
        let tasks: Vec<Value> = (1..=n)
            .map(|i| json!({"id": format!("t{i}"), "status": "pending"}))
            .collect();
        self.write_run_state(Value::Array(tasks));
    }

    fn task_statuses(&self) -> Vec<String> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(self.run_path()).unwrap()).unwrap();
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["status"].as_str().unwrap().to_string())
            .collect()
    }

    fn write_metrics(&self, turns: u64, tool_events: u64) {
        let dir = self.home.join(".session-insights").join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", self.session)),
            json!({"turns": turns, "tool_events": tool_events}).to_string(),
        )
        .unwrap();
    }

    fn write_config(&self, body: &str) {
        let dir = self.home.join(".autoflow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    /// Stub `backlog`: `lock status --project <p>` prints `status_for_project`
    /// (a JSON line) iff `<p>` equals `live_project`, `none` otherwise.
    fn stub_backlog(&self, live_project: &str, status_json: &str) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = lock ] && [ \"$2\" = status ]; then\n\
               if [ \"$4\" = '{live_project}' ]; then echo '{status_json}'; else echo none; fi\n\
             fi\n\
             exit 0\n"
        );
        let path = self.bin_dir.join("backlog");
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The `--project` value autoflow passes to backlog for this repo.
    fn project_arg(&self) -> String {
        self.repo
            .canonicalize()
            .unwrap_or_else(|_| self.repo.clone())
            .to_string_lossy()
            .into_owned()
    }

    fn live_status_json(&self, session: &str) -> String {
        json!({
            "session_id": session,
            "pid": 1,
            "project": self.project_arg(),
            "acquired_at": 0,
            "heartbeat_at": 0
        })
        .to_string()
    }

    fn run(&self, sub: &str, payload: Value, extra_env: &[(&str, &str)]) -> Out {
        let path = format!("{}:/usr/bin:/bin", self.bin_dir.display());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_autoflow"));
        cmd.arg(sub)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("AUTOFLOW_DISABLE")
            .env_remove("HARNESS_AUTONOMOUS")
            .env_remove("CONDUKT_AUTONOMOUS")
            .env_remove("HARNESS_AUTONOMY_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("autoflow spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("autoflow runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn stop_full(&self, transcript: &str, extra_env: &[(&str, &str)]) -> Out {
        self.run(
            "stop",
            json!({
                "hook_event_name": "Stop",
                "session_id": self.session,
                "cwd": self.repo,
                "transcript_path": transcript
            }),
            extra_env,
        )
    }

    fn stop(&self) -> Out {
        self.stop_full("", &[])
    }

    fn pre_compact(&self) -> Out {
        self.run(
            "pre-compact",
            json!({"hook_event_name": "PreCompact", "session_id": self.session, "cwd": self.repo}),
            &[],
        )
    }

    fn prompt_submit(&self) -> Out {
        self.run(
            "prompt-submit",
            json!({"hook_event_name": "UserPromptSubmit", "session_id": self.session}),
            &[],
        )
    }
}

/// `Some(reason)` iff stdout is a JSON `{"decision":"block", "reason": ...}`.
fn block_reason(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("decision")?.as_str()? != "block" {
        return None;
    }
    Some(v.get("reason")?.as_str()?.to_string())
}

fn expect_block(o: &Out, what: &str) -> String {
    assert_eq!(o.code, 0, "{what}: exit code; stderr: {}", o.stderr);
    block_reason(&o.stdout).unwrap_or_else(|| {
        panic!(
            "{what}: expected a block decision, got stdout {:?}",
            o.stdout
        )
    })
}

fn expect_silent(o: &Out, what: &str) {
    assert_eq!(o.code, 0, "{what}: exit code; stderr: {}", o.stderr);
    assert!(
        o.stdout.trim().is_empty(),
        "{what}: expected empty stdout, got {:?}",
        o.stdout
    );
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                walk(&p, out);
            } else if ft.is_file() {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, &mut m);
    m
}

fn user_text_line(text: &str) -> String {
    json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":text}]}})
        .to_string()
}

fn write_transcript(env: &Env, name: &str, lines: &[String]) -> String {
    let p = env.home.join(name);
    std::fs::write(&p, lines.join("\n")).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn claim_record_then_condukt_sequence() {
    let env = Env::new("seq");
    env.write_metrics(2, 3);

    // Idle + enough work -> /record, once.
    let r = expect_block(&env.stop(), "stop 1 (idle, metrics met)");
    assert!(r.contains("/session-insights:record"), "got: {r}");
    assert_eq!(env.phase().as_deref(), Some("record_requested"));

    // Pending condukt tasks -> /condukt, NOT /record again.
    env.pending(3);
    let r = expect_block(&env.stop(), "stop 2 (3 pending)");
    assert!(r.contains("/condukt"), "got: {r}");
    assert!(!r.contains("session-insights:record"), "got: {r}");
    assert_eq!(env.phase().as_deref(), Some("continuing"));

    // Pending set shrinks -> keeps looping /condukt.
    env.pending(2);
    let r = expect_block(&env.stop(), "stop 3 (2 pending)");
    assert!(r.contains("/condukt") && r.contains("2 件"), "got: {r}");
    assert!(!r.contains("session-insights:record"), "got: {r}");

    // Observed empty -> silent, and latched done.
    env.write_run_state(json!([{"id":"t1","status":"verified"}]));
    expect_silent(&env.stop(), "stop 4 (pending set observed empty)");
    assert_eq!(env.phase().as_deref(), Some("done"));
}

#[test]
fn claim_stop_side_effects_only() {
    let env = Env::new("sidefx");
    env.write_phase("record_requested");
    env.pending(2);

    let before = snapshot(&env.home);
    let o = env.stop();
    let after = snapshot(&env.home);

    let r = expect_block(&o, "routine continuation");
    assert!(!r.trim().is_empty());

    let mut touched: Vec<PathBuf> = Vec::new();
    for (p, bytes) in &after {
        if before.get(p) != Some(bytes) {
            touched.push(p.clone());
        }
    }
    for p in before.keys() {
        assert!(
            after.contains_key(p),
            "file deleted by Stop: {}",
            p.display()
        );
    }
    touched.sort();

    let state = env.state_path();
    let run = env.run_path();
    let (mut n_state, mut n_run, mut n_viol) = (0, 0, 0);
    for p in &touched {
        if *p == state {
            n_state += 1;
        } else if *p == run {
            n_run += 1;
        } else if p.file_name().is_some_and(|n| n == "violations.jsonl")
            && p.starts_with(env.home.join(".overwatch"))
        {
            n_viol += 1;
        } else {
            panic!("unexpected side effect: {} (all: {touched:?})", p.display());
        }
    }
    assert_eq!(
        (n_state, n_run, n_viol),
        (1, 1, 1),
        "expected exactly state + run-state + overwatch ledger, got {touched:?}"
    );
    assert_eq!(env.task_statuses(), vec!["running", "running"]);

    let ledger = touched
        .iter()
        .find(|p| p.file_name().is_some_and(|n| n == "violations.jsonl"))
        .unwrap();
    let text = std::fs::read_to_string(ledger).unwrap();
    assert_eq!(
        text.lines().count(),
        1,
        "one violation record per block: {text}"
    );
}

#[test]
fn claim_silent_when_disabled_or_no_run_file() {
    // (a) gate disabled by config, with pending work.
    let env = Env::new("disabled");
    env.write_phase("record_requested");
    env.pending(2);
    env.write_config("enabled = false\n");
    expect_silent(&env.stop(), "enabled=false");

    // control: same world, config absent -> blocks.
    let ctl = Env::new("disabled-ctl");
    ctl.write_phase("record_requested");
    ctl.pending(2);
    expect_block(&ctl.stop(), "control");

    // (b) no condukt run file at all, RecordRequested phase.
    let env = Env::new("norun");
    env.write_phase("record_requested");
    assert!(std::fs::read_dir(&env.run_dir).unwrap().next().is_none());
    expect_silent(&env.stop(), "no condukt run file");
}

#[test]
fn claim_idle_record_threshold() {
    // met
    let env = Env::new("thr-met");
    env.write_metrics(2, 3);
    let r = expect_block(&env.stop(), "2 turns / 3 tool events");
    assert!(r.contains("/session-insights:record"), "got: {r}");

    // turns below
    let env = Env::new("thr-turns");
    env.write_metrics(1, 3);
    expect_silent(&env.stop(), "1 turn");

    // tool events below
    let env = Env::new("thr-tools");
    env.write_metrics(2, 2);
    expect_silent(&env.stop(), "2 tool events");

    // raising min_turns in config suppresses
    let env = Env::new("thr-cfg-turns");
    env.write_metrics(2, 3);
    env.write_config("min_turns = 5\n");
    expect_silent(&env.stop(), "min_turns=5");

    // raising min_tool_events in config suppresses
    let env = Env::new("thr-cfg-tools");
    env.write_metrics(2, 3);
    env.write_config("min_tool_events = 10\n");
    expect_silent(&env.stop(), "min_tool_events=10");

    // lowering min_turns lets 1 turn through (the knob is the threshold)
    let env = Env::new("thr-cfg-low");
    env.write_metrics(1, 3);
    env.write_config("min_turns = 1\n");
    let r = expect_block(&env.stop(), "min_turns=1");
    assert!(r.contains("/session-insights:record"), "got: {r}");
}

#[test]
fn claim_config_defaults() {
    // gate on by default; record prompt at exactly 2 turns / 3 tool events
    // (the boundary pair is pinned in claim_idle_record_threshold, with no
    // config file in all of the non-config cases there).
    let env = Env::new("def-record");
    env.write_metrics(2, 3);
    expect_block(&env.stop(), "defaults: record");
    let env = Env::new("def-below-turns");
    env.write_metrics(1, 3);
    expect_silent(&env.stop(), "defaults: 1 turn is below min_turns");
    let env = Env::new("def-below-tools");
    env.write_metrics(2, 2);
    expect_silent(
        &env.stop(),
        "defaults: 2 tool events is below min_tool_events",
    );

    // stuck_threshold = 3: the 3rd consecutive no-progress Stop escalates.
    let env = Env::new("def-stuck");
    env.write_continuing(2, 0);
    for n in 1..=2 {
        env.pending(2);
        let r = expect_block(&env.stop(), &format!("no-progress stop {n}"));
        assert!(
            !r.contains("進捗が止まっています"),
            "stop {n} escalated: {r}"
        );
        assert!(r.contains("/condukt"), "got: {r}");
    }
    env.pending(2);
    let r = expect_block(&env.stop(), "no-progress stop 3");
    assert!(r.contains("進捗が止まっています"), "got: {r}");
    assert!(r.contains("3 回連続"), "got: {r}");

    // resume_flow_on_compact = true: PreCompact with the lock held drops a
    // marker, which prompt-submit consumes.
    let env = Env::new("def-precompact");
    env.stub_backlog(&env.project_arg(), &env.live_status_json(&env.session));
    expect_silent(&env.pre_compact(), "pre-compact");
    let o = env.prompt_submit();
    assert_eq!(o.code, 0);
    assert!(!o.stdout.trim().is_empty(), "marker was not written");
}

#[test]
fn claim_undetermined_reason_phase_untouched() {
    for phase in ["continuing", "record_requested"] {
        let env = Env::new(&format!("undet-{phase}"));
        env.write_phase(phase);
        std::fs::write(env.run_path(), "{ this is not json ").unwrap();
        let before = std::fs::read(env.state_path()).unwrap();

        let r = expect_block(&env.stop(), phase);
        assert!(r.contains("判定不能"), "{phase}: got: {r}");
        assert_eq!(env.phase().as_deref(), Some(phase));
        assert_eq!(
            std::fs::read(env.state_path()).unwrap(),
            before,
            "{phase}: state file must be left untouched"
        );
    }
}

#[test]
fn claim_escalation_autonomy_wording() {
    let run = |auto: &str| {
        let env = Env::new(&format!("auto-{auto}"));
        env.write_continuing(2, 2);
        env.pending(2);
        let o = env.stop_full("", &[("HARNESS_AUTONOMOUS", auto)]);
        expect_block(&o, &format!("escalation, HARNESS_AUTONOMOUS={auto}"))
    };
    let on = run("1");
    let off = run("0");
    assert!(on.contains("自律継続中"), "autonomous: {on}");
    assert!(!on.contains("ユーザーに確認"), "autonomous: {on}");
    assert!(off.contains("ユーザーに確認"), "non-autonomous: {off}");
    assert!(!off.contains("自律継続中"), "non-autonomous: {off}");
    assert_ne!(on, off);
}

#[test]
fn claim_explicit_stop_allows_this_stop() {
    // typed message
    let env = Env::new("stop-typed");
    env.write_phase("record_requested");
    env.pending(2);
    let t = write_transcript(
        &env,
        "t.jsonl",
        &[user_text_line("作業を続けて"), user_text_line("止めて")],
    );
    let before = std::fs::read(env.state_path()).unwrap();
    expect_silent(&env.stop_full(&t, &[]), "typed 止めて");
    assert_eq!(std::fs::read(env.state_path()).unwrap(), before);
    assert_eq!(env.task_statuses(), vec!["pending", "pending"]);

    // control: ordinary last message blocks
    let ctl = Env::new("stop-ctl");
    ctl.write_phase("record_requested");
    ctl.pending(2);
    let t = write_transcript(&ctl, "t.jsonl", &[user_text_line("続けて")]);
    expect_block(&ctl.stop_full(&t, &[]), "ordinary message");

    // AskUserQuestion answer
    let env = Env::new("stop-ask");
    env.write_phase("record_requested");
    env.pending(2);
    let ask = json!({"type":"assistant","message":{"role":"assistant","content":[
        {"type":"tool_use","id":"toolu_ask","name":"AskUserQuestion","input":{"questions":[]}}]}})
    .to_string();
    let ans = json!({"type":"user","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":"toolu_ask","content":"終了する"}]}})
    .to_string();
    let t = write_transcript(&env, "t.jsonl", &[ask, ans]);
    expect_silent(&env.stop_full(&t, &[]), "AskUserQuestion answer 終了する");
}

#[test]
fn claim_explicit_stop_not_latched() {
    let env = Env::new("nolatch");
    env.write_phase("record_requested");
    env.pending(2);
    let p = env.home.join("t.jsonl");
    std::fs::write(&p, user_text_line("止めて") + "\n").unwrap();
    let t = p.to_string_lossy().into_owned();

    expect_silent(&env.stop_full(&t, &[]), "explicit stop");
    assert_ne!(env.phase().as_deref(), Some("done"));

    // The user keeps working: a later ordinary message.
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    writeln!(f, "{}", user_text_line("やっぱり続きをお願いします")).unwrap();
    drop(f);
    let r = expect_block(&env.stop_full(&t, &[]), "after the user resumed");
    assert!(r.contains("/condukt"), "got: {r}");
}

#[test]
fn claim_delegation_advisory_once() {
    let env = Env::new("deleg");
    env.write_phase("continuing");
    env.write_run_state(json!([
        {"id":"t1","status":"verified"},
        {"id":"t2","status":"pending"}
    ]));
    let bash = |cmd: &str| {
        json!({"type":"assistant","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_x","name":"Bash","input":{"command":cmd}}]}})
        .to_string()
    };
    let t = write_transcript(
        &env,
        "t.jsonl",
        &[
            bash("backlog lock acquire --task t1"),
            bash(&format!("condukt state resume-context --run {RUN_ID}")),
        ],
    );

    let r = expect_block(&env.stop_full(&t, &[]), "first stop");
    assert!(
        r.contains("fugu-router record --class flow-delegation"),
        "got: {r}"
    );

    // Second Stop: advisory not repeated (the pending branch speaks instead).
    env.write_run_state(json!([
        {"id":"t1","status":"verified"},
        {"id":"t2","status":"pending"}
    ]));
    let r2 = expect_block(&env.stop_full(&t, &[]), "second stop");
    assert!(!r2.contains("flow-delegation"), "advisory repeated: {r2}");
    assert!(r2.contains("/condukt"), "got: {r2}");
}

#[test]
fn claim_live_driver_stands_down() {
    let env = Env::new("live");
    env.write_phase("record_requested");
    env.pending(2);
    env.stub_backlog(&env.project_arg(), &env.live_status_json("other-session"));
    expect_silent(&env.stop(), "live driver for this project");
    assert_eq!(env.phase().as_deref(), Some("record_requested"));

    // control: backlog reports `none` for this project -> blocks.
    let ctl = Env::new("live-none");
    ctl.write_phase("record_requested");
    ctl.pending(2);
    ctl.stub_backlog(
        "/some/other/project",
        &ctl.live_status_json("other-session"),
    );
    expect_block(&ctl.stop(), "driver only on another project");
}

#[test]
fn claim_backlog_absent_no_standdown() {
    assert!(
        !Path::new("/usr/bin/backlog").exists() && !Path::new("/bin/backlog").exists(),
        "test premise: no backlog in the isolated PATH"
    );
    let env = Env::new("nobacklog");
    env.write_phase("record_requested");
    env.pending(2);
    assert!(std::fs::read_dir(&env.bin_dir).unwrap().next().is_none());
    let r = expect_block(&env.stop(), "backlog not installed");
    assert!(r.contains("/condukt"), "got: {r}");
}

#[test]
fn claim_precompact_never_blocks() {
    // lock held
    let env = Env::new("pc-held");
    env.stub_backlog(&env.project_arg(), &env.live_status_json(&env.session));
    let o = env.pre_compact();
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!o.stdout.contains("decision"), "got: {:?}", o.stdout);
    assert!(
        !env.prompt_submit().stdout.trim().is_empty(),
        "premise: the lock-held path really ran (marker written)"
    );

    // lock not held
    let env = Env::new("pc-free");
    let o = env.pre_compact();
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!o.stdout.contains("decision"), "got: {:?}", o.stdout);
}

#[test]
fn claim_hooks_json_wiring() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let hooks = v["hooks"].as_object().expect("hooks object");
    let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
    events.sort();
    assert_eq!(events, vec!["PreCompact", "Stop", "UserPromptSubmit"]);
    for (event, sub) in [
        ("Stop", "stop"),
        ("PreCompact", "pre-compact"),
        ("UserPromptSubmit", "prompt-submit"),
    ] {
        let entries = hooks[event].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{event}");
        let inner = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1, "{event}");
        assert_eq!(
            inner[0]["command"].as_str().unwrap(),
            format!("${{CLAUDE_PLUGIN_ROOT}}/bin/autoflow {sub}")
        );
    }
}

#[test]
fn claim_config_toml_override() {
    let env = Env::new("cfg-off");
    env.write_phase("record_requested");
    env.pending(2);
    env.write_config("enabled = false\n");
    expect_silent(&env.stop(), "~/.autoflow/config.toml enabled=false");

    // stuck_threshold = 1: the first no-progress Stop escalates.
    let env = Env::new("cfg-stuck1");
    env.write_config("stuck_threshold = 1\n");
    env.write_continuing(2, 0);
    env.pending(2);
    let r = expect_block(&env.stop(), "stuck_threshold=1");
    assert!(r.contains("進捗が止まっています"), "got: {r}");
    assert!(r.contains("1 回連続"), "got: {r}");

    // control: without the override the same Stop is a routine continuation.
    let ctl = Env::new("cfg-stuck-ctl");
    ctl.write_continuing(2, 0);
    ctl.pending(2);
    let r = expect_block(&ctl.stop(), "default threshold");
    assert!(!r.contains("進捗が止まっています"), "got: {r}");
}

#[test]
fn claim_unparseable_config_defaults() {
    let env = Env::new("cfg-garbage");
    env.write_phase("record_requested");
    env.pending(2);
    env.write_config("this is = = not [valid toml\n\u{0}\u{1}");
    let r = expect_block(&env.stop(), "garbage config.toml");
    assert!(r.contains("/condukt"), "got: {r}");

    // wrongly-typed values are also an unparseable file -> defaults apply.
    let env = Env::new("cfg-wrongtype");
    env.write_phase("record_requested");
    env.pending(2);
    env.write_config("enabled = \"no\"\n");
    expect_block(&env.stop(), "wrong-typed config.toml");
}

#[test]
fn claim_prompt_submit_inject_text() {
    let env = Env::new("inject");
    env.stub_backlog(&env.project_arg(), &env.live_status_json(&env.session));
    expect_silent(&env.pre_compact(), "pre-compact");

    let o = env.prompt_submit();
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(
        o.stdout
            .contains("中断した /flow の loop を次の一手から再開せよ"),
        "got: {:?}",
        o.stdout
    );

    // consumed: second call is silent.
    expect_silent(&env.prompt_submit(), "second prompt-submit");
}

#[test]
fn claim_disable_env() {
    let env = Env::new("disable");
    env.write_phase("record_requested");
    env.pending(2);
    expect_silent(
        &env.stop_full("", &[("AUTOFLOW_DISABLE", "1")]),
        "AUTOFLOW_DISABLE=1",
    );
    assert_eq!(env.task_statuses(), vec!["pending", "pending"]);
    // control
    expect_block(&env.stop(), "without AUTOFLOW_DISABLE");
}

#[test]
fn claim_only_launcher_committed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ls = Command::new("git")
        .args(["ls-files", "crates/autoflow/bin"])
        .current_dir(&root)
        .output()
        .expect("git runs");
    assert!(
        ls.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&ls.stderr)
    );
    let listed = String::from_utf8_lossy(&ls.stdout);
    let files: Vec<&str> = listed.lines().collect();
    assert_eq!(files, vec!["crates/autoflow/bin/autoflow"]);

    let ig = Command::new("git")
        .args([
            "check-ignore",
            "-q",
            "crates/autoflow/bin/autoflow-darwin-arm64",
        ])
        .current_dir(&root)
        .status()
        .expect("git runs");
    assert_eq!(ig.code(), Some(0), "per-platform binary must be gitignored");

    // and the launcher itself is not ignored
    let ig = Command::new("git")
        .args(["check-ignore", "-q", "crates/autoflow/bin/autoflow"])
        .current_dir(&root)
        .status()
        .expect("git runs");
    assert_eq!(ig.code(), Some(1), "launcher must not be ignored");
}

#[test]
fn claim_precompact_opt_out_with_lock() {
    // opted out: the session holds the lock but writes no marker.
    let env = Env::new("optout");
    env.stub_backlog(&env.project_arg(), &env.live_status_json(&env.session));
    env.write_config("resume_flow_on_compact = false\n");
    expect_silent(&env.pre_compact(), "pre-compact (opted out)");
    expect_silent(
        &env.prompt_submit(),
        "prompt-submit after opted-out pre-compact",
    );

    // control: default config, same lock -> marker written and injected.
    let ctl = Env::new("optout-ctl");
    ctl.stub_backlog(&ctl.project_arg(), &ctl.live_status_json(&ctl.session));
    expect_silent(&ctl.pre_compact(), "pre-compact (default)");
    assert!(
        !ctl.prompt_submit().stdout.trim().is_empty(),
        "control: default config must inject the resume instruction"
    );
}

#[test]
fn claim_unreadable_transcript_not_stop() {
    use std::os::unix::fs::PermissionsExt;

    // (a) exists but unreadable
    let env = Env::new("tr-unreadable");
    env.write_phase("record_requested");
    env.pending(2);
    let t = write_transcript(&env, "t.jsonl", &[user_text_line("止めて")]);
    std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read_to_string(&t).is_err(),
        "premise: transcript must really be unreadable (running as root?)"
    );
    let o = env.stop_full(&t, &[]);
    std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o644)).unwrap();
    let r = expect_block(&o, "unreadable transcript");
    assert!(r.contains("/condukt"), "got: {r}");

    // (b) malformed / non-JSON lines only
    let env = Env::new("tr-malformed");
    env.write_phase("record_requested");
    env.pending(2);
    let t = write_transcript(
        &env,
        "t.jsonl",
        &[
            "{ not json".to_string(),
            "止めて".to_string(),
            "\u{0}\u{1}".to_string(),
        ],
    );
    let r = expect_block(&env.stop_full(&t, &[]), "malformed transcript");
    assert!(r.contains("/condukt"), "got: {r}");
}

#[test]
fn claim_stop_side_effects_other_blocks() {
    fn check(env: &Env, what: &str, expect_ledger: bool) {
        let before = snapshot(&env.home);
        let o = env.stop();
        let after = snapshot(&env.home);
        let r = expect_block(&o, what);
        assert!(!r.trim().is_empty(), "{what}: empty reason");
        for p in before.keys() {
            assert!(after.contains_key(p), "{what}: deleted {}", p.display());
        }
        let mut ledger = 0;
        for (p, bytes) in &after {
            if before.get(p) == Some(bytes) {
                continue;
            }
            if *p == env.state_path() || *p == env.run_path() {
                continue;
            }
            if p.file_name().is_some_and(|n| n == "violations.jsonl")
                && p.starts_with(env.home.join(".overwatch"))
            {
                ledger += 1;
                continue;
            }
            panic!("{what}: unexpected side effect {}", p.display());
        }
        assert_eq!(
            ledger == 1,
            expect_ledger,
            "{what}: violation ledger writes"
        );
    }

    // escalation
    let env = Env::new("fx-esc");
    env.write_continuing(2, 2);
    env.pending(2);
    check(&env, "escalation block", true);

    // undetermined (unparseable run-state)
    let env = Env::new("fx-undet");
    env.write_phase("record_requested");
    std::fs::write(env.run_path(), "{ this is not json ").unwrap();
    check(&env, "undetermined block", true);

    // idle record
    let env = Env::new("fx-idle");
    env.write_metrics(2, 3);
    check(&env, "idle record block", true);
}

#[test]
fn claim_precompact_prompt_submit_cli_silent() {
    let env = Env::new("cli-silent");
    expect_silent(&env.pre_compact(), "pre-compact, no lock");
    expect_silent(&env.prompt_submit(), "prompt-submit, no marker");
}
