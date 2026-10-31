//! Contract for the subagent state classifier (`harness_status::agents`).
//!
//! Principle (CLAUDE.md section 1/3): an undeterminable state is an explicit
//! `Unknown` listing candidates; never a confident "running", never silence.
//! Fixtures follow the real transcript entry shapes (`type`, `message.content[]`
//! with `tool_use` / `tool_result`, `attachment.{type,hookName,toolUseID,stdout}`,
//! `isApiErrorMessage` + `error`, ISO `timestamp`).

use harness_status::agents::{classify, AgentState};
use serde_json::json;

const NOW: &str = "2026-10-31T16:00:00Z";
const T0: &str = "2026-10-31T15:00:00.000Z";
const CMD: &str = "cargo test -p harness-status --test agent_state";

fn line(v: serde_json::Value) -> String {
    format!("{v}\n")
}

fn user_prompt() -> String {
    line(json!({"type":"user","isSidechain":true,"agentId":"a1",
        "message":{"role":"user","content":"do the thing"},
        "timestamp":"2026-10-31T14:59:00.000Z"}))
}

fn assistant_tool_use(id: &str, command: &str) -> String {
    line(json!({"type":"assistant","isSidechain":true,"agentId":"a1",
        "message":{"role":"assistant","stop_reason":"tool_use","content":[
            {"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]},
        "timestamp":T0}))
}

fn tool_result(id: &str) -> String {
    line(json!({"type":"user","isSidechain":true,"agentId":"a1",
        "message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"ok"}]},
        "timestamp":"2026-10-31T15:00:05.000Z"}))
}

fn assistant_end_turn() -> String {
    line(json!({"type":"assistant","isSidechain":true,"agentId":"a1",
        "message":{"role":"assistant","stop_reason":"end_turn","content":[
            {"type":"text","text":"All done."}]},
        "timestamp":T0}))
}

fn subagent_stop() -> String {
    line(
        json!({"type":"attachment","isSidechain":true,"agentId":"a1",
        "attachment":{"type":"hook_success","hookName":"SubagentStop",
            "hookEvent":"SubagentStop","content":"","stdout":"","stderr":"","exitCode":0},
        "timestamp":"2026-10-31T15:00:01.000Z"}),
    )
}

fn pretooluse_ask(tool_use_id: &str) -> String {
    let stdout = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
        "permissionDecision":"ask","permissionDecisionReason":"blastguard: needs a human"}})
    .to_string();
    line(
        json!({"type":"attachment","isSidechain":true,"agentId":"a1",
        "attachment":{"type":"hook_success","hookName":"PreToolUse:Bash",
            "toolUseID":tool_use_id,"hookEvent":"PreToolUse","content":"",
            "stdout":stdout,"stderr":"","exitCode":0},
        "timestamp":"2026-10-31T15:00:01.000Z"}),
    )
}

fn rate_limit_error() -> String {
    line(json!({"type":"assistant","isSidechain":true,"agentId":"a1",
        "isApiErrorMessage":true,"error":"rate_limit",
        "message":{"role":"assistant","content":[
            {"type":"text","text":"You've hit your session limit \u{b7} resets 2:50am (Asia/Tokyo)"}]},
        "timestamp":T0}))
}

fn ps_with(command: &str) -> String {
    format!(
        "  501 100 1 /sbin/launchd\n  501 4242 4000 /bin/zsh -c cd /work && {command}\n  501 4300 4242 cargo test\n"
    )
}

const PS_OTHER: &str = "  501 100 1 /sbin/launchd\n  501 4242 4000 /bin/zsh -c ls -la\n";
const PMSET_AWAKE: &str = "2026-10-31 08:00:00 +0900 Wake  Wake from Normal Sleep\n";

fn candidates_mention(s: &AgentState, needle: &str) -> bool {
    match s {
        AgentState::Unknown { candidates, .. } => candidates.iter().any(|c| c.contains(needle)),
        _ => false,
    }
}

fn is_unknown_with_candidates(s: &AgentState) -> bool {
    matches!(s, AgentState::Unknown { candidates, .. } if !candidates.is_empty())
}

#[test]
fn finished_when_end_turn_then_subagent_stop() {
    let t = format!(
        "{}{}{}",
        user_prompt(),
        assistant_end_turn(),
        subagent_stop()
    );
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert_eq!(s, AgentState::Finished);
}

#[test]
fn end_turn_without_subagent_stop_is_not_finished() {
    let t = format!("{}{}", user_prompt(), assistant_end_turn());
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert_ne!(s, AgentState::Finished);
    assert!(is_unknown_with_candidates(&s), "got {s:?}");
}

#[test]
fn ask_pending_when_pretooluse_asked_and_no_result() {
    let t = format!(
        "{}{}{}",
        user_prompt(),
        assistant_tool_use("toolu_A", CMD),
        pretooluse_ask("toolu_A")
    );
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    match s {
        AgentState::AskPending { reason } => {
            assert!(
                reason.contains("blastguard") || !reason.is_empty(),
                "reason must carry the hook's reason, got {reason:?}"
            );
        }
        other => panic!("expected AskPending, got {other:?}"),
    }
}

#[test]
fn ask_for_a_different_tool_use_does_not_make_this_one_ask_pending() {
    let t = format!(
        "{}{}{}",
        user_prompt(),
        assistant_tool_use("toolu_A", CMD),
        pretooluse_ask("toolu_OTHER")
    );
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert!(!matches!(s, AgentState::AskPending { .. }), "got {s:?}");
    assert!(is_unknown_with_candidates(&s), "got {s:?}");
}

#[test]
fn rate_limited_when_last_entry_is_rate_limit_error() {
    let t = format!("{}{}", user_prompt(), rate_limit_error());
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    match s {
        AgentState::RateLimited { text } => assert!(text.contains("resets 2:50am"), "{text:?}"),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[test]
fn tool_running_when_pending_and_process_matches() {
    let t = format!("{}{}", user_prompt(), assistant_tool_use("toolu_A", CMD));
    let s = classify(&t, Some(&ps_with(CMD)), Some(PMSET_AWAKE), NOW);
    match s {
        AgentState::ToolRunning { command } => assert!(command.contains(CMD), "{command:?}"),
        other => panic!("expected ToolRunning, got {other:?}"),
    }
}

#[test]
fn pending_tool_without_matching_process_is_unknown_died_or_not_spawned() {
    let t = format!("{}{}", user_prompt(), assistant_tool_use("toolu_A", CMD));
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert!(matches!(s, AgentState::Unknown { .. }), "got {s:?}");
    assert!(candidates_mention(&s, "died"), "got {s:?}");
    assert!(candidates_mention(&s, "not yet spawned"), "got {s:?}");
}

#[test]
fn collision_pending_tool_with_ps_absent_is_unknown_not_tool_running() {
    // ps unreadable: a confident "running" would be a guess.
    let t = format!("{}{}", user_prompt(), assistant_tool_use("toolu_A", CMD));
    let s = classify(&t, None, Some(PMSET_AWAKE), NOW);
    assert!(!matches!(s, AgentState::ToolRunning { .. }), "got {s:?}");
    match &s {
        AgentState::Unknown { why, .. } => assert!(why.contains("ps"), "why={why:?}"),
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn none_ps_never_yields_confident_state_for_idle_transcript() {
    let t = format!("{}{}", user_prompt(), assistant_end_turn());
    let s = classify(&t, None, Some(PMSET_AWAKE), NOW);
    assert!(is_unknown_with_candidates(&s), "got {s:?}");
}

#[test]
fn none_pmset_is_unknown_cannot_read_not_not_asleep() {
    // Nothing pending, no error, no SubagentStop: the answer hinges on whether the
    // machine slept. pmset unreadable => cannot claim "slow model / died".
    let t = format!("{}{}", user_prompt(), assistant_end_turn());
    let s = classify(&t, Some(PS_OTHER), None, NOW);
    match &s {
        AgentState::Unknown { why, candidates } => {
            assert!(why.contains("pmset"), "why={why:?}");
            assert!(
                candidates.iter().any(|c| c.contains("sleep")),
                "{candidates:?}"
            );
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn nothing_pending_no_error_no_stop_is_unknown_listing_candidates() {
    let t = format!("{}{}", user_prompt(), assistant_end_turn());
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert!(candidates_mention(&s, "slow model generation"), "got {s:?}");
    assert!(candidates_mention(&s, "machine sleep"), "got {s:?}");
    assert!(candidates_mention(&s, "agent died"), "got {s:?}");
}

#[test]
fn tool_use_with_result_is_not_pending() {
    // The process list even contains the command text: still not ToolRunning,
    // because the tool_use already has a tool_result.
    let t = format!(
        "{}{}{}",
        user_prompt(),
        assistant_tool_use("toolu_A", CMD),
        tool_result("toolu_A")
    );
    let s = classify(&t, Some(&ps_with(CMD)), Some(PMSET_AWAKE), NOW);
    assert!(!matches!(s, AgentState::ToolRunning { .. }), "got {s:?}");
    assert!(!matches!(s, AgentState::AskPending { .. }), "got {s:?}");
    assert!(candidates_mention(&s, "slow model generation"), "got {s:?}");
}

#[test]
fn asleep_when_pmset_shows_machine_sleeping_at_now() {
    // last entry 15:00Z = 00:00 +0900; Sleep at 00:05 +0900, no Wake => asleep at now.
    let pmset = "2026-11-01 00:05:00 +0900 Sleep  Entering Sleep state due to 'Clamshell Sleep'\n";
    let t = format!("{}{}", user_prompt(), assistant_end_turn());
    let s = classify(&t, Some(PS_OTHER), Some(pmset), NOW);
    match s {
        AgentState::Asleep { since, underlying } => {
            assert!(
                since.contains("00:05") || since.contains("15:05"),
                "since={since:?}"
            );
            assert!(
                matches!(*underlying, AgentState::Unknown { .. }),
                "underlying={underlying:?}"
            );
        }
        other => panic!("expected Asleep, got {other:?}"),
    }
}

#[test]
fn garbage_input_is_unknown_cannot_read() {
    for bad in ["", "not json at all\n{{{ nope\n", "\u{0}\u{1}\u{2}"] {
        let s = classify(bad, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
        match &s {
            AgentState::Unknown { why, .. } => {
                assert!(why.contains("cannot read"), "input={bad:?} why={why:?}")
            }
            other => panic!("input={bad:?}: expected Unknown, got {other:?}"),
        }
    }
}

#[test]
fn partially_garbage_transcript_is_never_finished() {
    let t = format!("{}{}garbage line\n", user_prompt(), assistant_end_turn());
    let s = classify(&t, Some(PS_OTHER), Some(PMSET_AWAKE), NOW);
    assert_ne!(s, AgentState::Finished);
    assert!(is_unknown_with_candidates(&s), "got {s:?}");
}

// ---- CLI -----------------------------------------------------------------

fn write_agent(dir: &std::path::Path, id: &str, desc: &str, jsonl: &str) {
    let sub = dir.join("proj1").join("sess-1").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join(format!("agent-{id}.jsonl")), jsonl).unwrap();
    std::fs::write(
        sub.join(format!("agent-{id}.meta.json")),
        json!({"agentType":"general-purpose","description":desc}).to_string(),
    )
    .unwrap();
}

fn run_agents(dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_harness-status"))
        .arg("agents")
        .arg("--projects-dir")
        .arg(dir)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn cli_prints_one_line_per_subagent_with_id_description_state() {
    let tmp = tempfile::tempdir().unwrap();
    let fin = format!(
        "{}{}{}",
        user_prompt(),
        assistant_end_turn(),
        subagent_stop()
    );
    write_agent(tmp.path(), "aaa111", "finished worker", &fin);
    write_agent(tmp.path(), "bbb222", "broken worker", "garbage\n{{{\n");

    let out = run_agents(tmp.path(), &["--session", "sess-1"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one line per subagent, got {stdout:?}");

    let a = lines
        .iter()
        .find(|l| l.contains("aaa111"))
        .expect("aaa111 line");
    assert!(a.contains("finished worker"), "{a}");
    assert!(a.contains("finished"), "{a}");
    assert!(a.contains("2026-10-31"), "last-entry time missing: {a}");

    let b = lines
        .iter()
        .find(|l| l.contains("bbb222"))
        .expect("bbb222 line");
    assert!(b.contains("broken worker"), "{b}");
    assert!(b.contains("unknown"), "{b}");
    assert!(
        !b.contains("finished"),
        "unreadable must never read finished: {b}"
    );
}

#[test]
fn cli_session_filter_excludes_other_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let fin = format!(
        "{}{}{}",
        user_prompt(),
        assistant_end_turn(),
        subagent_stop()
    );
    write_agent(tmp.path(), "aaa111", "finished worker", &fin);
    let out = run_agents(tmp.path(), &["--session", "no-such-session"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("aaa111"), "{stdout:?}");
    // Silence would read as "nothing is stalled": say so explicitly.
    let all = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(all.contains("no subagents"), "silent on empty: {all:?}");
}
