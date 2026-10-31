//! `sitecraft gate` — the Stop hook.
//!
//! Runs through `harness_core::gate::run::run_guarded` (gate name
//! `sitecraft`; a panic blocks). In a repo with `sitecraft.toml` it combines,
//! with [`Verdict::worst_of`]: trust of the config, `decide check`,
//! `check security`, and — only when the session wrote a file matching
//! `web_globs`, or when that cannot be determined — `serve start` +
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
/// targets such a file; `Known(false)` only when the whole transcript was read
/// and parsed and no such call exists; `Undetermined` when there is no
/// transcript path, it cannot be read, it is empty, any line does not parse,
/// a write call has no usable path, or a sub-agent ran (its edits are not in
/// this transcript).
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
    Determination::known(false)
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
