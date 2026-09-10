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
    read_capped(std::io::stdin())
}

/// The pure core of [`read_stdin`]: apply the [`MAX_STDIN_BYTES`] cap and the
/// lossy decode to an arbitrary reader.
///
/// This exists so the cap and the decode can be exercised **without touching
/// the process's real stdin**. A test that reads ambient stdin is not hermetic:
/// when the test runner is launched with stdin on a pipe that never reaches
/// EOF, `read_to_end` blocks in `read(2)` forever, and a test that never
/// terminates reports no verdict at all — the verification apparatus itself
/// stops distinguishing "checked and fine" from "never finished checking".
/// Observed 2026-09-11: an orphaned `harness_core` test binary sat in
/// `hook.rs:158 read_stdin -> read_to_end -> read(2)` for over five minutes
/// under exactly that fd, while the same binary with stdin on `/dev/null`
/// finished in 6.3s.
pub fn read_capped<R: Read>(r: R) -> String {
    let mut buf = Vec::new();
    let _ = r.take(MAX_STDIN_BYTES).read_to_end(&mut buf);
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
    read_if_piped(std::io::stdin().is_terminal(), std::io::stdin())
}

/// The pure core of [`read_stdin_if_piped`]: the terminal check is an argument
/// rather than an ambient probe of fd 0, and the payload comes from an
/// arbitrary reader.
///
/// Both branches are therefore reachable from a test without the test
/// depending on how its own runner happened to wire fd 0. See [`read_capped`]
/// for why depending on that is not merely flaky but silently verdict-less.
pub fn read_if_piped<R: Read>(stdin_is_terminal: bool, r: R) -> String {
    if stdin_is_terminal {
        return String::new();
    }
    read_capped(r)
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
        // `is_headless()` only inspects the stdout fd, so calling it cannot
        // block under any runner. We do NOT assert an exact bool (it depends on
        // whether stdout is a terminal — captured, piped, or a real TTY), only
        // that the call returns.
        let headless: bool = is_headless();
        let _ = headless;

        // This used to call `read_stdin_if_piped()`, which reads the RUNNER's
        // own fd 0. The comment that stood here called that "hermetic and
        // non-flaky"; it was neither. With fd 0 on a pipe that never reaches
        // EOF the call blocks in `read(2)` and the test reports no verdict at
        // all — measured 2026-09-11 (backlog 5b8235fa): an orphaned test binary
        // sat in `read_stdin -> read_to_end -> read(2)` for over five minutes,
        // while the identical binary with fd 0 on `/dev/null` finished in 6.3s.
        //
        // The same two branches are covered here through the injected core, and
        // now actually assert a value: the old body ended in `let _ =
        // payload.len();`, which is true of every possible String.
        assert_eq!(
            read_if_piped(true, &b"never read"[..]),
            "",
            "an interactive stdin must short-circuit without reading the reader"
        );
        assert_eq!(
            read_if_piped(false, &b"payload"[..]),
            "payload",
            "a piped stdin must read the payload through"
        );
    }

    /// A reader that serves `head` and then fails with an [`std::io::Error`].
    ///
    /// [`read_capped`]'s doc comment claims "Errors are swallowed (returns what
    /// was read, possibly empty)". That is prose about a path no other test
    /// reaches, so it needs a reader that actually errors part way through.
    struct ErrAfter {
        head: Vec<u8>,
        drained: bool,
    }

    impl std::io::Read for ErrAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.drained {
                return Err(std::io::Error::other("synthetic mid-stream read failure"));
            }
            let n = self.head.len().min(buf.len());
            buf[..n].copy_from_slice(&self.head[..n]);
            self.head.drain(..n);
            self.drained = self.head.is_empty();
            Ok(n)
        }
    }

    /// A reader whose `read` must never be called.
    ///
    /// Panicking is strictly stronger than asserting an empty result: an empty
    /// reader also yields `""`, so `assert_eq!(.., "")` alone cannot tell "did
    /// not read" from "read and found nothing".
    struct PanicOnRead;

    impl std::io::Read for PanicOnRead {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            panic!("read_if_piped touched the reader even though stdin is a terminal");
        }
    }

    #[test]
    fn read_capped_truncates_at_exactly_max_stdin_bytes() {
        use std::io::Read as _;

        // A synthetic repeating reader offering MAX_STDIN_BYTES + OVER bytes.
        // Deliberately *bounded*: with an endless reader, a mutant that drops
        // the `.take(MAX_STDIN_BYTES)` would hang instead of fail, and a test
        // that never terminates reports no verdict at all (the very failure
        // mode `read_capped`'s doc comment records).
        const OVER: u64 = 4096;
        let over_cap = std::io::repeat(b'x').take(MAX_STDIN_BYTES + OVER);

        let got = read_capped(over_cap);
        assert_eq!(
            got.len() as u64,
            MAX_STDIN_BYTES,
            "a reader offering MAX_STDIN_BYTES + {OVER} bytes must yield exactly \
             MAX_STDIN_BYTES; got {} bytes",
            got.len()
        );
        assert!(
            got.bytes().all(|b| b == b'x'),
            "the truncated prefix must be the bytes the reader served"
        );

        // The other side of the boundary: input under the cap is not truncated.
        assert_eq!(read_capped(&b"short"[..]), "short");
    }

    #[test]
    fn read_capped_decodes_invalid_utf8_lossily() {
        // 0x80 is a UTF-8 continuation byte with no lead byte: not valid UTF-8.
        // A non-lossy decode would error (or panic on unwrap) here.
        assert_eq!(
            read_capped(&[b'a', 0x80, b'b'][..]),
            "a\u{FFFD}b",
            "an invalid byte must become U+FFFD, not abort the read"
        );
        assert_eq!(
            read_capped(&[0x80][..]),
            "\u{FFFD}",
            "input that is entirely invalid must still yield the replacement \
             char, not the empty string a swallowed decode error would give"
        );
    }

    #[test]
    fn read_capped_returns_partial_content_when_the_reader_errors_midway() {
        let got = read_capped(ErrAfter {
            head: b"partial".to_vec(),
            drained: false,
        });
        assert_eq!(
            got, "partial",
            "read_capped's doc comment promises errors are swallowed and what \
             was read is returned; a reader that serves bytes then fails must \
             therefore yield those bytes, not the empty string"
        );
    }

    #[test]
    fn read_if_piped_does_not_touch_the_reader_when_stdin_is_a_terminal() {
        // If the short-circuit is removed or inverted, PanicOnRead fires and
        // this test dies. Asserting only on the empty return value would not:
        // reading an exhausted reader also returns "".
        assert_eq!(
            read_if_piped(true, PanicOnRead),
            "",
            "an interactive stdin must return empty WITHOUT reading"
        );
    }

    #[test]
    fn read_if_piped_reads_through_and_splits_a_multibyte_char_at_the_cap() {
        use std::io::Read as _;

        assert_eq!(read_if_piped(false, &b"payload"[..]), "payload");

        // 'あ' is 3 bytes (E3 81 82). Placing it so that only its first byte
        // fits under the cap is exactly the case read_stdin's doc comment
        // names: "a multi-byte char split at the cap can't error".
        let split_at_cap = std::io::repeat(b'a')
            .take(MAX_STDIN_BYTES - 1)
            .chain("\u{3042}".as_bytes());

        let got = read_if_piped(false, split_at_cap);
        assert_eq!(
            got.chars().count() as u64,
            MAX_STDIN_BYTES,
            "MAX_STDIN_BYTES-1 filler chars plus one replacement char"
        );
        assert!(
            got.ends_with('\u{FFFD}'),
            "the half-read multi-byte char must decode to U+FFFD"
        );
        assert!(
            !got.contains('\u{3042}'),
            "the char was cut by the cap, so it must NOT appear intact"
        );
        assert_eq!(
            got.len() as u64,
            MAX_STDIN_BYTES - 1 + 3,
            "U+FFFD is 3 bytes in UTF-8, replacing the single truncated byte"
        );
    }
}
