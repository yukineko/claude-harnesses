//! Classify what a background subagent is doing from facts observable right now.
//!
//! STUB: API only. The implementation is a different agent's job; the contract is
//! pinned by `tests/agent_state.rs`. Principle (CLAUDE.md §1/§3): an undeterminable
//! state is `Unknown` with its candidates listed, never "fine", never silence.

/// What a subagent is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    /// Last entries: assistant `end_turn` text, then a `SubagentStop` hook_success.
    Finished,
    /// Last assistant tool_use has no tool_result and a PreToolUse hook answered
    /// `permissionDecision: ask`. Only a transcript-side fact: print as
    /// "ask-pending (if not asleep)".
    AskPending { reason: String },
    /// Last entry is an `isApiErrorMessage` with error `rate_limit`.
    RateLimited { text: String },
    /// A pending tool_use whose command text appears in a `zsh -c` child process.
    ToolRunning { command: String },
    /// The machine was asleep at `now` (or for most of the gap); `underlying` is
    /// the state the transcript alone implies.
    Asleep {
        since: String,
        underlying: Box<AgentState>,
    },
    /// Not determinable. `candidates` lists what it could be; `why` says what could
    /// not be read or distinguished.
    Unknown {
        candidates: Vec<String>,
        why: String,
    },
}

/// Classify one subagent transcript.
///
/// * `transcript_jsonl` - the raw `agent-<id>.jsonl` text.
/// * `ps_output` - process listing (command lines); `None` = could not be read.
/// * `pmset_log` - `pmset -g log` text (Sleep/Wake lines); `None` = could not be read.
/// * `now_utc` - RFC 3339 UTC, e.g. `2026-10-31T16:00:00Z`.
pub fn classify(
    transcript_jsonl: &str,
    ps_output: Option<&str>,
    pmset_log: Option<&str>,
    now_utc: &str,
) -> AgentState {
    let _ = (transcript_jsonl, ps_output, pmset_log, now_utc);
    AgentState::Unknown {
        candidates: Vec::new(),
        why: "unimplemented".to_string(),
    }
}

/// CLI entry for `harness-status agents [--session <id>]`: one line per subagent
/// (id, description from `.meta.json`, state, last-entry time). Returns the exit code.
pub fn run_cli(projects_dir: &std::path::Path, session: Option<&str>) -> i32 {
    let _ = (projects_dir, session);
    eprintln!("agents: unimplemented");
    2
}
