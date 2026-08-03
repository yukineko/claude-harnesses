//! Detects an explicit user request to stop the auto-driven backlog/condukt
//! loop, read from the session's own transcript (Stop hook's
//! `transcript_path`) — the same "trust only real transcript structure, not
//! prose" discipline as `delegation_audit` (see that module's doc comment):
//! it never scans raw transcript text for a substring, only real `text`
//! content blocks on `role: "user"` turns and the `tool_result` that
//! answered a genuine `AskUserQuestion` `tool_use` (matched by
//! `tool_use_id`, not by position).
//!
//! Two distinct sources of an explicit stop, unified into a single "what did
//! the user say/choose most recently" signal (whichever came LAST in the
//! transcript wins — an earlier stop request the user later walked back by
//! continuing to work must not keep firing on a later, unrelated Stop):
//!
//! 1. The user's answer to the most recent `AskUserQuestion` tool call, if
//!    its content matches a stop/end phrase (e.g. the user picked an option
//!    labeled "終了する"/"Stop").
//! 2. The user's most recent freely-typed chat message (a `text` content
//!    block on a `role: "user"` turn — never a `tool_result` block), if it
//!    matches an explicit stop instruction (e.g. "止めて", "stop").
//!
//! This is a bounded, best-effort heuristic, not NLP — it cannot detect
//! negation at a distance (e.g. "やめてほしくない", "I don't want you to
//! stop") and does not try to. It deliberately matches only imperative/
//! declarative/dictionary stop *forms* (e.g. "止めて", "止める" — never the
//! bare stem "止め" alone), which sidesteps the most common false positive:
//! Japanese verb negation replaces the dictionary ending ("止め-る" →
//! "止め-ない") or appends after the stem ("止め" + "ないで"), so neither
//! negated form contains "止めて" or "止める" as a substring. Undetermined
//! (no signal, unreadable/empty transcript) always resolves to `false` —
//! never guess a stop that wasn't asked for; the caller's existing nag
//! behavior is the safe default here, not this escape hatch.

use std::collections::HashSet;

use serde_json::Value;

fn stop_pattern() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(止めて|とめて|やめて|止める|とめる|やめる|中止して|中止します|中止する|終了して|終了します|終了する|終わりにして|ここまでにして|これで終わり|please stop|you can stop\b|stop for now|stop here|stop the (loop|flow|backlog)|halt\b|that's (all|enough)|we'?re done|end the session|don'?t continue|do not continue)",
        )
        .expect("static regex is valid")
    })
}

/// A short exact answer (an `AskUserQuestion` option label, trimmed of
/// surrounding whitespace) that unambiguously means stop/end on its own —
/// separate from [`stop_pattern`] because a bare `"stop"`/`"end"` would be far
/// too noisy to match as a mid-sentence substring in ordinary prose, but as
/// the WHOLE content of a deliberately chosen short answer it is unambiguous.
fn is_exact_stop_answer(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "stop" | "end" | "終了" | "止める" | "やめる" | "中止"
    )
}

/// `tool_result.content` is either a bare string or an array of
/// `{type: "text", text: "..."}` blocks — same shape `delegation_audit`
/// handles.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The single most-recent user-authored signal in the transcript: either a
/// freely-typed chat message or the answer to an `AskUserQuestion`, whichever
/// occurred later. Streams the file line-by-line (one JSON value per line);
/// a corrupt/non-JSON line is skipped rather than aborting the scan
/// (best-effort, fail-soft — same as `delegation_audit::bash_tool_events`).
fn last_user_signal(text: &str) -> Option<String> {
    let mut ask_ids: HashSet<String> = HashSet::new();
    let mut last_signal: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(role) = obj.pointer("/message/role").and_then(Value::as_str) else {
            continue;
        };
        let Some(blocks) = obj.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };

        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") if role == "assistant" => {
                    if block.get("name").and_then(Value::as_str) == Some("AskUserQuestion") {
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            ask_ids.insert(id.to_string());
                        }
                    }
                }
                Some("tool_result") if role == "user" => {
                    let answered_an_ask = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| ask_ids.contains(id));
                    if answered_an_ask {
                        let joined = tool_result_text(block);
                        if !joined.trim().is_empty() {
                            last_signal = Some(joined);
                        }
                    }
                }
                Some("text") if role == "user" => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        if !t.trim().is_empty() {
                            last_signal = Some(t.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    last_signal
}

/// Fail-soft: unreadable/empty transcript path, or no matching signal,
/// returns `false` — never panics, never stands autoflow down on a guess.
pub fn user_requested_stop(transcript_path: &str) -> bool {
    if transcript_path.is_empty() {
        return false;
    }
    let text = match std::fs::read_to_string(transcript_path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    match last_user_signal(&text) {
        Some(s) => stop_pattern().is_match(&s) || is_exact_stop_answer(&s),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_text_line(text: &str) -> String {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [ { "type": "text", "text": text } ]
            }
        })
        .to_string()
    }

    fn assistant_text_line(text: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [ { "type": "text", "text": text } ]
            }
        })
        .to_string()
    }

    fn ask_user_question_line(id: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": id,
                        "name": "AskUserQuestion",
                        "input": { "questions": [] }
                    }
                ]
            }
        })
        .to_string()
    }

    fn ask_answer_line(tool_use_id: &str, answer_text: &str) -> String {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": answer_text
                    }
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
    fn empty_transcript_path_is_false() {
        assert!(!user_requested_stop(""));
    }

    #[test]
    fn unreadable_transcript_path_is_false() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.jsonl");
        assert!(!user_requested_stop(missing.to_str().unwrap()));
    }

    #[test]
    fn ordinary_session_with_no_stop_signal_is_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                user_text_line("add a test for the parser"),
                assistant_text_line("done, added the test"),
            ],
        );
        assert!(!user_requested_stop(&path));
    }

    #[test]
    fn explicit_stop_instruction_in_last_user_message_is_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                user_text_line("continue with the backlog"),
                assistant_text_line("working on it"),
                user_text_line("ok, please stop for now"),
            ],
        );
        assert!(user_requested_stop(&path));
    }

    #[test]
    fn negated_stop_form_does_not_false_positive() {
        // "止めないで" ("don't stop") contains the bare stem "止め" but NOT the
        // imperative "止めて" the pattern actually requires — the whole point
        // of matching the inflected form instead of the stem.
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), &[user_text_line("止めないで、続けて")]);
        assert!(
            !user_requested_stop(&path),
            "a negated stop form must not be read as an explicit stop request"
        );
    }

    #[test]
    fn negated_plain_stop_form_does_not_false_positive() {
        // "止めない" ("will not stop", plain negative) contains neither
        // "止めて" nor "止める" — the dictionary ending "-る" is replaced by
        // "-ない", not appended after it.
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), &[user_text_line("止めないで作業を続けます")]);
        assert!(!user_requested_stop(&path));
    }

    #[test]
    fn exact_short_ask_answer_of_stop_is_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                ask_user_question_line("toolu_ask1"),
                ask_answer_line("toolu_ask1", "  Stop  "),
            ],
        );
        assert!(user_requested_stop(&path));
    }

    #[test]
    fn stop_request_walked_back_by_later_work_is_false() {
        // An earlier stop request the user then walked back (kept working
        // afterward, without repeating it) must not keep firing on a later,
        // unrelated Stop — only the LAST signal counts.
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                user_text_line("stop the flow"),
                assistant_text_line("ok, stopped"),
                user_text_line("actually, keep going and also fix the typo in README"),
            ],
        );
        assert!(!user_requested_stop(&path));
    }

    #[test]
    fn ask_user_question_answered_with_stop_is_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                ask_user_question_line("toolu_ask1"),
                ask_answer_line("toolu_ask1", "終了する"),
            ],
        );
        assert!(user_requested_stop(&path));
    }

    #[test]
    fn ask_user_question_answered_with_continue_is_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                ask_user_question_line("toolu_ask1"),
                ask_answer_line("toolu_ask1", "続ける"),
            ],
        );
        assert!(!user_requested_stop(&path));
    }

    #[test]
    fn tool_result_not_matched_to_an_ask_is_ignored_even_if_it_mentions_stop() {
        // A tool_result whose tool_use_id does NOT correspond to a real
        // AskUserQuestion call (e.g. some other tool's output happens to
        // contain the word "stop") must not be read as a user answer.
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                json!({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "tool_use",
                                "id": "toolu_bash1",
                                "name": "Bash",
                                "input": { "command": "echo hi" }
                            }
                        ]
                    }
                })
                .to_string(),
                ask_answer_line("toolu_bash1", "please stop"),
            ],
        );
        assert!(!user_requested_stop(&path));
    }

    #[test]
    fn last_signal_prefers_ask_answer_over_earlier_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                user_text_line("keep working on the backlog"),
                ask_user_question_line("toolu_ask1"),
                ask_answer_line("toolu_ask1", "終了して"),
            ],
        );
        assert!(user_requested_stop(&path));
    }
}
