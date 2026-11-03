//! `sitecraft gate` — the Stop hook.
//!
//! Runs through `harness_core::gate::run::run_guarded` (gate name
//! `sitecraft`; a panic blocks). In a repo with `sitecraft.toml` it combines,
//! with [`Verdict::worst_of`]: trust of the config, `decide check`,
//! `check security`, and — only when the session wrote a file matching
//! `web_globs` (a Write/Edit to one, or a Bash command naming one), or when
//! that cannot be determined (including any Bash command not provably
//! read-only from its command line: an allowlisted command with literal,
//! allowlisted arguments; see `bash_change`) — `serve start` +
//! `check pages` + `serve stop`. A repo without `sitecraft.toml` is allowed
//! and a `not-configured` line is appended to the state dir. Give-up is
//! bounded (`max_attempts`) and says NOTHING WAS VERIFIED when it allows.
//!
//! Stdin handling: empty stdin is a manual run (verdict on stderr, exit code
//! per [`crate::exit_code`], no give-up counter); non-empty stdin that is not
//! a Stop payload is judged `Undetermined` in hook mode (a block), never a
//! manual run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_core::gate::state;
use harness_core::hook::HookInput;

use crate::check;
use crate::config::{self, Config, Loaded};
use crate::decide::record;
use crate::serve;
use crate::trust::{self, Trust};
use crate::{Determination, Verdict};

/// Gate name passed to the panic barrier (its per-gate panic-streak marker).
pub const GATE_NAME: &str = "sitecraft";

/// File (under the state dir) the not-configured records are appended to.
pub const LOG_FILE: &str = "gate-log.jsonl";

/// Sub-directory of the state dir holding the per-session attempt counters.
const ATTEMPTS_DIR: &str = "gate-attempts";

/// Built-in give-up budget `(max_attempts, reset_after_secs)`, used whenever
/// the config is not trusted (an unapproved file cannot set its own budget).
const DEFAULT_BUDGET: (u32, i64) = (3, 1800);
/// Clamp for a trusted `max_attempts` (a floor keeps the gate from being
/// disabled; a ceiling keeps the give-up bounded).
const MIN_ATTEMPTS: u32 = 1;
const MAX_ATTEMPTS: u32 = 10;

/// Tools whose `file_path` / `notebook_path` input writes a file.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];
/// Tools that run a sub-agent: its Write/Edit calls are recorded in another
/// transcript, so web changes cannot be ruled out from this one.
const SUBAGENT_TOOLS: [&str; 2] = ["Task", "Agent"];
/// The shell tool: its writes are only visible as a command string.
const BASH_TOOL: &str = "Bash";

/// Evaluate the gate for one Stop payload (the testable core; no stdout, no
/// exit, no give-up counter).
///
/// Contract: no `sitecraft.toml` at the project root → `Clean` (recorded as
/// not-configured); otherwise `Clean` only when every applicable check is
/// `Clean`, `Violation` when one found something, and `Undetermined` when
/// any could not conclude (untrusted/unreadable config included). An
/// unreadable or missing transcript does not make the result `Undetermined`
/// by itself: it means "web files may have changed", so pages are checked.
pub fn run(input: &HookInput) -> Verdict {
    match state_dir() {
        Ok(state) => evaluate(input, &state).0,
        Err(v) => v,
    }
}

/// The whole `sitecraft gate` subcommand: read the Stop payload from stdin,
/// evaluate under the panic barrier, print the Stop-hook decision, and
/// return the process exit code.
pub fn command() -> i32 {
    let raw = harness_core::hook::read_stdin();
    if raw.trim().is_empty() {
        // Manual run: no payload, no session; report like any other check.
        return harness_core::gate::run::run_guarded(GATE_NAME, true, false, || {
            let v = run(&HookInput::default());
            crate::report("sitecraft gate", &v)
        });
    }
    match HookInput::parse(&raw) {
        Some(input) => {
            let active = input.stop_hook_active;
            harness_core::gate::run::run_guarded(GATE_NAME, false, active, move || hook_run(&input))
        }
        None => harness_core::gate::run::run_guarded(GATE_NAME, false, false, || {
            let session = std::env::var("CLAUDE_CODE_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "_local".to_string());
            let v = Verdict::undetermined(format!(
                "stdin is not a Stop-hook payload (expected a JSON object, got {} bytes that do not parse), so nothing was checked",
                raw.len()
            ));
            let state = state_dir().ok();
            finish(&v, DEFAULT_BUDGET, state.as_deref(), &session)
        }),
    }
}

/// Hook mode: evaluate, then apply the bounded give-up and print the decision.
fn hook_run(input: &HookInput) -> i32 {
    let session = input.session_key();
    let state = match state_dir() {
        Ok(s) => s,
        Err(v) => return finish(&v, DEFAULT_BUDGET, None, &session),
    };
    let (v, budget) = evaluate(input, &state);
    finish(&v, budget, Some(&state), &session)
}

fn state_dir() -> Result<PathBuf, Verdict> {
    match crate::state_dir() {
        Determination::Known(d) => Ok(d),
        Determination::Undetermined(why) => Err(Verdict::undetermined(why.as_str())),
    }
}

/// The verdict and the give-up budget that applies to it.
fn evaluate(input: &HookInput, state: &Path) -> (Verdict, (u32, i64)) {
    let cwd = input.cwd_or_current();
    let root = match project_root(&cwd) {
        Ok(r) => r,
        Err(why) => return (Verdict::undetermined(why), DEFAULT_BUDGET),
    };
    let cfg = match config::load(&root) {
        Loaded::Absent => return (not_configured(state, &root, input), DEFAULT_BUDGET),
        Loaded::Undetermined(why) => {
            return (
                Verdict::undetermined(format!(
                    "{why}; fix {} and run `sitecraft trust`",
                    config::FILE_NAME
                )),
                DEFAULT_BUDGET,
            )
        }
        Loaded::Config(cfg, bytes) => {
            match trust::check_bytes(state, &root, &config::path_for(&root), &bytes) {
                Trust::Trusted => cfg,
                // Nothing from an unapproved file is honoured, including its
                // give-up budget: the built-in default bounds this block.
                Trust::Untrusted(why) => {
                    return (
                        Verdict::undetermined(format!("untrusted config: {why}")),
                        DEFAULT_BUDGET,
                    )
                }
                Trust::Undetermined(why) => {
                    return (
                        Verdict::undetermined(format!(
                            "untrusted config (trust cannot be established): {why}; fix it, review {} and run `sitecraft trust`",
                            config::FILE_NAME
                        )),
                        DEFAULT_BUDGET,
                    )
                }
            }
        }
    };
    let budget = (
        cfg.max_attempts.clamp(MIN_ATTEMPTS, MAX_ATTEMPTS),
        cfg.reset_after_secs,
    );
    let decide = annotate(
        "decide check",
        record::check_root(&root),
        "ask the user and record each answer: `sitecraft decide next` prints the next question, `sitecraft decide record --area <area> --question <id> --answer <value>` records it",
    );
    let security = annotate(
        "check security",
        check::security::check_security(&cfg, &root),
        "fix each finding listed above, then stop again",
    );
    let pages = match web_changes(&input.transcript_path, &root, &cfg) {
        Determination::Known(false) => None,
        Determination::Known(true) => Some(pages_with_server(state, &root)),
        Determination::Undetermined(why) => {
            eprintln!(
                "sitecraft gate: web changes cannot be ruled out ({}); checking pages",
                why.as_str()
            );
            Some(pages_with_server(state, &root))
        }
    };
    let v = Verdict::worst_of([decide, security].into_iter().chain(pages));
    (v, budget)
}

/// Prefix a check's non-clean reason with its name and a fix hint. `Clean`
/// stays `Clean`; no arm changes the verdict's kind.
fn annotate(label: &str, v: Verdict, fix: &str) -> Verdict {
    match v {
        Verdict::Clean(_) => v,
        Verdict::Violation(r) => Verdict::violation(format!("{label}: {}; fix: {fix}", r.as_str())),
        Verdict::Undetermined(u) => {
            Verdict::undetermined(format!("{label}: undetermined: {}; fix: {fix}", u.as_str()))
        }
    }
}

/// The git top level of `cwd`; outside any repository the cwd itself. Any
/// other git failure is an error (the root, and so the config, is unknown).
fn project_root(cwd: &Path) -> Result<PathBuf, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| {
            format!(
                "cannot run git to find the project root of {}: {e}",
                cwd.display()
            )
        })?;
    if out.status.success() {
        let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if top.is_empty() {
            return Err(format!(
                "git rev-parse --show-toplevel printed nothing for {}",
                cwd.display()
            ));
        }
        return Ok(PathBuf::from(top));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("not a git repository") {
        return Ok(cwd.to_path_buf());
    }
    Err(format!(
        "cannot resolve the project root of {}: git rev-parse failed ({}): {}",
        cwd.display(),
        out.status,
        err.trim()
    ))
}

/// Allow (the project never opted in) and append one `not-configured` record.
fn not_configured(state: &Path, root: &Path, input: &HookInput) -> Verdict {
    let entry = serde_json::json!({
        "event": "not-configured",
        "root": root.display().to_string(),
        "session": input.session_key(),
        "ts": now_secs(),
    });
    if let Err(e) = append_line(&state.join(LOG_FILE), &entry.to_string()) {
        eprintln!("sitecraft gate: not-configured record not written: {e}");
    }
    eprintln!(
        "sitecraft gate: not configured ({} not found in {}); nothing to check",
        config::FILE_NAME,
        root.display()
    );
    Verdict::from_findings(Vec::new())
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    f.write_all(format!("{line}\n").as_bytes())
        .map_err(|e| format!("cannot append to {}: {e}", path.display()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// web changes in the transcript
// ---------------------------------------------------------------------------

fn web_globset(cfg: &Config) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in &cfg.web_globs {
        b.add(Glob::new(g).map_err(|e| format!("web glob {g:?} does not compile: {e}"))?);
    }
    b.build()
        .map_err(|e| format!("cannot build web_globs: {e}"))
}

/// Did this session write a file matching `web_globs`?
///
/// Contract: `Known(true)` when a Write/Edit/MultiEdit/NotebookEdit tool call
/// targets such a file, or a Bash command mentions one (see [`bash_change`]);
/// `Known(false)` only when the whole transcript was read and parsed, no such
/// call exists, and every Bash command was proven unable to write a file;
/// `Undetermined` when there is no transcript path, it cannot be read, it is
/// empty, any line does not parse, a write call has no usable path, a Bash
/// call has no command string or a command that is not provably read-only,
/// or a sub-agent ran (its edits are not in this transcript).
fn web_changes(transcript_path: &str, root: &Path, cfg: &Config) -> Determination<bool> {
    if transcript_path.is_empty() {
        return Determination::undetermined("the Stop payload has no transcript_path");
    }
    let globs = match web_globset(cfg) {
        Ok(g) => g,
        Err(e) => return Determination::undetermined(e),
    };
    let text = match std::fs::read(transcript_path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot read transcript {transcript_path}: {e}"
            ))
        }
    };
    let canon_root = std::fs::canonicalize(root).ok();
    let mut lines = 0usize;
    let mut subagent = false;
    let mut bash_unknown: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        lines += 1;
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Determination::undetermined(format!(
                "transcript {transcript_path} line {} does not parse ({e}); it may hide a web edit",
                i + 1
            ))
            }
        };
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if SUBAGENT_TOOLS.contains(&name) {
                subagent = true;
                continue;
            }
            if name == BASH_TOOL {
                let command = item
                    .get("input")
                    .and_then(|i| i.get("command"))
                    .and_then(|c| c.as_str());
                let Some(command) = command else {
                    return Determination::undetermined(format!(
                        "transcript {transcript_path} line {}: a Bash call has no command string",
                        i + 1
                    ));
                };
                match bash_change(command, root, canon_root.as_deref(), &globs) {
                    Determination::Known(true) => return Determination::known(true),
                    Determination::Known(false) => {}
                    Determination::Undetermined(why) => {
                        if bash_unknown.is_none() {
                            bash_unknown = Some(format!(
                                "transcript {transcript_path} line {}: {}",
                                i + 1,
                                why.as_str()
                            ));
                        }
                    }
                }
                continue;
            }
            if !WRITE_TOOLS.contains(&name) {
                continue;
            }
            let input = item.get("input");
            let path = input
                .and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path")))
                .and_then(|p| p.as_str());
            let Some(path) = path else {
                return Determination::undetermined(format!(
                    "transcript {transcript_path} line {}: a {name} call has no file path",
                    i + 1
                ));
            };
            if is_web_file(path, root, canon_root.as_deref(), &globs) {
                return Determination::known(true);
            }
        }
    }
    if lines == 0 {
        return Determination::undetermined(format!("transcript {transcript_path} is empty"));
    }
    if subagent {
        return Determination::undetermined(
            "a sub-agent ran in this session and its edits are not in this transcript",
        );
    }
    if let Some(why) = bash_unknown {
        return Determination::undetermined(why);
    }
    Determination::known(false)
}

/// Commands that neither write a file nor run another program when given
/// literal arguments. Two conditions apply on top of the name (see
/// [`simple_command_is_read_only`]): no word of the command may be expanded
/// by the shell, and `rg` may only be given the options in [`RG_FLAGS`].
/// `git` is not listed here: only the subcommands in [`READ_ONLY_GIT`] with
/// the options of [`git_flags`] are cleared. Anything not listed — `sed`,
/// `tee`, `cp`, `mv`, `find`, `sort`, `npm`, scripts, … — is treated as
/// possibly writing.
const READ_ONLY_COMMANDS: [&str; 23] = [
    "ls", "cat", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg", "pwd", "echo", "printf",
    "which", "stat", "true", "false", "test", "cd", "basename", "dirname", "realpath", "diff",
    "cmp",
];
/// `git` subcommands that do not write the work tree; each has an option
/// allowlist in [`git_flags`].
const READ_ONLY_GIT: [&str; 4] = ["status", "log", "rev-parse", "ls-files"];

/// Can this Bash `command` have written a file matching `web_globs`?
///
/// Contract (restrictive by construction — a command is only cleared when it
/// is proven unable to write):
/// - `Known(true)` when any word (or the value after a word's first `=`)
///   names a path matching `web_globs` — any mention counts as a change;
/// - `Known(false)` only when the command lexes completely and every simple
///   command in it is in [`READ_ONLY_COMMANDS`] (or `git`, optionally after
///   `--no-pager`, with a [`READ_ONLY_GIT`] subcommand), no word of it is
///   expanded by the shell (`$`, `*`, `?`, `[`, `{` outside single quotes),
///   every option given to `rg` / `git` is on its allowlist ([`RG_FLAGS`],
///   [`git_flags`]; a `git log` argument containing `%G` is rejected) —
///   options are parsed value-aware ([`check_flags`]), so a `--` that is an
///   option's value (`rg -e -- --pre=PROG`) does not end the check — and
///   there is no output redirect other than to `/dev/null` or an fd
///   duplicate;
/// - `Undetermined` otherwise: command / process substitution, backticks,
///   ANSI-C quoting (`$'...'`), heredocs, unterminated quotes, subshells,
///   variable assignments, an expanded word anywhere in argv (the command
///   name included), an unlisted command / git subcommand / option, a git
///   global option other than `--no-pager`, or a file-writing redirect.
///
/// Only the command line is judged. Configuration the commands read at run
/// time — `RIPGREP_CONFIG_PATH` (which can hold `--pre`), git config such as
/// `core.fsmonitor`, filters or `core.pager` — is not inspected: a read-only
/// command line is cleared even if such configuration would make it run a
/// program.
fn bash_change(
    command: &str,
    root: &Path,
    canon_root: Option<&Path>,
    globs: &GlobSet,
) -> Determination<bool> {
    let tokens = match lex_shell(command) {
        Ok(t) => t,
        Err(why) => {
            return Determination::undetermined(format!(
                "a Bash command cannot be analysed ({why}), so it may have written a web file: {command:?}"
            ))
        }
    };
    let mentions_web = |s: &str| {
        is_web_file(s, root, canon_root, globs)
            || s.split_once('=')
                .is_some_and(|(_, v)| !v.is_empty() && is_web_file(v, root, canon_root, globs))
    };
    if tokens.iter().any(|t| match t {
        Tok::Word { text, .. } => mentions_web(text),
        _ => false,
    }) {
        return Determination::known(true);
    }
    for cmd in tokens.split(|t| matches!(t, Tok::Sep)) {
        if let Err(why) = simple_command_is_read_only(cmd) {
            return Determination::undetermined(format!(
                "a Bash command may have written a web file ({why}): {command:?}"
            ));
        }
    }
    Determination::known(false)
}

/// One simple command (no separators) is provably unable to write a file.
fn simple_command_is_read_only(cmd: &[Tok]) -> Result<(), String> {
    let mut words: Vec<(&str, bool)> = Vec::new();
    let mut iter = cmd.iter();
    while let Some(t) = iter.next() {
        match t {
            Tok::Sep => {}
            Tok::Group => return Err("it uses a subshell or group".into()),
            Tok::Word { text, dynamic } => words.push((text.as_str(), *dynamic)),
            Tok::Redir(kind) => {
                let target = match iter.next() {
                    Some(Tok::Word { text, dynamic }) => (text.as_str(), *dynamic),
                    _ => return Err("a redirect has no target".into()),
                };
                match kind {
                    RedirKind::Read => {}
                    RedirKind::Write => {
                        if target.1 || target.0 != "/dev/null" {
                            return Err(format!("it redirects output into {:?}", target.0));
                        }
                    }
                    RedirKind::Dup => {
                        let fd_like = target.0 == "-"
                            || (!target.0.is_empty()
                                && target.0.bytes().all(|b| b.is_ascii_digit()));
                        if target.1 || !fd_like {
                            return Err(format!("it redirects output into {:?}", target.0));
                        }
                    }
                }
            }
        }
    }
    let Some(&(name, _)) = words.first() else {
        return Ok(());
    };
    // Any expansion in argv (the command name included): the words the
    // program receives are not the words written (`rg $F` may be
    // `rg --pre=prog`, `git log $X` may be `git log --output=<web file>`).
    if let Some(&(w, _)) = words.iter().find(|w| w.1) {
        return Err(format!(
            "the word {w:?} is expanded by the shell, so the argv is not known"
        ));
    }
    if name.contains('=') {
        return Err(format!("it sets a variable ({name:?})"));
    }
    let mut args: Vec<&str> = words[1..].iter().map(|w| w.0).collect();
    if name == "git" {
        // Global options are not allowed (`-c`, `--exec-path`, `--git-dir`,
        // `--work-tree`, `--config-env`, … change what git runs or where),
        // except the output-only `--no-pager`.
        while args.first() == Some(&"--no-pager") {
            args.remove(0);
        }
        let sub = args.first().copied().unwrap_or("");
        let Some(spec) = git_flags(sub) else {
            return Err(format!("`git {sub}` is not a known read-only git command"));
        };
        let opts = &args[1..];
        if sub == "log" {
            if let Some(a) = opts.iter().find(|a| a.contains("%G")) {
                return Err(format!(
                    "git log format {a:?} verifies signatures (runs gpg)"
                ));
            }
        }
        return check_flags(&format!("git {sub}"), opts, &spec);
    }
    if !READ_ONLY_COMMANDS.contains(&name) {
        return Err(format!("`{name}` is not a known read-only command"));
    }
    if name == "rg" {
        return check_flags("rg", &args, &RG_FLAGS);
    }
    Ok(())
}

/// The options an allowlisted command may be given. Anything else that looks
/// like an option makes the command not provably read-only.
struct FlagSpec {
    /// Long options without a value (`--name`, exact match).
    long_bool: &'static [&'static str],
    /// Long options with a required value (`--name=VALUE` or `--name VALUE`:
    /// without `=`, the next word is the value whatever it is, `--`
    /// included).
    long_val: &'static [&'static str],
    /// Long options with an optional value, which is only ever attached
    /// (`--name` or `--name=VALUE`; the next word is never the value).
    long_opt: &'static [&'static str],
    /// Short options without a value; they may be clustered (`-abc`).
    short_bool: &'static str,
    /// Short options with a required value; the rest of the cluster (`-eX`,
    /// `-ieX`) is that value, or, when the option ends the cluster (`-e`,
    /// `-ie`), the next word is, whatever it is (`--` included).
    short_val: &'static str,
    /// Short options with an optional value, which is only ever the rest of
    /// the cluster (`-u`, `-uno`; the next word is never the value).
    short_opt: &'static str,
    /// `-<digits>` (git log's `-5`) is accepted.
    digits: bool,
}

/// Check every option-looking word of `args` against `spec`.
///
/// Parsing is value-aware: an option of [`FlagSpec::long_val`] written
/// without `=`, or a cluster ending in a [`FlagSpec::short_val`] option,
/// consumes the next word as its value, so a `--` in that position is a value
/// (rg reads `rg -e -- --pre=PROG` as pattern `--` plus option `--pre`) and
/// does not end option parsing. Any other `--` word ends it (every later word
/// is an operand). Each allowlisted option is classified as taking no value,
/// a required value or an optional attached value; an option on no list is
/// rejected (whether it takes a value is then not known either).
///
/// A word is option-looking when it starts with `-` and is not `-` itself.
/// A consumed value that starts with `-` (other than `--` / `-`) is checked
/// too, as if it were an option (a pattern such as `-e -x` is then rejected:
/// conservative, never permissive); it does not itself consume a value.
/// Operands are not options and are not restricted here; a web-file operand
/// is caught by [`bash_change`]'s mention check.
fn check_flags(cmd: &str, args: &[&str], spec: &FlagSpec) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        i += 1;
        if a == "--" {
            break;
        }
        if check_option(cmd, a, spec)? {
            if let Some(&value) = args.get(i) {
                i += 1;
                if value != "--" {
                    check_option(cmd, value, spec)?;
                }
            }
        }
    }
    Ok(())
}

/// Check one word against `spec`: `Err` when it is an option not on the
/// allowlist, else whether the NEXT word is this option's value.
fn check_option(cmd: &str, a: &str, spec: &FlagSpec) -> Result<bool, String> {
    let reject = || -> Result<bool, String> {
        Err(format!(
            "`{cmd}` option {a:?} is not on its read-only allowlist"
        ))
    };
    if let Some(long) = a.strip_prefix("--") {
        return match long.split_once('=') {
            Some((name, _)) => {
                if spec.long_val.contains(&name) || spec.long_opt.contains(&name) {
                    Ok(false)
                } else {
                    reject()
                }
            }
            None => {
                if spec.long_val.contains(&long) {
                    Ok(true)
                } else if spec.long_bool.contains(&long) || spec.long_opt.contains(&long) {
                    Ok(false)
                } else {
                    reject()
                }
            }
        };
    }
    let Some(short) = a.strip_prefix('-') else {
        return Ok(false);
    };
    if short.is_empty() {
        return Ok(false);
    }
    if spec.digits && short.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(false);
    }
    for (at, c) in short.char_indices() {
        if spec.short_val.contains(c) {
            // The value is the rest of the cluster, or the next word.
            return Ok(at + c.len_utf8() == short.len());
        }
        if spec.short_opt.contains(c) {
            // The value, if any, is the rest of the cluster.
            return Ok(false);
        }
        if !spec.short_bool.contains(c) {
            return reject();
        }
    }
    Ok(false)
}

/// ripgrep options that neither run a program nor write a file, taken from
/// the installed rg's `--help` (ripgrep 14). Left out on purpose:
/// `--pre` / `--pre-glob` (run a preprocessor on every file),
/// `--hostname-bin` (runs a program to get the hostname for hyperlinks) and
/// `-z` / `--search-zip` (runs decompression programs found on `PATH`).
/// rg never writes files; it is an allowlist so that an option this list
/// does not know (a newer rg, an abbreviation) is not cleared.
const RG_FLAGS: FlagSpec = FlagSpec {
    long_bool: &[
        "case-sensitive",
        "crlf",
        "fixed-strings",
        "ignore-case",
        "invert-match",
        "line-regexp",
        "mmap",
        "multiline",
        "multiline-dotall",
        "no-unicode",
        "null-data",
        "pcre2",
        "smart-case",
        "stop-on-nonmatch",
        "text",
        "word-regexp",
        "auto-hybrid-regex",
        "no-pcre2-unicode",
        "binary",
        "follow",
        "glob-case-insensitive",
        "hidden",
        "ignore-file-case-insensitive",
        "no-ignore",
        "no-ignore-dot",
        "no-ignore-exclude",
        "no-ignore-files",
        "no-ignore-global",
        "no-ignore-parent",
        "no-ignore-vcs",
        "no-require-git",
        "one-file-system",
        "unrestricted",
        "block-buffered",
        "byte-offset",
        "column",
        "heading",
        "no-heading",
        "help",
        "include-zero",
        "line-buffered",
        "line-number",
        "no-line-number",
        "max-columns-preview",
        "null",
        "only-matching",
        "passthru",
        "pretty",
        "quiet",
        "trim",
        "vimgrep",
        "with-filename",
        "no-filename",
        "sort-files",
        "count",
        "count-matches",
        "files-with-matches",
        "files-without-match",
        "json",
        "debug",
        "no-ignore-messages",
        "no-messages",
        "stats",
        "trace",
        "files",
        "no-config",
        "no-pre",
        "no-search-zip",
        "pcre2-version",
        "type-list",
        "version",
    ],
    long_val: &[
        "regexp",
        "file",
        "dfa-size-limit",
        "encoding",
        "engine",
        "max-count",
        "regex-size-limit",
        "threads",
        "glob",
        "iglob",
        "ignore-file",
        "max-depth",
        "max-filesize",
        "type",
        "type-not",
        "type-add",
        "type-clear",
        "after-context",
        "before-context",
        "color",
        "colors",
        "context",
        "context-separator",
        "field-context-separator",
        "field-match-separator",
        "hyperlink-format",
        "max-columns",
        "path-separator",
        "replace",
        "sort",
        "sortr",
        "generate",
    ],
    // rg has no option with an optional value: every value above is
    // required (`-e PATTERN, --regexp=PATTERN` in `rg --help`), and rg takes
    // the next word as it whatever it is.
    long_opt: &[],
    short_bool: "sivxUFPSawL.ubnN0opqHIclhV",
    short_val: "efEmjgdtTABCMr",
    short_opt: "",
    digits: false,
};

/// The option allowlist for a read-only `git` subcommand ([`READ_ONLY_GIT`]),
/// or `None` for any other subcommand. Allowlists, not denylists: git
/// accepts unambiguous abbreviations of long options (`--out` for
/// `--output`), so a denylist cannot be complete. Left out on purpose:
/// `--output`, `--ext-diff`, `--textconv` and anything that prints a diff
/// (`-p`, `--stat`, `-v`: diff drivers / textconv run programs),
/// `--show-signature` (runs gpg), and every option that takes a file.
fn git_flags(sub: &str) -> Option<FlagSpec> {
    if !READ_ONLY_GIT.contains(&sub) {
        return None;
    }
    let spec = match sub {
        "status" => FlagSpec {
            long_bool: &[
                "short",
                "no-short",
                "branch",
                "no-branch",
                "show-stash",
                "ahead-behind",
                "no-ahead-behind",
                "long",
                "null",
                "no-renames",
                "renames",
            ],
            // `git status -h`: `--porcelain[=<version>]`,
            // `-u, --untracked-files[=<mode>]`, `--ignored[=<mode>]` — all
            // optional, attached only.
            long_val: &[],
            long_opt: &["porcelain", "untracked-files", "ignored"],
            short_bool: "sbz",
            short_val: "",
            short_opt: "u",
            digits: false,
        },
        "log" => FlagSpec {
            long_bool: &[
                "oneline",
                "graph",
                "decorate",
                "no-decorate",
                "all",
                "branches",
                "tags",
                "remotes",
                "reverse",
                "abbrev-commit",
                "no-abbrev-commit",
                "no-merges",
                "merges",
                "first-parent",
                "name-only",
                "name-status",
                "topo-order",
                "date-order",
                "no-color",
            ],
            // Revision options (git's revision.c): `--max-count`, `--author`,
            // `--grep`, … and `-n` take the next word when not attached;
            // `--format=` / `--pretty[=]` / `--abbrev[=]` are attached only.
            // (git log also ends options at the first `--` word anywhere, so
            // consuming a `--` as a value only ever checks more words.)
            long_val: &[
                "max-count",
                "skip",
                "since",
                "until",
                "after",
                "before",
                "author",
                "committer",
                "grep",
                "date",
            ],
            long_opt: &["format", "pretty", "abbrev"],
            short_bool: "",
            short_val: "n",
            short_opt: "",
            digits: true,
        },
        "rev-parse" => FlagSpec {
            long_bool: &[
                "show-toplevel",
                "show-prefix",
                "show-cdup",
                "git-dir",
                "git-common-dir",
                "absolute-git-dir",
                "is-inside-work-tree",
                "is-inside-git-dir",
                "is-bare-repository",
                "is-shallow-repository",
                "symbolic-full-name",
                "verify",
                "quiet",
            ],
            // `--abbrev-ref[=<mode>]`, `--short[=<n>]`: attached only.
            long_val: &[],
            long_opt: &["abbrev-ref", "short"],
            short_bool: "q",
            short_val: "",
            short_opt: "",
            digits: false,
        },
        "ls-files" => FlagSpec {
            long_bool: &[
                "cached",
                "deleted",
                "modified",
                "others",
                "ignored",
                "stage",
                "killed",
                "directory",
                "eol",
                "unmerged",
                "exclude-standard",
                "full-name",
                "error-unmatch",
                "deduplicate",
            ],
            // `git ls-files -h`: `-x, --exclude <pattern>`,
            // `--format <format>` (required), `--abbrev[=<n>]` (optional).
            long_val: &["exclude", "format"],
            long_opt: &["abbrev"],
            short_bool: "zcdmoisktuvf",
            short_val: "x",
            short_opt: "",
            digits: false,
        },
        _ => return None,
    };
    Some(spec)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirKind {
    /// `<` (reads its target).
    Read,
    /// `>`, `>>`, `>|`, `&>`, `<>` (writes its target).
    Write,
    /// `>&`, `<&` (duplicates an fd; a non-numeric target is a file write).
    Dup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A word after quote removal; `dynamic` when bash may expand it into
    /// something other than `text` (see [`lex_shell`]).
    Word {
        text: String,
        dynamic: bool,
    },
    /// `;`, `&`, `&&`, `|`, `||`, `|&`, newline.
    Sep,
    /// `(` or `)`.
    Group,
    Redir(RedirKind),
}

/// A minimal POSIX-shell lexer, sufficient to prove a command read-only.
/// Constructs whose effect it cannot see — command substitution, backticks,
/// process substitution, heredocs, bash ANSI-C quoting (`$'...'`) — and
/// unterminated quotes are errors. A word is marked `dynamic` when it
/// contains an unquoted or double-quoted `$`, or an unquoted `*`, `?`, `[`
/// or `{` (pathname / brace expansion).
fn lex_shell(s: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut dynamic = false;
    let mut i = 0usize;

    fn flush(toks: &mut Vec<Tok>, word: &mut String, in_word: &mut bool, dynamic: &mut bool) {
        if *in_word {
            toks.push(Tok::Word {
                text: std::mem::take(word),
                dynamic: *dynamic,
            });
        }
        *in_word = false;
        *dynamic = false;
    }

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                i += 1;
            }
            '\n' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                toks.push(Tok::Sep);
                i += 1;
            }
            '#' if !in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' => {
                match next {
                    Some('\n') => {}
                    Some(n) => {
                        word.push(n);
                        in_word = true;
                    }
                    None => return Err("trailing backslash".into()),
                }
                i += 2;
            }
            '\'' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&ch| ch == '\'')
                    .ok_or("unterminated single quote")?;
                word.extend(&chars[i + 1..i + 1 + end]);
                in_word = true;
                i += end + 2;
            }
            '"' => {
                i += 1;
                loop {
                    let Some(&ch) = chars.get(i) else {
                        return Err("unterminated double quote".into());
                    };
                    match ch {
                        '"' => {
                            i += 1;
                            break;
                        }
                        '\\' => {
                            match chars.get(i + 1) {
                                Some(&n @ ('$' | '`' | '"' | '\\')) => word.push(n),
                                Some('\n') => {}
                                Some(&n) => {
                                    word.push('\\');
                                    word.push(n);
                                }
                                None => return Err("unterminated double quote".into()),
                            }
                            i += 2;
                        }
                        '`' => return Err("command substitution".into()),
                        '$' => {
                            if chars.get(i + 1) == Some(&'(') {
                                return Err("command substitution".into());
                            }
                            dynamic = true;
                            word.push('$');
                            i += 1;
                        }
                        _ => {
                            word.push(ch);
                            i += 1;
                        }
                    }
                }
                in_word = true;
            }
            '`' => return Err("command substitution".into()),
            '$' => {
                if next == Some('(') {
                    return Err("command substitution".into());
                }
                // bash ANSI-C quoting: inside `$'...'` a `\'` does not end
                // the string, so treating it as `$` + a plain single quote
                // mis-splits the command. It is not lexed; it is rejected.
                if next == Some('\'') {
                    return Err("ANSI-C quoting ($'...') is not analysed".into());
                }
                dynamic = true;
                word.push('$');
                in_word = true;
                i += 1;
            }
            // Unquoted pathname / brace expansion: the words bash passes are
            // not the text written (`*` may expand to a file named
            // `--pre=prog`, `{--pre=prog,x}` to `--pre=prog x`).
            '*' | '?' | '[' | '{' => {
                dynamic = true;
                word.push(c);
                in_word = true;
                i += 1;
            }
            ';' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                toks.push(Tok::Sep);
                i += if next == Some(';') { 2 } else { 1 };
            }
            '|' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                toks.push(Tok::Sep);
                i += if matches!(next, Some('|' | '&')) {
                    2
                } else {
                    1
                };
            }
            '&' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                match next {
                    Some('>') => {
                        toks.push(Tok::Redir(RedirKind::Write));
                        i += if chars.get(i + 2) == Some(&'>') { 3 } else { 2 };
                    }
                    Some('&') => {
                        toks.push(Tok::Sep);
                        i += 2;
                    }
                    _ => {
                        toks.push(Tok::Sep);
                        i += 1;
                    }
                }
            }
            '(' | ')' => {
                flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                toks.push(Tok::Group);
                i += 1;
            }
            '<' | '>' => {
                // A word of digits right before the operator is its fd number.
                if in_word
                    && !dynamic
                    && !word.is_empty()
                    && word.chars().all(|d| d.is_ascii_digit())
                {
                    word.clear();
                    in_word = false;
                } else {
                    flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
                }
                if next == Some('(') {
                    return Err("process substitution".into());
                }
                let (kind, len) = match (c, next) {
                    ('<', Some('<')) => return Err("heredoc / herestring".into()),
                    ('<', Some('>')) => (RedirKind::Write, 2),
                    ('<', Some('&')) | ('>', Some('&')) => (RedirKind::Dup, 2),
                    ('<', _) => (RedirKind::Read, 1),
                    ('>', Some('>' | '|')) => (RedirKind::Write, 2),
                    _ => (RedirKind::Write, 1),
                };
                toks.push(Tok::Redir(kind));
                i += len;
            }
            _ => {
                word.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    flush(&mut toks, &mut word, &mut in_word, &mut dynamic);
    Ok(toks)
}

/// `path` (absolute, or relative to the root) is inside the root and its
/// root-relative path matches a web glob.
fn is_web_file(path: &str, root: &Path, canon_root: Option<&Path>, globs: &GlobSet) -> bool {
    let p = Path::new(path);
    if p.is_relative() {
        return globs.is_match(p);
    }
    let rel = p
        .strip_prefix(root)
        .ok()
        .or_else(|| canon_root.and_then(|c| p.strip_prefix(c).ok()));
    rel.is_some_and(|r| globs.is_match(r))
}

// ---------------------------------------------------------------------------
// pages, with a dev server the gate starts and always stops
// ---------------------------------------------------------------------------

/// Stops the dev server it guards when dropped (early return or panic), so
/// the server the gate started never outlives the gate.
struct ServerGuard<'a> {
    state: &'a Path,
    root: &'a Path,
    armed: bool,
}

impl ServerGuard<'_> {
    /// Stop now and return the result (the drop then does nothing).
    fn stop(mut self) -> Determination<serve::Stopped> {
        self.armed = false;
        serve::stop(self.state, self.root)
    }
}

impl Drop for ServerGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Determination::Undetermined(why) = serve::stop(self.state, self.root) {
                eprintln!(
                    "sitecraft gate: the dev server could not be stopped: {}",
                    why.as_str()
                );
            }
        }
    }
}

/// `serve start`, `check pages` against it, `serve stop`. A server that
/// cannot be started or stopped is `Undetermined`.
fn pages_with_server(state: &Path, root: &Path) -> Verdict {
    let started = match serve::start(state, root) {
        Determination::Known(st) => st,
        Determination::Undetermined(why) => {
            return Verdict::undetermined(format!(
                "check pages: undetermined: the dev server could not be started, so no page was checked: {}",
                why.as_str()
            ))
        }
    };
    let guard = ServerGuard {
        state,
        root,
        armed: true,
    };
    let pages = annotate(
        "check pages",
        check::pages::run(root, Some(&started.url)),
        "fix the listed page errors, then stop again",
    );
    let stopped = match guard.stop() {
        Determination::Known(_) => Verdict::from_findings(Vec::new()),
        Determination::Undetermined(why) => Verdict::undetermined(format!(
            "serve stop: undetermined: the dev server the gate started could not be stopped: {}",
            why.as_str()
        )),
    };
    Verdict::worst_of([pages, stopped])
}

// ---------------------------------------------------------------------------
// decision output and bounded give-up
// ---------------------------------------------------------------------------

/// Print the Stop-hook decision for `v` and return the exit code (always 0 in
/// hook mode: Claude Code reads the decision JSON only on exit 0).
///
/// `Clean` resets the counter and prints nothing. A block bumps the
/// per-session counter; once it exceeds `max_attempts` the stop is allowed
/// with a `systemMessage` saying NOTHING WAS VERIFIED. Without a state dir
/// the counter cannot be kept and every stop blocks.
fn finish(v: &Verdict, budget: (u32, i64), state: Option<&Path>, session: &str) -> i32 {
    let (max_attempts, reset_after) = budget;
    let counters = state.map(|s| s.join(ATTEMPTS_DIR));
    let text = match v {
        Verdict::Clean(_) => {
            if let Some(dir) = &counters {
                state::reset(dir, session);
            }
            return 0;
        }
        Verdict::Violation(r) => format!("sitecraft: BLOCKED - {}", r.as_str()),
        Verdict::Undetermined(u) => format!("sitecraft: BLOCKED - undetermined: {}", u.as_str()),
    };
    eprintln!("{text}");
    let Some(dir) = counters else {
        print_block(&format!(
            "{text}\n(the give-up counter cannot be kept without a state dir; fix the cause and stop again)"
        ));
        return 0;
    };
    let attempt = state::bump(&dir, session, reset_after);
    if attempt > max_attempts {
        state::reset(&dir, session);
        println!(
            "{}",
            serde_json::json!({
                "systemMessage": format!(
                    "sitecraft gave up after {max_attempts} blocked stops and is allowing this stop. \
                     NOTHING WAS VERIFIED: the sitecraft checks are still failing or undetermined. Last result: {text}"
                )
            })
        );
        return 0;
    }
    print_block(&format!(
        "{text}\n(attempt {attempt}/{max_attempts}; fix the cause and stop again)"
    ));
    0
}

fn print_block(reason: &str) {
    println!(
        "{}",
        serde_json::json!({ "decision": "block", "reason": reason })
    );
}
