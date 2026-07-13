//! Claude Code hook I/O: the stdin payload struct and the never-break-a-turn
//! execution wrapper.
//!
//! Invariant (shared by every plugin): a hook must NEVER break the user's turn.
//! On any error or panic we exit 0 and stay silent. `run_hook` enforces that.

use std::io::{IsTerminal, Read};
use std::panic::UnwindSafe;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Context window statistics supplied by Claude Code on Stop / SubagentStop events.
#[derive(Debug, Default, Deserialize)]
pub struct ContextWindow {
    /// Fraction of the context window used (0.0–100.0 as a percentage).
    /// `None` when Claude Code did not include the field.
    pub used_percentage: Option<f64>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
}

impl ContextWindow {
    /// Total token count (input + output), if both are present.
    pub fn total_tokens(&self) -> Option<u64> {
        match (self.total_input_tokens, self.total_output_tokens) {
            (Some(i), Some(o)) => Some(i + o),
            (Some(i), None) => Some(i),
            _ => None,
        }
    }
}

// Some fields (hook_event_name, tool_input) are part of the payload schema but
// not consumed by every plugin; kept for completeness and future hooks.
#[allow(dead_code)]
#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub hook_event_name: String,

    /// UserPromptSubmit
    #[serde(default)]
    pub prompt: String,

    /// PreCompact: "manual" | "auto"
    #[serde(default)]
    pub trigger: String,

    /// SessionStart: "startup" | "resume" | "clear" | "compact"
    #[serde(default)]
    pub source: String,

    /// PostToolUse
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,

    /// Notification: text supplied by Claude Code on a `Notification` event
    /// (e.g. "Claude needs your permission to use Bash").
    #[serde(default)]
    pub message: String,

    /// Stop / SubagentStop: true when this stop is itself the result of a
    /// previous stop-hook continuation.
    #[serde(default)]
    pub stop_hook_active: bool,

    /// Stop / SubagentStop: context window usage reported by Claude Code.
    /// `None` on hook events that don't carry this payload.
    pub context_window: Option<ContextWindow>,
}

impl HookInput {
    /// Parse a hook payload from a raw stdin string. Returns None on empty/invalid
    /// input so callers can stay silent (never break the user's turn).
    ///
    /// DoS note: deeply nested JSON (`{"a":{"a":…}}`) cannot exhaust the stack.
    /// `serde_json::from_str` enforces a recursion limit (128 levels by default);
    /// input nested past it is *rejected* — this returns None rather than
    /// recursing to a stack overflow. Do NOT swap in a deserializer with
    /// `.disable_recursion_limit()` or the DoS guard is lost (see
    /// `deep_nesting_is_rejected_not_stack_overflow`). Combined with the
    /// [`MAX_STDIN_BYTES`] read cap, both size- and depth-based stdin DoS are
    /// bounded.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        serde_json::from_str(raw).ok()
    }

    /// cwd, falling back to the process cwd if the hook did not supply one.
    pub fn cwd_or_current(&self) -> std::path::PathBuf {
        if self.cwd.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        } else {
            std::path::PathBuf::from(&self.cwd)
        }
    }

    /// Short, human-facing project label — the basename of `cwd`.
    pub fn project_name(&self) -> String {
        self.cwd_or_current()
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "project".to_string())
    }

    /// A stable session key for per-session state. Empty session ids (manual
    /// runs) collapse to a shared "_local" bucket.
    pub fn session_key(&self) -> String {
        if self.session_id.is_empty() {
            "_local".to_string()
        } else {
            self.session_id.clone()
        }
    }

    /// A short detail for the touched-file / command set, if extractable from
    /// the tool input (the path a file-oriented tool acted on).
    pub fn target(&self) -> Option<String> {
        let ti = self.tool_input.as_ref()?;
        match self.tool_name.as_str() {
            "Edit" | "Write" | "MultiEdit" | "Read" | "NotebookEdit" => ti
                .get("file_path")
                .or_else(|| ti.get("notebook_path"))
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            _ => None,
        }
    }
}

/// Upper bound on how much stdin a hook will read (10 MiB). Hook payloads are
/// small JSON; anything larger is treated as hostile or garbage. Bounding the
/// read stops an oversized or endless stdin from exhausting memory (DoS guard).
pub const MAX_STDIN_BYTES: u64 = 10 * 1024 * 1024;

/// Read stdin into a String, capped at [`MAX_STDIN_BYTES`]. Errors are swallowed
/// (returns what was read, possibly empty) so a hook never aborts on a read
/// hiccup; input past the cap is truncated rather than read unbounded. Reads as
/// bytes and lossily decodes so a multi-byte char split at the cap can't error.
pub fn read_stdin() -> String {
    let mut buf = Vec::new();
    let _ = std::io::stdin().take(MAX_STDIN_BYTES).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// True when stdout is NOT a terminal — i.e. the hook is running headless
/// (CI / cron / a piped or captured session) rather than attached to an
/// interactive TTY. When this is true, user-facing stdout reminders MUST be
/// suppressed: printing to a captured/piped stdout would pollute programmatic
/// or JSON output that a caller is parsing. This is the shared guard hooks use
/// to stay silent in non-interactive contexts (never break a turn, never
/// corrupt machine output). Pure: only inspects the stdout fd.
pub fn is_headless() -> bool {
    !std::io::stdout().is_terminal()
}

/// Read the hook's stdin payload only when it was actually piped in.
///
/// If stdin is an interactive terminal (no piped hook payload), returns
/// `String::new()` immediately instead of calling [`read_stdin`], which would
/// block on `read_to_end` waiting for a manual EOF. This matters for a
/// SessionEnd (or any) hook that can fire in an interactive terminal where
/// stdin has no EOF: blocking there would hang the turn. When stdin is piped
/// (the normal Claude Code hook path), delegates to [`read_stdin`] to read the
/// payload with the usual [`MAX_STDIN_BYTES`] cap. Pure: only inspects/reads
/// the stdin fd.
pub fn read_stdin_if_piped() -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    read_stdin()
}

/// Run `f`, swallowing any panic. Returns `true` if `f` completed without
/// panicking, `false` if it unwound. The testable core of `run_hook`.
pub fn catch_silent<F: FnOnce() + UnwindSafe>(f: F) -> bool {
    std::panic::catch_unwind(f).is_ok()
}

/// Like `catch_silent` but writes the panic payload to stderr so it is
/// visible in Claude Code's hook diagnostics without breaking the turn.
/// Returns `true` if `f` completed without panicking.
pub fn catch_and_log<F: FnOnce() + UnwindSafe>(hook_name: &str, f: F) -> bool {
    match std::panic::catch_unwind(f) {
        Ok(()) => true,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "(non-string panic payload)".to_string()
            };
            eprintln!("[harness hook panic] {hook_name}: {msg}");
            false
        }
    }
}

/// Run a hook handler with all panics caught and logged to stderr; always
/// exits 0 so the turn is never broken.
pub fn run_hook<F: FnOnce() + UnwindSafe>(f: F) -> ! {
    let _ = catch_and_log("hook", f);
    std::process::exit(0);
}

/// A hook registered in `settings.json` whose command's binary could not be
/// found on disk — surfaced for observability (e.g. a stale rollout, a
/// re-build that never ran `rollout-plugins.sh`/`rebuild-plugins.sh`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingHookBinary {
    /// The `hooks.<event>` key this hook group was registered under.
    pub event: String,
    /// The full `command` string as written in `settings.json`.
    pub command: String,
    /// The binary path extracted from `command` that does not exist.
    pub binary_path: String,
}

/// Extract the binary path from a hook `command` string: the first
/// whitespace-separated token, with a leading `${CLAUDE_PLUGIN_ROOT}` expanded
/// via `plugin_root` (when supplied — the env var is only meaningful while
/// Claude Code is actually invoking the hook, so a health-check run outside
/// that context passes the plugin root in explicitly).
///
/// Returns `None` when there's nothing concrete to check: an empty command, or
/// a `${CLAUDE_PLUGIN_ROOT}`-prefixed command with no `plugin_root` supplied
/// (unresolvable — never guessed at, so callers never false-flag it as
/// missing). Any *other* unexpanded `${VAR}` is returned as-is (rare/unknown
/// shape; existence-checking it will correctly report it missing, since a
/// literal `${...}` path segment never exists on disk).
pub fn extract_binary_path(command: &str, plugin_root: Option<&str>) -> Option<String> {
    let token = command.split_whitespace().next()?;
    if token.is_empty() {
        return None;
    }
    if let Some(rest) = token.strip_prefix("${CLAUDE_PLUGIN_ROOT}") {
        let root = plugin_root?;
        return Some(format!("{root}{rest}"));
    }
    Some(token.to_string())
}

/// True if `command`'s extracted binary path exists on disk. A command with no
/// extractable/resolvable path (empty, malformed, or an unresolved
/// `${CLAUDE_PLUGIN_ROOT}`) is treated as "exists" — i.e. not reported missing
/// — since there's nothing concrete to check; this function only flags a
/// *concrete, absent* path, never an unresolvable one (fail-soft: never turns
/// an unrelated shell snippet or unresolved var into a false warning).
fn binary_present(command: &str, plugin_root: Option<&str>) -> bool {
    match extract_binary_path(command, plugin_root) {
        Some(path) => Path::new(&path).exists(),
        None => true,
    }
}

/// Scan a parsed `settings.json` (as produced by [`crate::install::load_settings`])
/// for hook groups whose command's binary path does not exist, and return them.
/// `plugin_root`, when supplied, is substituted for a literal
/// `${CLAUDE_PLUGIN_ROOT}` prefix in a command (health-check runs happen
/// outside the env Claude Code sets, so callers that know the intended root —
/// e.g. checking one plugin's own install — can pass it; a caller scanning the
/// whole file across plugins generally can't, and those entries are silently
/// skipped rather than false-flagged). Never panics: a malformed/non-object
/// `settings` or a hook group missing expected fields is simply skipped.
pub fn missing_hook_binaries(
    settings: &Value,
    plugin_root: Option<&str>,
) -> Vec<MissingHookBinary> {
    let mut out = Vec::new();
    let Some(events) = settings.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for h in hooks {
                let Some(command) = h.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if !binary_present(command, plugin_root) {
                    if let Some(binary_path) = extract_binary_path(command, plugin_root) {
                        out.push(MissingHookBinary {
                            event: event.clone(),
                            command: command.to_string(),
                            binary_path,
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_binary_path_handles_plain_and_plugin_root_forms() {
        assert_eq!(
            extract_binary_path("/x/y/bin/daily session-start", None),
            Some("/x/y/bin/daily".to_string())
        );
        assert_eq!(
            extract_binary_path(
                "${CLAUDE_PLUGIN_ROOT}/bin/daily session-start",
                Some("/plugins/daily/0.1.0")
            ),
            Some("/plugins/daily/0.1.0/bin/daily".to_string())
        );
        // No plugin_root supplied: unresolvable, so no false-positive path.
        assert_eq!(
            extract_binary_path("${CLAUDE_PLUGIN_ROOT}/bin/daily", None),
            None
        );
        assert_eq!(extract_binary_path("", None), None);
        assert_eq!(extract_binary_path("   ", None), None);
    }

    #[test]
    fn missing_hook_binaries_flags_absent_paths_and_skips_unresolvable() {
        let missing_path = "/definitely/does/not/exist/bin/ghost";
        assert!(!Path::new(missing_path).exists());
        // A real, present binary path so the "exists" branch is also exercised.
        let present_path = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let settings = json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [
                        {"type": "command", "command": format!("{missing_path} session-start")},
                        {"type": "command", "command": format!("{present_path} watch")}
                    ]}
                ],
                "PostToolUse": [
                    {"hooks": [
                        // Unresolvable ${VAR} with no plugin_root passed for this
                        // scan — must NOT be false-flagged.
                        {"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/other check"}
                    ]}
                ]
            }
        });

        let missing = missing_hook_binaries(&settings, None);
        assert_eq!(
            missing.len(),
            1,
            "only the genuinely absent path is flagged: {missing:?}"
        );
        assert_eq!(missing[0].event, "SessionStart");
        assert_eq!(missing[0].binary_path, missing_path);
    }

    #[test]
    fn missing_hook_binaries_tolerates_malformed_and_empty_settings() {
        assert!(missing_hook_binaries(&json!({}), None).is_empty());
        assert!(missing_hook_binaries(&json!("not an object"), None).is_empty());
        assert!(missing_hook_binaries(&json!({"hooks": "not an object"}), None).is_empty());
        assert!(
            missing_hook_binaries(&json!({"hooks": {"Stop": "not an array"}}), None).is_empty()
        );
        assert!(missing_hook_binaries(
            &json!({"hooks": {"Stop": [{"hooks": "not an array"}]}}),
            None
        )
        .is_empty());
        assert!(missing_hook_binaries(
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command"}]}]}}),
            None
        )
        .is_empty());
    }

    #[test]
    fn parse_absorbs_any_event_and_rejects_empty() {
        assert!(HookInput::parse("").is_none());
        assert!(HookInput::parse("   \n").is_none());
        assert!(HookInput::parse("not json").is_none());
        // A PostToolUse-shaped payload with fields absent from other events.
        let h = HookInput::parse(r#"{"session_id":"S","tool_name":"Read"}"#).unwrap();
        assert_eq!(h.session_id, "S");
        assert_eq!(h.tool_name, "Read");
        assert_eq!(h.prompt, ""); // missing field → default, never a parse failure
    }

    #[test]
    fn deep_nesting_is_rejected_not_stack_overflow() {
        // A hostile hook payload nested far past serde_json's recursion limit
        // (128) must be rejected gracefully — parse returns None — rather than
        // recursing into a stack overflow (DoS). This pins the guard so a future
        // refactor to a `.disable_recursion_limit()` deserializer, or a custom
        // recursive parser without a depth bound, is caught here.
        for depth in [500usize, 5_000, 100_000] {
            let mut payload = String::with_capacity(depth * 14 + 1);
            for _ in 0..depth {
                payload.push_str("{\"tool_input\":");
            }
            payload.push('1');
            for _ in 0..depth {
                payload.push('}');
            }
            // Must not panic / overflow the stack, and must reject the input.
            let parsed =
                std::panic::catch_unwind(|| HookInput::parse(&payload)).unwrap_or_else(|_| {
                    panic!("depth {depth}: parse panicked/overflowed instead of rejecting")
                });
            assert!(
                parsed.is_none(),
                "depth {depth}: over-nested payload must be rejected (None), got Some"
            );
        }

        // Sanity: nesting within the limit still parses (the guard rejects only
        // the pathological depth, not ordinary nested tool_input).
        let shallow = r#"{"tool_name":"Edit","tool_input":{"a":{"b":{"c":1}}}}"#;
        assert!(HookInput::parse(shallow).is_some());
    }

    #[test]
    fn catch_silent_contains_panic_and_reports_outcome() {
        // Suppress the default panic message so the test output stays clean.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = catch_silent(|| panic!("boom"));
        let ok = catch_silent(|| { /* no-op */ });
        std::panic::set_hook(prev);
        assert!(
            !panicked,
            "a panicking handler must be caught, not propagated"
        );
        assert!(ok, "a clean handler reports success");
    }

    #[test]
    fn catch_and_log_returns_false_on_panic_and_true_on_ok() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = catch_and_log("test-hook", || panic!("logged panic"));
        let ok = catch_and_log("test-hook", || {});
        std::panic::set_hook(prev);
        assert!(!panicked, "panicking handler returns false");
        assert!(ok, "clean handler returns true");
    }

    #[test]
    fn headless_and_piped_guards_are_callable_and_pure() {
        // Both guards must complete without panicking regardless of how the
        // test harness wires stdout/stdin fds. We do NOT assert an exact bool
        // for `is_headless()` (it depends on whether stdout is a terminal under
        // the runner — captured, piped, or a real TTY), only that the call
        // returns a bool. This keeps the test hermetic and non-flaky.
        let headless: bool = is_headless();
        let _ = headless;

        // Under `cargo test` stdin is typically NOT a terminal, so this takes
        // the piped path and reads (usually EOF → empty). It must not block on
        // an interactive read and must return a String either way.
        let payload: String = read_stdin_if_piped();
        let _ = payload.len();
    }
}
