//! Claude Code hook I/O: the stdin payload struct and the panic-barrier
//! execution wrappers.
//!
//! Two barriers, split by whether the hook decides anything:
//!
//! * [`run_hook`] — for hooks that decide NOTHING (pure observability). A
//!   panic is logged to stderr and the process exits 0.
//! * [`run_pretooluse_guarded`] — for verdict-bearing PreToolUse hooks. For
//!   PreToolUse a silent exit 0 is an ALLOW, so a panic exits 2 (block) with a
//!   stderr message instead (CLAUDE.md §3: cannot-determine resolves to the
//!   restrictive side).
//!
//! Stop gates use `crate::gate::run::run_guarded` (the Stop protocol).

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

/// Read stdin into a String, capped at [`MAX_STDIN_BYTES`]; input past the cap
/// is truncated rather than read unbounded. Reads as bytes and lossily decodes
/// so a multi-byte char split at the cap can't error.
///
/// A stdin READ ERROR is NOT swallowed: an empty or partial payload would be
/// indistinguishable from "nothing was piped" / a complete payload (CLAUDE.md
/// §3: IO failure is cannot-determine). This function therefore refuses by
/// panicking (user ruling 2026-11-03, backlog 8004d80f). What that panic
/// becomes depends on where it is raised: inside `gate::run::run_guarded`'s
/// body it is that barrier's fail-closed block (bounded allow on a consecutive
/// re-entry); inside [`run_pretooluse_guarded`] it is exit 2 (tool call
/// blocked); inside [`run_hook`] it is swallowed (exit 0, nothing emitted);
/// anywhere else (e.g. a gate's `main` before `run_guarded`) it is an uncaught
/// panic (exit 101), which is NOT a block decision. So callers that can handle
/// the failure themselves use [`try_read_stdin`]; Stop gates reading before
/// `gate::run::run_guarded` use `gate::run::read_stop_payload_guarded`.
pub fn read_stdin() -> String {
    read_capped(std::io::stdin())
}

/// Fallible [`read_stdin`]: a read error at any point (including after a
/// partial read) is `Err`, never a partial `Ok`.
pub fn try_read_stdin() -> std::io::Result<String> {
    try_read_capped(std::io::stdin())
}

/// The pure core of [`try_read_stdin`]: apply the [`MAX_STDIN_BYTES`] cap and the
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
pub fn try_read_capped<R: Read>(r: R) -> std::io::Result<String> {
    let mut buf = Vec::new();
    r.take(MAX_STDIN_BYTES).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Infallible wrapper over [`try_read_capped`]: same cap and lossy decode, but
/// a read error (including after a partial read) panics instead of returning
/// the empty or partial payload (user ruling 2026-11-03, backlog 8004d80f).
/// The panic is a block only inside `gate::run::run_guarded`; see
/// [`read_stdin`] for what it becomes elsewhere.
#[allow(
    clippy::panic,
    reason = "user ruling 2026-11-03 (backlog 8004d80f): a stdin read error must refuse, never become an empty/partial Ok payload; the panic is a block only inside run_guarded (swallowed under run_hook, exit 101 elsewhere)"
)]
pub fn read_capped<R: Read>(r: R) -> String {
    match try_read_capped(r) {
        Ok(s) => s,
        Err(e) => panic!("stdin read failed: {e} (refusing to return an empty or partial payload)"),
    }
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
/// payload with the usual [`MAX_STDIN_BYTES`] cap; a read error panics there
/// (see [`read_stdin`]). Use [`try_read_stdin_if_piped`] to get the error.
pub fn read_stdin_if_piped() -> String {
    read_if_piped(std::io::stdin().is_terminal(), std::io::stdin())
}

/// Fallible [`read_stdin_if_piped`]: a read error is `Err`.
pub fn try_read_stdin_if_piped() -> std::io::Result<String> {
    try_read_if_piped(std::io::stdin().is_terminal(), std::io::stdin())
}

/// The pure core of [`try_read_stdin_if_piped`]: the terminal check is an
/// argument rather than an ambient probe of fd 0, and the payload comes from an
/// arbitrary reader.
///
/// Both branches are therefore reachable from a test without the test
/// depending on how its own runner happened to wire fd 0. See [`try_read_capped`]
/// for why depending on that is not merely flaky but silently verdict-less.
pub fn try_read_if_piped<R: Read>(stdin_is_terminal: bool, r: R) -> std::io::Result<String> {
    if stdin_is_terminal {
        return Ok(String::new());
    }
    try_read_capped(r)
}

/// Infallible wrapper over [`try_read_if_piped`]: a read error panics (see
/// [`read_capped`]; user ruling 2026-11-03, backlog 8004d80f).
#[allow(
    clippy::panic,
    reason = "user ruling 2026-11-03 (backlog 8004d80f): a stdin read error must refuse, never become an empty/partial Ok payload"
)]
pub fn read_if_piped<R: Read>(stdin_is_terminal: bool, r: R) -> String {
    match try_read_if_piped(stdin_is_terminal, r) {
        Ok(s) => s,
        Err(e) => panic!("stdin read failed: {e} (refusing to return an empty or partial payload)"),
    }
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
///
/// ONLY for hooks that decide nothing (pure observability). For a PreToolUse
/// hook a silent exit 0 IS an allow, so a verdict-bearing PreToolUse hook must
/// use [`run_pretooluse_guarded`] instead; a Stop gate uses
/// `crate::gate::run::run_guarded`.
pub fn run_hook<F: FnOnce() + UnwindSafe>(f: F) -> ! {
    let _ = catch_and_log("hook", f);
    std::process::exit(0);
}

/// Exit code Claude Code treats as "block this tool call" for a PreToolUse
/// hook (stderr is shown to the model).
pub const PRETOOLUSE_BLOCK_EXIT: i32 = 2;

/// Run a verdict-bearing PreToolUse hook body under a FAIL-CLOSED panic
/// barrier, then exit.
///
/// * The body returned normally: exit 0. Whatever decision it already printed
///   (a `permissionDecision` JSON, or nothing = defer to the normal permission
///   flow) stands. A body that wants another exit code calls
///   `std::process::exit` itself; that path is untouched.
/// * The body panicked — anywhere: stdin read, payload parse, config load, the
///   detector, or the decision `println!` (EPIPE on a closed stdout panics):
///   exit [`PRETOOLUSE_BLOCK_EXIT`] (2) with a stderr line saying the gate
///   crashed and the tool call is blocked. The call was not judged, and for
///   PreToolUse exit 0 would be an allow (CLAUDE.md §3: a panic is a
///   cannot-determine and resolves to the restrictive side).
///
/// Why not `crate::gate::run::run_guarded`: that is the Stop protocol — it
/// answers a panic with `{"decision":"block"}` on stdout and bounds it with
/// `stop_hook_active`, which a PreToolUse payload does not carry. Here stdout
/// itself may be what panicked, so the answer is carried by the exit code
/// only. No bounded-allow is needed: a blocked tool call does not trap the
/// session; the model sees stderr and can do something else.
///
/// Modelled on blastguard's local `run_verdict_guarded` (commit 99f1c0ef).
pub fn run_pretooluse_guarded<F: FnOnce()>(gate_name: &str, body: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    std::process::exit(pretooluse_exit_code(
        gate_name,
        outcome,
        &mut std::io::stderr(),
    ))
}

/// Pure core of [`run_pretooluse_guarded`]: 0 when the body completed, 2 when
/// it panicked (with the block message written to `err`). A failed write to
/// `err` is ignored on purpose: the exit code, not the message, carries the
/// block, so the restrictive outcome does not depend on stderr being writable.
pub fn pretooluse_exit_code<W: std::io::Write>(
    gate_name: &str,
    outcome: std::thread::Result<()>,
    err: &mut W,
) -> i32 {
    match outcome {
        Ok(()) => 0,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "(non-string panic payload)".to_string()
            };
            let _ = writeln!(
                err,
                "[harness hook panic] {gate_name}: the PreToolUse gate crashed (panic: {msg}) \
                 — the tool call was not judged, so it is BLOCKED (exit {PRETOOLUSE_BLOCK_EXIT}), \
                 not allowed"
            );
            PRETOOLUSE_BLOCK_EXIT
        }
    }
}

/// Run a verdict-bearing PostToolUse hook body under a FAIL-CLOSED panic
/// barrier, then exit.
///
/// * The body returned normally: exit 0. Whatever it already printed (a
///   `{"decision":"block","reason":...}` line, or nothing = no block) stands.
/// * The body panicked — anywhere: a panic is a cannot-determine (CLAUDE.md
///   §3), so it resolves to block, never to the silent exit 0 that the harness
///   reads as "no block". A `{"decision":"block","reason":...}` line naming the
///   crash is written to stdout (the same protocol a PostToolUse gate uses for
///   its own blocks) and the process exits 0 so Claude Code reads that JSON.
///   If that stdout write itself fails (e.g. EPIPE — stdout may be what
///   panicked), the block is carried by stderr + exit 2 instead, which Claude
///   Code also feeds back to the model for PostToolUse.
///
/// Why not [`run_hook`]: it swallows the panic and exits 0 with nothing
/// printed — fail-open for a verdict-bearing hook. Why not
/// [`run_pretooluse_guarded`]: exit 2 there is the PreToolUse block channel;
/// for PostToolUse the established block channel is the stdout JSON, which is
/// only read on exit 0. Why not `crate::gate::run::run_guarded`: that is the
/// Stop protocol with a `stop_hook_active`-bounded allow; a blocked PostToolUse
/// does not trap the session (the tool already ran), so no bounded allow is
/// needed and every panic blocks.
pub fn run_posttooluse_guarded<F: FnOnce()>(gate_name: &str, body: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    std::process::exit(posttooluse_exit_code(
        gate_name,
        outcome,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ))
}

/// Pure core of [`run_posttooluse_guarded`]: 0 when the body completed; when
/// it panicked, the block JSON is written to `out` and 0 is returned, or — if
/// writing/flushing `out` fails — a block message goes to `err` and
/// [`PRETOOLUSE_BLOCK_EXIT`] (2) is returned. A panic never yields a silent 0.
pub fn posttooluse_exit_code<O: std::io::Write, E: std::io::Write>(
    gate_name: &str,
    outcome: std::thread::Result<()>,
    out: &mut O,
    err: &mut E,
) -> i32 {
    let payload = match outcome {
        Ok(()) => return 0,
        Err(payload) => payload,
    };
    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(non-string panic payload)".to_string()
    };
    let reason = format!(
        "{gate_name}: internal error — the PostToolUse gate crashed (panic: {msg}) before it \
         could judge this tool call, so it is BLOCKED as fail-closed (the check did not \
         complete; its result is unknown), not passed"
    );
    let line = serde_json::json!({ "decision": "block", "reason": reason }).to_string();
    if writeln!(out, "{line}").and_then(|()| out.flush()).is_ok() {
        return 0;
    }
    let _ = writeln!(err, "[harness hook panic] {reason}");
    PRETOOLUSE_BLOCK_EXIT
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
///
/// Interpreter-wrapped commands (`bash /abs/guard.sh`, `python3 -u /abs/x.py`)
/// check the *script*, not the interpreter: the first argument after the
/// interpreter that is not a flag. An inline-code form (`bash -c '…'`,
/// `python3 -c …` / `-m mod`) and a relative script path (resolved against
/// the hook's runtime cwd, which a health check cannot know) both yield
/// `None` — nothing concrete to check, same as the unresolved-root case.
///
/// A bare command name (no `/`) is resolved through `$PATH` — never as a path
/// relative to the checker's own cwd, which made the verdict depend on where
/// the health check happened to run. Found → the resolved absolute path;
/// not found → the bare name, which [`binary_present`] reports missing.
///
/// Further shell shapes, each handled so it neither false-flags nor hides the
/// real target: leading `NAME=value` assignments and `env`/`exec`/`nohup`/
/// `command` wrappers are skipped; a shell builtin / keyword head (`cd …`,
/// `if …`) yields `None`; one pair of surrounding quotes is stripped; `~` and
/// `$HOME` are expanded as the shell would, any other `$VAR` yields `None`;
/// a relative path containing `/` yields `None` (cwd-relative at hook time).
///
/// `None` means "could not be checked", not "fine": a caller that needs a
/// verdict uses [`hook_target_status`], which reports it as `Unverifiable`.
pub fn extract_binary_path(command: &str, plugin_root: Option<&str>) -> Option<String> {
    match resolve_command(command, plugin_root) {
        Resolved::Target { path, .. } => Some(path),
        Resolved::Unresolved(_) => None,
    }
}

/// What a hook command resolves to, for the health check.
enum Resolved {
    /// A concrete target: an absolute path, or a bare name not found on
    /// `$PATH`. `direct` is true when the hook execs it itself (so it needs
    /// the exec bit) and false for a script handed to an interpreter.
    Target { path: String, direct: bool },
    /// The checker cannot tell which file the hook runs; the reason says why.
    Unresolved(&'static str),
}

const WHY_EMPTY: &str = "no command to check (empty, or only assignments / wrappers)";
const WHY_BUILTIN: &str = "the command starts with a shell builtin or keyword, not a file";
const WHY_VAR: &str = "contains a variable the checker cannot expand \
     (e.g. ${CLAUDE_PLUGIN_ROOT} outside the hook's runtime env)";
const WHY_INLINE: &str = "inline interpreter code (-c / -m / -e …): the file it runs, if any, \
     cannot be checked";
const WHY_REL_SCRIPT: &str = "relative script after an interpreter: it resolves against the \
     hook's runtime cwd, which the checker cannot know";
const WHY_REL_PATH: &str = "relative command path: it resolves against the hook's runtime cwd, \
     which the checker cannot know";

fn resolve_command(command: &str, plugin_root: Option<&str>) -> Resolved {
    let mut tokens = command.split_whitespace().map(unquote).peekable();
    // Skip assignments and transparent wrappers in front of the real command.
    let first = loop {
        let Some(tok) = tokens.next() else {
            return Resolved::Unresolved(WHY_EMPTY);
        };
        if is_assignment(tok) {
            continue;
        }
        if WRAPPERS.contains(&basename(tok)) {
            while tokens.peek().is_some_and(|t| t.starts_with('-')) {
                tokens.next();
            }
            continue;
        }
        break tok;
    };
    if SHELL_BUILTINS.contains(&first) {
        return Resolved::Unresolved(WHY_BUILTIN);
    }
    let Some(first) = expand_token(first, plugin_root) else {
        return Resolved::Unresolved(WHY_VAR);
    };
    if let Some(kind) = interpreter_kind(&first) {
        let mut prev_was_flag = false;
        for tok in tokens {
            if tok.starts_with('-') {
                if is_inline_code_flag(kind, tok) {
                    return Resolved::Unresolved(WHY_INLINE);
                }
                prev_was_flag = tok != "--";
                continue;
            }
            let Some(arg) = expand_token(tok, plugin_root) else {
                return Resolved::Unresolved(WHY_VAR);
            };
            if Path::new(&arg).is_absolute() {
                return Resolved::Target {
                    path: arg,
                    direct: false,
                };
            }
            if prev_was_flag {
                // Likely the value of an option (`-o pipefail`, `-W ignore`).
                prev_was_flag = false;
                continue;
            }
            return Resolved::Unresolved(WHY_REL_SCRIPT);
        }
        // Bare interpreter with no script argument: check the interpreter.
    }
    if Path::new(&first).is_absolute() {
        return Resolved::Target {
            path: first,
            direct: true,
        };
    }
    if first.contains('/') {
        return Resolved::Unresolved(WHY_REL_PATH);
    }
    Resolved::Target {
        path: resolve_on_path(&first).unwrap_or(first),
        direct: true,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Interp {
    Shell,
    Python,
    Perl,
    Node,
}

/// Interpreter family by basename (`python3.12` counts as Python).
fn interpreter_kind(cmd: &str) -> Option<Interp> {
    let base = basename(cmd);
    match base {
        "bash" | "sh" | "dash" | "zsh" | "ksh" => Some(Interp::Shell),
        "perl" | "ruby" => Some(Interp::Perl),
        "node" => Some(Interp::Node),
        _ => base
            .strip_prefix("python")
            .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .map(|_| Interp::Python),
    }
}

/// True if `flag` makes the interpreter run inline code / a module rather
/// than a script file. Short clusters (`-lc`, `-mjson.tool`) are scanned up
/// to the first option that takes a glued value.
fn is_inline_code_flag(kind: Interp, flag: &str) -> bool {
    if let Some(long) = flag.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or(long);
        return kind == Interp::Node && matches!(name, "eval" | "print");
    }
    let (inline, takes_value): (&[char], &[char]) = match kind {
        Interp::Shell => (&['c'], &['o', 'O']),
        Interp::Python => (&['c', 'm'], &['W', 'X']),
        Interp::Perl => (&['e', 'E'], &['I', 'M', 'm', 'r']),
        Interp::Node => (&['e', 'p'], &['r']),
    };
    for c in flag.chars().skip(1) {
        if inline.contains(&c) {
            return true;
        }
        if takes_value.contains(&c) {
            return false;
        }
    }
    false
}

/// Commands that run their argument as the real command.
const WRAPPERS: &[&str] = &["env", "exec", "nohup", "command"];

/// Builtins / keywords that are not files on `$PATH`.
const SHELL_BUILTINS: &[&str] = &[
    "cd", "export", "source", ".", "set", "unset", "if", "for", "while", "case", "[", "[[", "test",
    "true", "false", ":", "eval", "ulimit", "umask", "trap", "{", "(",
];

fn basename(cmd: &str) -> &str {
    cmd.rsplit('/').next().unwrap_or(cmd)
}

fn is_assignment(tok: &str) -> bool {
    tok.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Strip one pair of surrounding matching quotes.
fn unquote(tok: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = tok.strip_prefix(q).and_then(|t| t.strip_suffix(q)) {
            return inner;
        }
    }
    tok
}

/// Expand `${CLAUDE_PLUGIN_ROOT}` / `$CLAUDE_PLUGIN_ROOT` (only when a root is
/// supplied), `~` and `$HOME`. `None` when the token still holds a `$` the
/// checker cannot resolve — never guessed at, never existence-checked as a
/// literal.
fn expand_token(token: &str, plugin_root: Option<&str>) -> Option<String> {
    let home = || std::env::var("HOME").ok().filter(|h| !h.is_empty());
    let expanded = if let Some(rest) = token
        .strip_prefix("${CLAUDE_PLUGIN_ROOT}")
        .or_else(|| token.strip_prefix("$CLAUDE_PLUGIN_ROOT"))
    {
        format!("{}{rest}", plugin_root?)
    } else if let Some(rest) = token
        .strip_prefix("${HOME}")
        .or_else(|| token.strip_prefix("$HOME"))
    {
        format!("{}{rest}", home()?)
    } else if token == "~" || token.starts_with("~/") {
        format!("{}{}", home()?, &token[1..])
    } else {
        token.to_string()
    };
    (!expanded.contains('$')).then_some(expanded)
}

/// The `$PATH` entry the shell would run for `name`: the first *executable*
/// file named `name`. When only non-executable files match, the first of
/// those is returned so the caller can report it as not executable rather
/// than missing. Pure lookup — spawns nothing. Empty `$PATH` entries are
/// skipped rather than read as the cwd.
fn resolve_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let files: Vec<_> = std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .filter(|cand| cand.is_file())
        .collect();
    files
        .iter()
        .find(|cand| std::fs::metadata(cand).is_ok_and(|m| has_exec_bit(&m)))
        .or_else(|| files.first())
        .map(|cand| cand.to_string_lossy().into_owned())
}

/// True if any exec bit is set (same notion as `check-launcher-exec-bit.py`).
#[cfg(unix)]
fn has_exec_bit(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// No exec bit exists off unix; a regular file is runnable as far as the
/// filesystem says.
#[cfg(not(unix))]
fn has_exec_bit(_meta: &std::fs::Metadata) -> bool {
    true
}

/// False only if `command`'s extracted binary path is concrete and absent —
/// the narrow "missing" bucket of [`missing_hook_binaries`]. A command with no
/// extractable/resolvable path (empty, malformed, an unresolved
/// `${CLAUDE_PLUGIN_ROOT}`, inline interpreter code, or a relative script)
/// yields true here because it is not *missing*; that is NOT a claim that it
/// was checked — [`hook_target_status`] reports it as `Unverifiable`. A bare
/// name (no `/`) reaching here was not found on `$PATH`, so it is missing —
/// it is never existence-checked against the checker's cwd.
fn binary_present(command: &str, plugin_root: Option<&str>) -> bool {
    match extract_binary_path(command, plugin_root) {
        Some(path) if path.contains('/') => Path::new(&path).exists(),
        Some(_) => false,
        None => true,
    }
}

/// Scan a parsed `settings.json` (as produced by [`crate::install::load_settings`])
/// for hook groups whose command's binary path does not exist, and return them.
/// `plugin_root`, when supplied, is substituted for a literal
/// `${CLAUDE_PLUGIN_ROOT}` prefix in a command (health-check runs happen
/// outside the env Claude Code sets, so callers that know the intended root —
/// e.g. checking one plugin's own install — can pass it; a caller scanning the
/// whole file across plugins generally can't, and those entries are skipped
/// here rather than false-flagged). Never panics: a malformed/non-object
/// `settings` or a hook group missing expected fields is simply skipped.
///
/// This lists only the *missing* bucket. An empty result does NOT mean every
/// hook is healthy: unresolvable targets, non-executable targets and
/// malformed entries are not in it. A caller that renders a health verdict
/// must use [`hook_target_findings`].
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

/// Health of one hook command's target. Only `Present` is healthy: an
/// `Unverifiable` target was *not checked* and must never be summarised as
/// present (CLAUDE.md §3 — undetermined is not clean).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HookTargetStatus {
    /// The target exists and (when the hook execs it directly) is executable.
    Present,
    /// The target path (or bare name not on `$PATH`) does not exist.
    Missing { binary_path: String },
    /// The target exists but cannot be run: a direct-exec target without any
    /// exec bit, or a directory. A script handed to an interpreter
    /// (`bash /abs/x.sh`) does not need the exec bit.
    NotExecutable { binary_path: String },
    /// The checker cannot tell which file the hook runs, or cannot stat it.
    Unverifiable { reason: String },
}

/// A registered hook whose target is not [`HookTargetStatus::Present`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookTargetFinding {
    /// The `hooks.<event>` key (or `hooks` for a malformed top level).
    pub event: String,
    /// The full `command` string as written in `settings.json` (empty when
    /// the entry is malformed and has none).
    pub command: String,
    pub status: HookTargetStatus,
}

/// Classify one hook `command`'s target. See [`extract_binary_path`] for how
/// the target is found; every shape it cannot resolve is `Unverifiable` with
/// the reason, never `Present`.
pub fn hook_target_status(command: &str, plugin_root: Option<&str>) -> HookTargetStatus {
    let (path, direct) = match resolve_command(command, plugin_root) {
        Resolved::Unresolved(why) => {
            return HookTargetStatus::Unverifiable {
                reason: why.to_string(),
            }
        }
        Resolved::Target { path, direct } => (path, direct),
    };
    if !path.contains('/') {
        // A bare name that `$PATH` lookup did not find.
        return HookTargetStatus::Missing { binary_path: path };
    }
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            HookTargetStatus::Missing { binary_path: path }
        }
        Err(e) => HookTargetStatus::Unverifiable {
            reason: format!("cannot stat {path}: {e}"),
        },
        Ok(m) if m.is_dir() || (direct && !has_exec_bit(&m)) => {
            HookTargetStatus::NotExecutable { binary_path: path }
        }
        Ok(_) => HookTargetStatus::Present,
    }
}

/// Scan a parsed `settings.json` and return every registered command hook
/// whose target is not `Present` — missing, not executable, or unverifiable
/// — in settings order. A malformed `hooks` structure (non-object `hooks`,
/// non-array event, group without a `hooks` array, a command hook without a
/// command string) is itself reported as `Unverifiable`: the hooks it should
/// hold could not be checked. An empty result means every command hook was
/// checked and is present. Non-command hooks (`"type": "prompt"` …) have no
/// target and are skipped. A `settings` without a `hooks` key has no hooks.
pub fn hook_target_findings(settings: &Value, plugin_root: Option<&str>) -> Vec<HookTargetFinding> {
    let mut out = Vec::new();
    let malformed = |event: &str, why: &str| HookTargetFinding {
        event: event.to_string(),
        command: String::new(),
        status: HookTargetStatus::Unverifiable {
            reason: format!("malformed settings: {why}"),
        },
    };
    let Some(hooks) = settings.get("hooks") else {
        if !settings.is_object() {
            out.push(malformed("hooks", "settings is not a JSON object"));
        }
        return out;
    };
    let Some(events) = hooks.as_object() else {
        out.push(malformed("hooks", "`hooks` is not an object"));
        return out;
    };
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else {
            out.push(malformed(event, "event value is not an array"));
            continue;
        };
        for group in groups {
            let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
                out.push(malformed(event, "hook group has no `hooks` array"));
                continue;
            };
            for h in hooks {
                let kind = h.get("type").and_then(Value::as_str).unwrap_or("command");
                if kind != "command" {
                    continue;
                }
                let Some(command) = h.get("command").and_then(Value::as_str) else {
                    out.push(malformed(event, "command hook has no `command` string"));
                    continue;
                };
                let status = hook_target_status(command, plugin_root);
                if status != HookTargetStatus::Present {
                    out.push(HookTargetFinding {
                        event: event.clone(),
                        command: command.to_string(),
                        status,
                    });
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
    fn pretooluse_exit_code_is_zero_when_body_completes() {
        let mut err = Vec::new();
        let outcome = std::panic::catch_unwind(|| {});
        assert_eq!(pretooluse_exit_code("g", outcome, &mut err), 0);
        assert!(err.is_empty(), "a clean run writes nothing to stderr");
    }

    #[test]
    fn pretooluse_exit_code_blocks_on_every_panic_payload_shape() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let str_panic = std::panic::catch_unwind(|| panic!("static str boom"));
        let string_panic = std::panic::catch_unwind(|| panic!("formatted {}", 42));
        // The EPIPE shape: `println!` on a closed stdout panics with this text.
        let print_panic = std::panic::catch_unwind(|| {
            panic!("failed printing to stdout: Broken pipe (os error 32)")
        });
        let other_panic = std::panic::catch_unwind(|| std::panic::panic_any(7_u8));
        std::panic::set_hook(prev);

        for (outcome, needle) in [
            (str_panic, "static str boom"),
            (string_panic, "formatted 42"),
            (print_panic, "failed printing to stdout"),
            (other_panic, "(non-string panic payload)"),
        ] {
            let mut err = Vec::new();
            assert_eq!(
                pretooluse_exit_code("ctxrot preguard", outcome, &mut err),
                PRETOOLUSE_BLOCK_EXIT,
                "a panic on the PreToolUse verdict path must exit 2 (block), never 0 (allow)"
            );
            let text = String::from_utf8(err).unwrap();
            assert!(text.contains("ctxrot preguard"), "names the gate: {text}");
            assert!(text.contains(needle), "carries the panic message: {text}");
            assert!(text.contains("BLOCKED"), "states the consequence: {text}");
        }
        assert_eq!(PRETOOLUSE_BLOCK_EXIT, 2);
    }

    #[test]
    fn pretooluse_exit_code_still_blocks_when_stderr_is_unwritable() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(|| panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(
            pretooluse_exit_code("g", outcome, &mut Broken),
            PRETOOLUSE_BLOCK_EXIT
        );
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
