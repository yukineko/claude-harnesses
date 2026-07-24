//! Tier 2 delegation-record advisory: an independent, non-LLM-controlled check
//! that inspects the session's own transcript (Stop hook's `transcript_path`)
//! to catch a `/flow`-driven condukt run completing without the LLM ever
//! calling `fugu-router record` for it. Tier 1 (`fugu-router audit-recent`)
//! relies on the LLM remembering to self-verify; this tier catches the case
//! where it forgets to even try.
//!
//! Both firing conditions below only trust **actually executed Bash tool_use
//! invocations** (`message.content[].type == "tool_use"` with
//! `name == "Bash"`, reading `input.command`), never raw substring matches
//! over the whole transcript text. The transcript also contains the `/flow`
//! skill's own Markdown documentation verbatim (injected as ordinary
//! assistant/user prose when the skill file is read), which quotes example
//! commands like `fugu-router record --class "flow-delegation" --delegation
//! <fork|inline>` and `backlog lock acquire` as *instructions*, not as
//! executed commands. A raw substring search cannot tell those apart from a
//! real invocation and fires a false alarm on doc text alone — this module
//! only looks at genuine `tool_use` Bash command strings.

use std::path::Path;

use regex::Regex;
use serde_json::Value;

/// Matches a condukt run id: `run-YYYYMMDD-HHMMSS-<pid>` (see
/// `condukt::main::default_run_id`), anchored to a word boundary on both
/// sides so it doesn't partially match inside a longer token.
fn run_id_pattern() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"run-\d{8}-\d{6}-\d+").expect("static regex is valid"))
}

/// Extract the `command` string of every top-level Bash `tool_use` invocation
/// in the transcript, plus the text content of every `tool_result` that
/// followed one. Both are trustworthy evidence of *actually executed* shell
/// commands and their *actually observed* output — unlike assistant/user
/// prose, which can contain the same substrings quoted as documentation.
///
/// Streams the file line-by-line (one JSON value per line) rather than
/// loading it whole; a corrupt/non-JSON line is skipped rather than aborting
/// the scan (best-effort, fail-soft — see module doc).
fn bash_tool_events(text: &str) -> (Vec<String>, Vec<String>) {
    let mut commands = Vec::new();
    let mut results = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(content) = obj.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let Some(blocks) = content.as_array() else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") if block.get("name").and_then(Value::as_str) == Some("Bash") => {
                    if let Some(cmd) = block
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(Value::as_str)
                    {
                        commands.push(cmd.to_string());
                    }
                }
                Some("tool_result") => {
                    // `content` on a tool_result is either a bare string or an
                    // array of `{type: "text", text: "..."}` blocks.
                    match block.get("content") {
                        Some(Value::String(s)) => results.push(s.clone()),
                        Some(Value::Array(items)) => {
                            for item in items {
                                if let Some(t) = item.get("text").and_then(Value::as_str) {
                                    results.push(t.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    (commands, results)
}

/// Which condukt run id(s) did THIS session's own transcript actually drive?
///
/// Scans genuine executed Bash `tool_use` commands (e.g. `condukt state
/// init`, `condukt state resume-context --run <RID>`, `condukt state set
/// --run <RID> ...`) for a `--run <run-id>` argument, and scans the text of
/// the `tool_result`s that followed such commands (e.g. the bare run id
/// `condukt state init` prints to stdout) for the same `run-YYYYMMDD-HHMMSS-
/// <pid>` shape. Ignores assistant/user prose entirely — a run id can only be
/// recovered from something the harness actually executed or actually
/// observed, never from a doc/example quoting one.
///
/// Returns an empty `Vec` (never panics) on any read/parse failure or when no
/// run id is found — callers must treat that as "undetermined" and fail soft
/// (see [`missing_delegation_record`]).
pub fn extract_flow_run_ids(text: &str) -> Vec<String> {
    let (commands, results) = bash_tool_events(text);
    let re = run_id_pattern();

    let mut ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_unique = |id: &str| {
        if seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
    };

    for cmd in &commands {
        // Only trust run ids from commands that are actually condukt
        // invocations, not any Bash command that happens to mention the
        // shape (defense in depth beyond the tool_use/prose split above).
        if !cmd.contains("condukt") {
            continue;
        }
        for m in re.find_iter(cmd) {
            push_unique(m.as_str());
        }
    }
    for out in &results {
        for m in re.find_iter(out) {
            push_unique(m.as_str());
        }
    }

    ids
}

/// Fail-soft check: does this session's transcript show `/flow` drove a
/// condukt run to completion without ever calling `fugu-router record` (with
/// `--class flow-delegation` or `--delegation`)? Returns `false` on any
/// read/parse failure or when any of the three firing conditions isn't met —
/// never panics, never blocks the Stop turn.
///
/// Firing requires ALL of:
/// 1. The transcript shows an executed Bash `tool_use` command containing
///    `"backlog lock acquire"` — proof this session was `/flow`-driven
///    (flow's SKILL.md Step 2 always calls this). Matching only actual
///    `tool_use` command strings (not raw transcript text) means the
///    skill's own documentation quoting this command as an example doesn't
///    count as evidence.
/// 2. At least one condukt run id this session's transcript actually shows
///    it interacting with ([`extract_flow_run_ids`]) reached a terminal
///    state on at least one task
///    ([`crate::condukt::has_completed_tasks_for_run`]) — a completion
///    happened, scoped to a run THIS session drove, not merely the newest
///    run file on disk (which could belong to a concurrent, unrelated
///    session — see `condukt::has_completed_tasks_for_run`'s doc comment).
///    If no run id can be extracted at all, this condition is treated as
///    unmet (fail soft: undetermined never fires this advisory).
/// 3. No executed Bash `tool_use` command mentions both `"fugu-router
///    record"` and (`"flow-delegation"` or `"--delegation"`) — no evidence a
///    delegation record call was ever actually run (as opposed to merely
///    quoted in prose/documentation).
pub fn missing_delegation_record(transcript_path: &str, cwd: &Path) -> bool {
    if transcript_path.is_empty() {
        return false;
    }
    let text = match std::fs::read_to_string(transcript_path) {
        Ok(t) => t,
        Err(_) => return false,
    };

    let (commands, _results) = bash_tool_events(&text);

    let flow_driven = commands.iter().any(|c| c.contains("backlog lock acquire"));
    if !flow_driven {
        return false;
    }

    let run_ids = extract_flow_run_ids(&text);
    if run_ids.is_empty() {
        // Undetermined which run (if any) this session drove — fail soft,
        // never fire on a suspicion we can't attribute to this session.
        return false;
    }
    let any_completed = run_ids
        .iter()
        .any(|rid| crate::condukt::has_completed_tasks_for_run(cwd, rid));
    if !any_completed {
        return false;
    }

    let has_record = commands.iter().any(|c| {
        c.contains("fugu-router record")
            && (c.contains("flow-delegation") || c.contains("--delegation"))
    });

    !has_record
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // These tests exercise `missing_delegation_record`'s transcript-reading half
    // directly; they don't touch `$HOME`, so no `test_home_guard` is needed —
    // but they DO call `condukt::has_completed_tasks_for_run`, which reads
    // `$HOME/.condukt/state/...`. To keep condition 2 controllable without a
    // real condukt run on disk, we point `cwd` at a repo with no `.condukt`
    // state at all (so the lookup is deterministically `false`) except where
    // a test explicitly needs it `true`, in which case it reuses condukt.rs's
    // own HOME-mutating test harness pattern inline.

    /// Build one assistant-turn JSONL line containing a single Bash `tool_use`
    /// block with the given command.
    fn bash_tool_use_line(command: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_test",
                        "name": "Bash",
                        "input": { "command": command }
                    }
                ]
            }
        })
        .to_string()
    }

    /// Build one user-turn JSONL line containing a `tool_result` block whose
    /// content is the given plain-string stdout.
    fn tool_result_line(text: &str) -> String {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_test",
                        "content": text
                    }
                ]
            }
        })
        .to_string()
    }

    /// Build one assistant-turn JSONL line of plain prose text (e.g. the
    /// `/flow` skill's own doc content being quoted back), NOT a tool_use.
    fn prose_line(text: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": text }
                ]
            }
        })
        .to_string()
    }

    fn write_transcript(dir: &std::path::Path, lines: &[String]) -> String {
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn not_flow_driven_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), &[prose_line("some ordinary session line")]);
        // Whatever cwd/condukt state, condition 1 fails first.
        assert!(!missing_delegation_record(&path, dir.path()));
    }

    #[test]
    fn unreadable_transcript_path_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.jsonl");
        assert!(!missing_delegation_record(
            missing.to_str().unwrap(),
            dir.path()
        ));
    }

    #[test]
    fn empty_transcript_path_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!missing_delegation_record("", dir.path()));
    }

    #[test]
    fn flow_driven_but_no_completed_tasks_returns_false() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // No .condukt run state at all → has_completed_tasks_for_run is false
        // for any run id, even though we do extract one.

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                bash_tool_use_line("backlog lock acquire --task t1"),
                bash_tool_use_line("condukt state init"),
                tool_result_line("run-20260101-000000-1111"),
            ],
        );
        assert!(!missing_delegation_record(&path, &repo));
    }

    #[test]
    fn flow_driven_and_completed_but_record_present_returns_false() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_id = "run-20260101-000000-2222";
        std::fs::write(
            run_dir.join(format!("{run_id}.json")),
            format!(
                r#"{{"run_id":"{run_id}","goal":"g","tasks":[{{"id":"t1","status":"verified"}}]}}"#
            ),
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                bash_tool_use_line("backlog lock acquire --task t1"),
                bash_tool_use_line(&format!("condukt state resume-context --run {run_id}")),
                bash_tool_use_line(
                    r#"fugu-router record --title "x" --class "flow-delegation" --delegation fork"#,
                ),
            ],
        );
        assert!(!missing_delegation_record(&path, &repo));
    }

    #[test]
    fn flow_driven_and_completed_and_no_record_returns_true() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_id = "run-20260101-000000-3333";
        std::fs::write(
            run_dir.join(format!("{run_id}.json")),
            format!(
                r#"{{"run_id":"{run_id}","goal":"g","tasks":[{{"id":"t1","status":"verified"}}]}}"#
            ),
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                bash_tool_use_line("backlog lock acquire --task t1"),
                bash_tool_use_line("condukt state init"),
                tool_result_line(run_id),
                bash_tool_use_line("some other tool call"),
            ],
        );
        assert!(missing_delegation_record(&path, &repo));
    }

    /// False-positive avoidance: an ordinary condukt run driven WITHOUT `/flow`
    /// (no "backlog lock acquire" anywhere in the transcript) must never trigger
    /// the advisory, even when it completed tasks and never called
    /// `fugu-router record` — because it was never expected to.
    #[test]
    fn ordinary_non_flow_condukt_run_never_fires() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_id = "run-20260101-000000-4444";
        std::fs::write(
            run_dir.join(format!("{run_id}.json")),
            format!(
                r#"{{"run_id":"{run_id}","goal":"g","tasks":[{{"id":"t1","status":"verified"}}]}}"#
            ),
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        // A plain `/condukt` invocation transcript: tasks completed, no
        // delegation record call, but crucially no "backlog lock acquire" —
        // this session was never /flow-driven.
        let path = write_transcript(
            dir.path(),
            &[
                prose_line("user ran /condukt directly"),
                bash_tool_use_line(&format!(
                    "condukt state set --run {run_id} --status verified"
                )),
            ],
        );
        assert!(
            !missing_delegation_record(&path, &repo),
            "non-/flow condukt runs must never trigger the delegation advisory"
        );
    }

    /// Regression for root cause 1: the `/flow` SKILL.md's own documentation
    /// text — quoting `backlog lock acquire` and a `fugu-router record
    /// --class "flow-delegation" --delegation <fork|inline>` example as
    /// *instructions*, not as executed commands — appearing verbatim in the
    /// transcript (e.g. because the skill file was read/quoted back) must
    /// never trigger the advisory on its own. Only genuine executed Bash
    /// tool_use invocations count as evidence.
    #[test]
    fn skill_doc_text_quoting_commands_never_fires() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_id = "run-20260101-000000-5555";
        std::fs::write(
            run_dir.join(format!("{run_id}.json")),
            format!(
                r#"{{"run_id":"{run_id}","goal":"g","tasks":[{{"id":"t1","status":"verified"}}]}}"#
            ),
        )
        .unwrap();

        // Verbatim-ish excerpt of the /flow SKILL.md's own doc content, as it
        // would appear when the skill file gets read into the transcript as
        // plain assistant/user prose (a file-read tool_result, or the skill
        // instructions block) — never as an executed tool_use.
        let skill_doc_excerpt = r#"
## Step 2: acquire the backlog lock

Run `backlog lock acquire` before starting work on a task.

## Step N: record the delegation

After the condukt run finishes, call:

    fugu-router record --title "..." --class "flow-delegation" --delegation <fork|inline>

so Tier 1 auditing can find it later.
"#;

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                prose_line(skill_doc_excerpt),
                // The session's actual, real tool_use activity: it drove a
                // condukt run to completion directly (not through /flow) and
                // never really invoked fugu-router record.
                bash_tool_use_line("condukt state init"),
                tool_result_line(run_id),
                bash_tool_use_line(&format!(
                    "condukt state set --run {run_id} --status verified"
                )),
            ],
        );
        assert!(
            !missing_delegation_record(&path, &repo),
            "doc/prose text quoting the trigger commands must not count as real invocation evidence"
        );
    }

    #[test]
    fn extract_flow_run_ids_ignores_prose_and_non_condukt_commands() {
        let text = [
            prose_line("see run-20260101-000000-9999 in the docs for an example"),
            bash_tool_use_line("echo run-20260101-000000-8888"),
            bash_tool_use_line("condukt state init"),
            tool_result_line("run-20260101-000000-7777"),
        ]
        .join("\n");

        let ids = extract_flow_run_ids(&text);
        assert_eq!(
            ids,
            vec!["run-20260101-000000-7777".to_string()],
            "prose mentions and non-condukt commands must not be treated as evidence \
             of a run this session drove"
        );
    }

    #[test]
    fn extract_flow_run_ids_from_run_flag_in_command() {
        let text = bash_tool_use_line(
            "condukt state set --run run-20260101-000000-6666 --status verified",
        );
        let ids = extract_flow_run_ids(&text);
        assert_eq!(ids, vec!["run-20260101-000000-6666".to_string()]);
    }

    #[test]
    fn extract_flow_run_ids_empty_when_none_found() {
        let text = prose_line("no run ids here at all");
        assert!(extract_flow_run_ids(&text).is_empty());
    }
}
