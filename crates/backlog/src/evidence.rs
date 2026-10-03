//! Close evidence: running a COMMITTED test under a fixed runner allowlist and
//! turning what was observed into a closure record (close-evidence spec,
//! user-ratified 2026-10-01).
//!
//! The rule this module exists to enforce: **suspicion is never evidence.**
//! Evidence is an EXECUTED, COMMITTED test. grep, `file:line` citations,
//! commit citations and code reading are never evidence, so the runner
//! allowlist admits test runners only (`cargo test`, `pytest` /
//! `python3 -m pytest`, or `bash`/`sh` running a git-tracked script under a
//! tests directory), and every command is split into argv and executed
//! directly — never through `sh -c`, and never with shell metacharacters.
//!
//! Three answers everywhere, never two (CLAUDE.md §3): a run that could not be
//! carried to a conclusion (spawn failure, timeout, unreadable output, a git
//! query that failed, a dirty tree) is `Undetermined` and every caller turns
//! it into a REFUSAL naming the cause. It is never folded into "passed" or
//! "failed".
//!
//! The doc-only predicate here ([`is_doc_path`]) is mirrored by the pre-commit
//! gate `scripts/check-closure-evidence.py`; the two must be changed together.

use crate::task::{
    GreenRun, RedRun, Repro, REPRO_NOT_REPRODUCED, REPRO_REPRODUCED, REPRO_UNDETERMINED,
};
use harness_core::boundary::{self, CommandOutput};
use harness_core::verdict::Determination;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default bound on one test run (`BACKLOG_TEST_TIMEOUT_SECS` overrides).
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
/// Clamp for `BACKLOG_TEST_TIMEOUT_SECS`. The floor keeps a `0` from meaning
/// "no time at all" (every run undetermined); the ceiling keeps a typo from
/// meaning "wait forever".
pub const MIN_TIMEOUT_SECS: u64 = 1;
pub const MAX_TIMEOUT_SECS: u64 = 7200;
/// Cap on the recorded output excerpt.
pub const EXCERPT_MAX_BYTES: usize = 4096;
/// Bound on each git query this module makes (local reads/worktree ops).
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Characters that give a command line shell meaning. Their presence refuses
/// the command outright: the command is executed as argv, so accepting them
/// would record a command text that does not mean what it reads as.
const SHELL_METACHARS: &[char] = &[
    ';', '&', '|', '<', '>', '$', '`', '(', ')', '{', '}', '[', ']', '*', '?', '~', '!', '#', '\\',
    '\'', '"', '\n', '\r',
];

/// Which allowlisted runner a command is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    CargoTest,
    Pytest,
    Script,
}

impl Runner {
    pub fn label(self) -> &'static str {
        match self {
            Runner::CargoTest => "cargo-test",
            Runner::Pytest => "pytest",
            Runner::Script => "script",
        }
    }
}

/// A parsed, allowlisted test command.
#[derive(Debug, Clone)]
pub struct TestCmd {
    pub runner: Runner,
    pub argv: Vec<String>,
    /// The exact text the caller supplied (recorded as `green.cmd`).
    pub text: String,
    /// For [`Runner::Script`], the repo-relative script path.
    pub script: Option<String>,
}

/// Parse `text` into an allowlisted [`TestCmd`], or refuse naming why.
pub fn parse_test_cmd(text: &str) -> Result<TestCmd, String> {
    if let Some(c) = text.chars().find(|c| SHELL_METACHARS.contains(c)) {
        return Err(format!(
            "test command {text:?} contains the shell metacharacter {c:?}; commands are executed \
             as argv (never through a shell), so a command with shell syntax is refused"
        ));
    }
    let argv: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    let words: Vec<&str> = text.split_whitespace().collect();
    let not_allowed = || {
        format!(
            "test command {text:?} is not on the runner allowlist (`cargo test ...`, `pytest ...`, \
             `python3 -m pytest ...`, or `bash|sh <git-tracked script under a tests dir>`); \
             grep/cat/echo and other non-test commands are never evidence"
        )
    };
    match words.as_slice() {
        ["cargo", "test", ..] => Ok(TestCmd {
            runner: Runner::CargoTest,
            argv,
            text: text.to_string(),
            script: None,
        }),
        ["pytest", ..] | ["python3", "-m", "pytest", ..] => Ok(TestCmd {
            runner: Runner::Pytest,
            argv,
            text: text.to_string(),
            script: None,
        }),
        ["bash" | "sh", script, ..] => {
            let script = script.strip_prefix("./").unwrap_or(script);
            if script.starts_with('-') {
                return Err(format!(
                    "test command {text:?}: `{}` must be followed by a script path, not an option",
                    words[0]
                ));
            }
            let path = Path::new(script);
            if path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!(
                    "test command {text:?}: the script path must be repo-relative without `..`"
                ));
            }
            let in_tests_dir = path
                .parent()
                .map(|p| {
                    p.components().any(|c| {
                        let s = c.as_os_str().to_string_lossy();
                        s == "tests" || s == "test"
                    })
                })
                .unwrap_or(false);
            if !in_tests_dir {
                return Err(format!(
                    "test command {text:?}: script {script} is not under a tests directory \
                     (`tests/` or `test/`); only committed test scripts are evidence"
                ));
            }
            Ok(TestCmd {
                runner: Runner::Script,
                argv,
                text: text.to_string(),
                script: Some(script.to_string()),
            })
        }
        _ => Err(not_allowed()),
    }
}

/// The per-run timeout from `BACKLOG_TEST_TIMEOUT_SECS`, clamped to
/// `MIN..=MAX`. An unset variable is the default; a value that is not an
/// integer is refused rather than guessed.
pub fn test_timeout() -> Result<Duration, String> {
    match std::env::var("BACKLOG_TEST_TIMEOUT_SECS") {
        Err(std::env::VarError::NotPresent) => Ok(Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("BACKLOG_TEST_TIMEOUT_SECS is not valid unicode".to_string())
        }
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(n) => Ok(Duration::from_secs(
                n.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS),
            )),
            Err(e) => Err(format!(
                "BACKLOG_TEST_TIMEOUT_SECS={v:?} is not an integer number of seconds ({e})"
            )),
        },
    }
}

// ---- git -------------------------------------------------------------------

/// Run `git -C root ARGS` bounded; `Known` only for an exit code in `ok`.
fn git_out(root: &Path, args: &[&str], ok: &[i32]) -> Determination<(i32, String)> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args).stdin(Stdio::null());
    match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => {
            let code = out.code();
            match out.stdout_allowing(ok) {
                Determination::Known(s) => Determination::known((code, s)),
                Determination::Undetermined(why) => Determination::Undetermined(why),
            }
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// `git -C root ARGS` that must exit 0; its stdout.
fn git_ok(root: &Path, args: &[&str]) -> Result<String, String> {
    match git_out(root, args, &[0]) {
        Determination::Known((_, s)) => Ok(s),
        Determination::Undetermined(why) => Err(format!("git {} failed: {why}", args.join(" "))),
    }
}

/// The toplevel of the checkout `cwd` is in.
pub fn repo_toplevel(cwd: &Path) -> Result<PathBuf, String> {
    let s = git_ok(cwd, &["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("cannot determine the git checkout to run tests in: {e}"))?;
    let t = s.trim();
    if t.is_empty() {
        return Err("git rev-parse --show-toplevel printed nothing".to_string());
    }
    Ok(PathBuf::from(t))
}

/// The full 40-hex commit id `rev` names, or a refusal naming why.
pub fn resolve_commit(root: &Path, rev: &str) -> Result<String, String> {
    if rev.is_empty()
        || rev.starts_with('-')
        || rev
            .chars()
            .any(|c| c.is_whitespace() || SHELL_METACHARS.contains(&c))
    {
        return Err(format!("{rev:?} is not a valid revision"));
    }
    let spec = format!("{rev}^{{commit}}");
    match git_out(
        root,
        &["rev-parse", "--verify", "--quiet", &spec],
        &[0, 1, 128],
    ) {
        Determination::Known((0, s)) => {
            let sha = s.trim().to_string();
            if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                Ok(sha)
            } else {
                Err(format!(
                    "git resolved {rev:?} to {sha:?}, which is not a 40-hex commit id"
                ))
            }
        }
        Determination::Known((_, _)) => {
            Err(format!("{rev:?} does not name a commit in this repository"))
        }
        Determination::Undetermined(why) => Err(format!("cannot resolve {rev:?}: {why}")),
    }
}

/// HEAD as a full commit id.
pub fn head_commit(root: &Path) -> Result<String, String> {
    resolve_commit(root, "HEAD")
}

/// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
pub fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    match git_out(
        root,
        &["merge-base", "--is-ancestor", ancestor, descendant],
        &[0, 1],
    ) {
        Determination::Known((0, _)) => Ok(true),
        Determination::Known((_, _)) => Ok(false),
        Determination::Undetermined(why) => Err(format!(
            "cannot determine whether {ancestor} is an ancestor of {descendant}: {why}"
        )),
    }
}

/// Refuse unless the working tree (outside `.backlog`) is clean, untracked
/// files included: a test run against uncommitted content is not evidence
/// about any commit.
pub fn require_clean_tree(root: &Path) -> Result<(), String> {
    let s = git_ok(
        root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).backlog",
        ],
    )
    .map_err(|e| format!("cannot determine whether the working tree is clean: {e}"))?;
    if s.trim().is_empty() {
        return Ok(());
    }
    let first: Vec<&str> = s.lines().take(5).collect();
    Err(format!(
        "the working tree has uncommitted changes outside .backlog ({}); tests must run against \
         committed content — commit or stash them first",
        first.join(", ")
    ))
}

/// Whether `path` is present in the tree of commit `rev`.
fn tracked_at(root: &Path, rev: &str, path: &str) -> Result<bool, String> {
    let s = git_ok(root, &["ls-tree", "--name-only", rev, "--", path])?;
    Ok(s.lines().any(|l| l == path))
}

// ---- running ---------------------------------------------------------------

/// What a completed (not undetermined) run showed.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub exit: i32,
    pub output: String,
}

/// Execute `cmd` in `dir` with the bound. `Undetermined` = no conclusion.
fn execute(cmd: &TestCmd, dir: &Path, timeout: Duration) -> Determination<RunResult> {
    let mut c = Command::new(&cmd.argv[0]);
    c.args(&cmd.argv[1..]).current_dir(dir).stdin(Stdio::null());
    match boundary::run_with_timeout(&mut c, timeout) {
        Determination::Known(out) => Determination::known(to_result(out)),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

fn to_result(out: CommandOutput) -> RunResult {
    let code = out.code();
    let stderr = out.stderr().to_string();
    let stdout = match out.stdout_allowing(&[code]) {
        Determination::Known(s) => s,
        Determination::Undetermined(_) => {
            unreachable!("stdout_allowing given the command's own exit code always succeeds")
        }
    };
    RunResult {
        exit: code,
        output: format!("{stdout}{stderr}"),
    }
}

/// Sum of every `<N> passed` in `output`, or `None` when no such count
/// appears at all (the run did not say how many tests it ran).
pub fn passed_count(output: &str) -> Option<u64> {
    let words: Vec<&str> = output
        .split(|c: char| c.is_whitespace() || c == ';' || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let mut total: Option<u64> = None;
    for pair in words.windows(2) {
        if pair[1] == "passed" || pair[1].starts_with("passed") {
            if let Ok(n) = pair[0].parse::<u64>() {
                total = Some(total.unwrap_or(0).saturating_add(n));
            }
        }
    }
    total
}

/// Markers of a BUILD failure (the test never ran), as opposed to a test that
/// ran and failed.
fn build_failure_marker(output: &str) -> Option<&'static str> {
    const MARKERS: &[&str] = &[
        "could not compile",
        "error[E",
        "error: linking with",
        "ERROR collecting",
        "ImportError",
        "ModuleNotFoundError",
        "SyntaxError",
    ];
    MARKERS.iter().copied().find(|m| output.contains(m))
}

/// Is a non-zero run a BEHAVIOURAL failure (the test ran and failed)?
/// `Err` names why it is not (build failure, runner usage error, signal...).
fn classify_red(cmd: &TestCmd, r: &RunResult) -> Result<(), String> {
    if let Some(m) = build_failure_marker(&r.output) {
        return Err(format!(
            "the run failed at BUILD/compile time (output contains {m:?}), not behaviourally; a \
             build failure is not RED. Restructure the test so the pre-fix revision builds and \
             fails an assertion, or route the item via `backlog ruling request`"
        ));
    }
    match cmd.runner {
        Runner::CargoTest => {
            if r.output.contains("test result: FAILED") {
                Ok(())
            } else {
                Err(format!(
                    "cargo exited {} without a `test result: FAILED` line, so no test is shown \
                     to have run and failed (a build/compile failure is not RED; route via \
                     `backlog ruling request` if the RED cannot be behavioural)",
                    r.exit
                ))
            }
        }
        Runner::Pytest => {
            if r.exit == 1 {
                Ok(())
            } else {
                Err(format!(
                    "pytest exited {} (only 1 means tests ran and failed; 2-5 are collection, \
                     usage or no-tests errors, not RED)",
                    r.exit
                ))
            }
        }
        Runner::Script => {
            if r.exit == 126 || r.exit == 127 || r.exit >= 128 {
                Err(format!(
                    "the script exited {} (not executable / command not found / killed by a \
                     signal), which is not a behavioural failure",
                    r.exit
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn digest(output: &str) -> String {
    let mut h = Sha256::new();
    h.update(output.as_bytes());
    let bytes = h.finalize();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// The last `EXCERPT_MAX_BYTES` of `output`, cut at a char boundary.
pub fn excerpt(output: &str) -> String {
    if output.len() <= EXCERPT_MAX_BYTES {
        return output.to_string();
    }
    let mut start = output.len() - EXCERPT_MAX_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].to_string()
}

/// Pre-run checks shared by closes and repros: clean tree, and for a script
/// runner, that the script is committed at HEAD.
fn preflight(root: &Path, cmd: &TestCmd, head: &str) -> Result<(), String> {
    require_clean_tree(root)?;
    if let Some(script) = &cmd.script {
        if !tracked_at(root, head, script)? {
            return Err(format!(
                "script {script} is not committed at HEAD ({head}); only a git-tracked, committed \
                 test script is evidence"
            ));
        }
    }
    Ok(())
}

/// The observed F2P evidence of a close.
#[derive(Debug, Clone)]
pub struct F2p {
    pub green: GreenRun,
    pub red: RedRun,
}

/// Run the F2P protocol: GREEN at HEAD in the (clean) working tree, RED at
/// `red_rev` in a detached temp worktree with the test files overlaid from
/// HEAD. Every failure to observe either side refuses with its cause.
pub fn run_f2p(root: &Path, cmd: &TestCmd, red_rev: &str, now: i64) -> Result<F2p, String> {
    let timeout = test_timeout()?;
    let head = head_commit(root)?;
    let red = resolve_commit(root, red_rev).map_err(|e| format!("--red-rev: {e}"))?;
    if !is_ancestor(root, &red, &head)? {
        return Err(format!(
            "--red-rev {red} is not an ancestor of HEAD ({head}); RED must be observed on this \
             history"
        ));
    }
    preflight(root, cmd, &head)?;

    // GREEN at HEAD.
    let g = match execute(cmd, root, timeout) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => {
            return Err(format!(
                "the GREEN run at HEAD did not complete, so nothing was observed: {why}"
            ))
        }
    };
    if g.exit != 0 {
        return Err(format!(
            "the test FAILS at HEAD (exit {}); a failing test is not evidence of a fix. Output \
             tail: {}",
            g.exit,
            excerpt_short(&g.output)
        ));
    }
    let passed = match passed_count(&g.output) {
        Some(0) => {
            return Err(
                "the GREEN run reported 0 passed; a run that executed no test proves nothing"
                    .to_string(),
            )
        }
        Some(n) => n,
        None => {
            return Err(
                "the GREEN run exited 0 but reported no `<N> passed` count, so it is unknown \
                 whether any test ran; make the test print its pass count"
                    .to_string(),
            )
        }
    };

    // RED at red-rev.
    let r = run_in_temp_worktree(root, &red, &head, cmd, timeout)?;
    let red_run = match r {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => {
            return Err(format!(
                "the RED run at {red} did not complete, so no RED was observed: {why}"
            ))
        }
    };
    if red_run.exit == 0 {
        return Err(format!(
            "the test PASSES at --red-rev {red}; a test that passes before the fix is not RED \
             and proves nothing about it"
        ));
    }
    classify_red(cmd, &red_run).map_err(|why| format!("RED at {red} refused: {why}"))?;

    // Nothing the runs did may have moved HEAD or dirtied the tree.
    let head_after = head_commit(root)?;
    if head_after != head {
        return Err(format!(
            "HEAD moved during the test run ({head} -> {head_after}); the observation is not about \
             a single commit"
        ));
    }
    require_clean_tree(root).map_err(|e| format!("after the test run: {e}"))?;

    Ok(F2p {
        green: GreenRun {
            runner: cmd.runner.label().to_string(),
            cmd: cmd.text.clone(),
            exit: g.exit,
            passed,
            rev: head,
            observed_at: now,
            output_digest: digest(&g.output),
            excerpt: excerpt(&g.output),
        },
        red: RedRun {
            rev: red,
            exit: red_run.exit,
            kind: "behavioural".to_string(),
        },
    })
}

fn excerpt_short(output: &str) -> String {
    let t = output.trim_end();
    let start = t.len().saturating_sub(400);
    let mut s = start;
    while !t.is_char_boundary(s) {
        s += 1;
    }
    t[s..].to_string()
}

/// A detached temp worktree that is removed on every exit path.
struct TempWorktree {
    root: PathBuf,
    path: PathBuf,
    removed: bool,
}

impl TempWorktree {
    fn create(root: &Path, rev: &str) -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .map_err(|e| format!("system clock before the epoch: {e}"))?;
        let path = std::env::temp_dir().join(format!("backlog-red-{}-{nanos}", std::process::id()));
        let p = path.to_string_lossy().into_owned();
        git_ok(root, &["worktree", "add", "--detach", "--quiet", &p, rev])
            .map_err(|e| format!("cannot create the temp worktree for the RED run: {e}"))?;
        Ok(TempWorktree {
            root: root.to_path_buf(),
            path,
            removed: false,
        })
    }

    fn remove(&mut self) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        self.removed = true;
        let p = self.path.to_string_lossy().into_owned();
        let r = git_ok(
            &self.root,
            &["worktree", "remove", "--force", "--force", &p],
        );
        if self.path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.path) {
                return Err(format!(
                    "the temp worktree {} could not be removed: {e}",
                    self.path.display()
                ));
            }
        }
        let prune = git_ok(&self.root, &["worktree", "prune"]);
        r.and(prune).map(|_| ())
    }
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        if let Err(e) = self.remove() {
            eprintln!("warning: {e}");
        }
    }
}

/// Is `p` (a `/`-separated repo path) a test file for the RED overlay? The
/// same predicate as `is_test_path` in `scripts/check-closure-evidence.py`, so
/// the CLI and the pre-commit gate re-run RED against the same overlay: any
/// `tests`/`test` directory component, or a basename `test_*.py`,
/// `*_test.py` or `conftest.py`.
fn is_test_path(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    let (base, dirs) = match parts.split_last() {
        Some((b, d)) => (*b, d),
        None => return false,
    };
    dirs.iter().any(|d| *d == "tests" || *d == "test")
        || (base.starts_with("test_") && base.ends_with(".py"))
        || base.ends_with("_test.py")
        || base == "conftest.py"
}

/// The test files of HEAD (see [`is_test_path`]), plus the script itself.
fn head_test_files(root: &Path, head: &str, cmd: &TestCmd) -> Result<Vec<String>, String> {
    let s = git_ok(root, &["ls-tree", "-r", "-z", "--name-only", head])?;
    let mut files: Vec<String> = s
        .split('\0')
        .filter(|p| !p.is_empty() && is_test_path(p))
        .map(str::to_string)
        .collect();
    if let Some(script) = &cmd.script {
        if !files.iter().any(|f| f == script) {
            files.push(script.clone());
        }
    }
    Ok(files)
}

fn run_in_temp_worktree(
    root: &Path,
    red: &str,
    head: &str,
    cmd: &TestCmd,
    timeout: Duration,
) -> Result<Determination<RunResult>, String> {
    let files = head_test_files(root, head, cmd)?;
    let mut wt = TempWorktree::create(root, red)?;
    for chunk in files.chunks(200) {
        let mut args: Vec<&str> = vec!["checkout", head, "--"];
        args.extend(chunk.iter().map(String::as_str));
        if let Err(e) = git_ok(&wt.path, &args) {
            let cleanup = match wt.remove() {
                Ok(()) => String::new(),
                Err(c) => format!("; additionally {c}"),
            };
            return Err(format!(
                "cannot overlay HEAD's test files onto the RED worktree: {e}{cleanup}"
            ));
        }
    }
    let out = execute(cmd, &wt.path, timeout);
    wt.remove()?;
    Ok(out)
}

// ---- findings: repro -------------------------------------------------------

/// Run a finding's repro test at HEAD and record the outcome. Polarity: a
/// repro that FAILS behaviourally reproduces the bug (same as a RED test);
/// exit 0 is not-reproduced; everything that cannot be run to a conclusion is
/// undetermined, with the cause in `detail`. Never returns an error: a finding
/// is never lost because its repro could not run.
pub fn run_repro(cwd: &Path, text: &str, now: i64) -> Repro {
    let undetermined = |detail: String, rev: Option<String>| Repro {
        outcome: REPRO_UNDETERMINED.to_string(),
        cmd: text.to_string(),
        rev,
        observed_at: now,
        detail,
    };
    let cmd = match parse_test_cmd(text) {
        Ok(c) => c,
        Err(e) => return undetermined(e, None),
    };
    let timeout = match test_timeout() {
        Ok(t) => t,
        Err(e) => return undetermined(e, None),
    };
    let root = match repo_toplevel(cwd) {
        Ok(r) => r,
        Err(e) => return undetermined(e, None),
    };
    let head = match head_commit(&root) {
        Ok(h) => h,
        Err(e) => return undetermined(e, None),
    };
    if let Err(e) = preflight(&root, &cmd, &head) {
        return undetermined(e, Some(head));
    }
    let r = match execute(&cmd, &root, timeout) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => {
            return undetermined(format!("the repro run did not complete: {why}"), Some(head))
        }
    };
    if r.exit == 0 {
        return Repro {
            outcome: REPRO_NOT_REPRODUCED.to_string(),
            cmd: text.to_string(),
            rev: Some(head),
            observed_at: now,
            detail: format!("exit 0: {}", excerpt_short(&r.output)),
        };
    }
    match classify_red(&cmd, &r) {
        Ok(()) => Repro {
            outcome: REPRO_REPRODUCED.to_string(),
            cmd: text.to_string(),
            rev: Some(head),
            observed_at: now,
            detail: format!("exit {}: {}", r.exit, excerpt_short(&r.output)),
        },
        Err(why) => undetermined(
            format!("exit {} is not a behavioural failure: {why}", r.exit),
            Some(head),
        ),
    }
}

// ---- doc-only --------------------------------------------------------------

/// Is `path` a DOC path for `--doc-only`? Doc = `*.md`, anything under a
/// top-level `docs/`, or a `README*` file — EXCEPT prompt-bearing markdown,
/// which is CODE: any `SKILL.md`, and any `.md` under an `agents/`,
/// `commands/` or `skills/` directory. Comments/docstrings inside code files
/// are code (the predicate is by path, so a `.rs` file is never doc).
///
/// Mirrored by `scripts/check-closure-evidence.py`; change both together.
pub fn is_doc_path(path: &str) -> bool {
    let p = Path::new(path);
    let name = match p.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => return false,
    };
    let dirs: Vec<String> = p
        .parent()
        .map(|d| {
            d.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let is_md = name.to_ascii_lowercase().ends_with(".md");
    if name == "SKILL.md" {
        return false;
    }
    if is_md
        && dirs
            .iter()
            .any(|d| d == "agents" || d == "commands" || d == "skills")
    {
        return false;
    }
    if is_md {
        return true;
    }
    if dirs.first().is_some_and(|d| d == "docs") {
        return true;
    }
    name.starts_with("README")
}

/// Verify `commit` for a doc-only close: an ancestor of HEAD, non-root,
/// non-merge, touching only doc paths. Returns the full commit id.
pub fn verify_doc_only(root: &Path, commit: &str) -> Result<String, String> {
    let c = resolve_commit(root, commit).map_err(|e| format!("--doc-only: {e}"))?;
    let head = head_commit(root)?;
    if !is_ancestor(root, &c, &head)? {
        return Err(format!(
            "--doc-only {c} is not an ancestor of HEAD ({head})"
        ));
    }
    let parents = git_ok(root, &["rev-list", "--parents", "-n", "1", &c])?;
    let n_parents = parents.split_whitespace().count().saturating_sub(1);
    if n_parents == 0 {
        return Err(format!(
            "--doc-only {c} is a root commit; its diff is not a doc change"
        ));
    }
    if n_parents > 1 {
        return Err(format!(
            "--doc-only {c} is a merge commit; name the non-merge commit that made the doc change"
        ));
    }
    let names = git_ok(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--no-renames",
            "-r",
            "-z",
            &c,
        ],
    )?;
    let paths: Vec<&str> = names.split('\0').filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        return Err(format!(
            "--doc-only {c} changes no files; an empty diff is not a doc change"
        ));
    }
    let code: Vec<&str> = paths.iter().copied().filter(|p| !is_doc_path(p)).collect();
    if !code.is_empty() {
        return Err(format!(
            "--doc-only {c} touches non-doc (code) paths: {} — prompt-bearing markdown (SKILL.md, \
             .md under agents/ commands/ skills/) and comments inside code files are code",
            code.join(", ")
        ));
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_matches_the_gate_predicate() {
        for p in [
            "tests/a.sh",
            "crates/x/tests/y.rs",
            "a/test/b.py",
            "test_x.py",
            "pkg/test_mod.py",
            "pkg/mod_test.py",
            "pkg/conftest.py",
        ] {
            assert!(is_test_path(p), "{p} is a test path");
        }
        for p in [
            "tests",
            "src/lib.rs",
            "test_x.rs",
            "pkg/testing.py",
            "contest.py",
            "a/tests_x/b.rs",
        ] {
            assert!(!is_test_path(p), "{p} is not a test path");
        }
    }

    #[test]
    fn allowlist_and_metachars() {
        assert!(parse_test_cmd("cargo test -p x").is_ok());
        assert!(parse_test_cmd("python3 -m pytest t").is_ok());
        assert!(parse_test_cmd("bash tests/a.sh").is_ok());
        assert!(parse_test_cmd("bash scripts/a.sh").is_err());
        assert!(parse_test_cmd("grep -q x y").is_err());
        assert!(parse_test_cmd("bash tests/a.sh; true").is_err());
        assert!(parse_test_cmd("").is_err());
    }

    #[test]
    fn passed_counts() {
        assert_eq!(passed_count("test result: ok. 3 passed; 0 failed"), Some(3));
        assert_eq!(passed_count("a\n1 passed\nb 2 passed"), Some(3));
        assert_eq!(passed_count("0 passed"), Some(0));
        assert_eq!(passed_count("nothing"), None);
    }

    #[test]
    fn doc_paths() {
        assert!(is_doc_path("docs/a.md"));
        assert!(is_doc_path("docs/x/y.txt"));
        assert!(is_doc_path("README.md"));
        assert!(is_doc_path("crates/x/README.ja.md"));
        assert!(!is_doc_path("SKILL.md"));
        assert!(!is_doc_path("crates/x/skills/a/SKILL.md"));
        assert!(!is_doc_path("crates/x/agents/w.md"));
        assert!(!is_doc_path("src/lib.rs"));
    }

    #[test]
    fn excerpt_caps() {
        let s = "x".repeat(10_000);
        assert_eq!(excerpt(&s).len(), EXCERPT_MAX_BYTES);
    }
}
