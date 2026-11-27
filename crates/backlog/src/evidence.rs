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
//! Every run inherits only the fixed [`INHERITED_ENV`] variables from the
//! caller, so the recorded command text alone reproduces what was observed.
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
    GreenRun, RedRun, Repro, RED_KIND_NOT_EXECUTED, REPRO_NOT_REPRODUCED, REPRO_REPRODUCED,
    REPRO_UNDETERMINED,
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

/// Task ids named by a `(backlog <id>...)` marker in the subject of any commit
/// reachable from HEAD (backlog a04b2b87): the fix commit of such a task has
/// already landed, so `next` must not hand it out again.
///
/// Marker grammar (`fix_marker_id`, backlog 536e2811): the text between
/// `(backlog ` and the next `)` is trimmed; `<id>` is its leading run of
/// characters up to the first whitespace or `;`. The marker names `<id>` as
/// fixed only when the remainder after `<id>` (trimmed) is
/// - empty: `(backlog <id>)`;
/// - starts with `;` (everything after it is commentary, e.g. a version):
///   `(backlog <id>; 0.2.395)`;
/// - exactly `wave <digits>` (one space, at least one ASCII digit),
///   optionally followed by `;...`:
///   `(backlog <id> wave 2)`, `(backlog <id> wave 2; x)`.
///
/// Any other remainder -- e.g. `(backlog <id> RED repro)`, a RED-repro commit
/// that is not a fix -- names nothing. Comma lists such as
/// `(backlog A, B; 0.2.395)` are deliberately NOT recognised for either id
/// (an id token containing `,` names nothing): whether a multi-id
/// subject means "all fixed" is undecided, and counting an id as fixed drops
/// it from `next`, so an unrecognised marker resolves to "not fixed".
///
/// A recognised marker counts only when its commit is OBSERVED to change
/// product code ([`commit_changes_product`], backlog 363ad7da): a commit that
/// only files a RED test (test paths, `.backlog/`, version-line bumps) is not
/// a fix even with a plain `(backlog <id>)` subject, and a commit whose
/// changes cannot be determined is not counted as a fix (the task stays in
/// the queue; rationale there).
///
/// HEAD is first checked with `git rev-parse --verify -q HEAD`: exit 0 proceeds
/// to one `git log -z --grep='(backlog ' --name-only
/// --diff-merges=first-parent HEAD --` query (a git too old for
/// `--diff-merges`, < 2.31, fails it and so refuses). An UNBORN HEAD (a valid repo with no
/// commits yet) is `Ok(empty)` -- a known-empty history -- but only when it is
/// positively observed as unborn ([`head_is_unborn`]): `rev-parse` exited 1,
/// HEAD is a symbolic ref, and its target ref is absent (no loose ref file,
/// nothing listed by `for-each-ref`, nothing on its stderr). A corrupt branch
/// ref also makes `rev-parse` exit 1, and is NOT unborn. Everything else --
/// no repo, a corrupt or undeterminable ref, timeout, abnormal exit -- is
/// `Err`: an unanswered question is not "nothing is fixed", and the caller
/// refuses (CLAUDE.md section 3).
pub fn fixed_task_ids(root: &Path) -> Result<std::collections::HashSet<String>, String> {
    // A shallow clone hides history: a fix commit absent from `git log` is not
    // evidence that no fix landed (backlog 2072023d). Undetermined => refuse.
    match git_out(root, &["rev-parse", "--is-shallow-repository"], &[0]) {
        Determination::Known((_, s)) if s.trim() == "false" => {}
        Determination::Known((_, s)) if s.trim() == "true" => {
            return Err(format!(
                "{} is a shallow repository (truncated history): whether a task's fix commit \
                 landed cannot be determined; run `git fetch --unshallow` first",
                root.display()
            ));
        }
        Determination::Known((_, s)) => {
            return Err(format!(
                "git rev-parse --is-shallow-repository printed {:?}, expected true/false: \
                 shallow-ness is undetermined",
                s.trim()
            ));
        }
        Determination::Undetermined(why) => {
            return Err(format!(
                "git rev-parse --is-shallow-repository failed (shallow-ness undetermined): {why}"
            ));
        }
    }
    match git_out(root, &["rev-parse", "--verify", "-q", "HEAD"], &[0, 1]) {
        Determination::Known((0, _)) => {}
        Determination::Known((_, _)) => {
            head_is_unborn(root).map_err(|why| {
                format!(
                    "HEAD of {} does not resolve to a commit and is not a verifiably unborn \
                     branch: {why}",
                    root.display()
                )
            })?;
            return Ok(std::collections::HashSet::new());
        }
        Determination::Undetermined(why) => {
            return Err(format!("git rev-parse --verify -q HEAD failed: {why}"));
        }
    }
    // One bounded query: every commit reachable from HEAD whose message
    // contains `(backlog ` (subject markers are re-checked below), with the
    // paths it changed vs its FIRST parent (`--diff-merges=first-parent`: a
    // merge is judged by what it brought into the line it was merged into;
    // the merged branch's own commits are reachable too and are judged on
    // their own). `-z` keeps paths unquoted; `--no-renames` lists both sides
    // of a rename, so a product file moved under tests/ still shows its
    // product-side path.
    let log = git_ok(
        root,
        &[
            "log",
            "-z",
            "--no-color",
            "--no-renames",
            "-F",
            "--grep=(backlog ",
            "--format=%x01%H%x02%P%x02%s",
            "--name-only",
            "--diff-merges=first-parent",
            "HEAD",
            "--",
        ],
    )?;
    let mut ids = std::collections::HashSet::new();
    for commit in parse_marker_log(&log) {
        let marked = subject_marker_ids(&commit.subject);
        if marked.is_empty() {
            continue;
        }
        if commit_changes_product(root, &commit) {
            ids.extend(marked);
        }
    }
    Ok(ids)
}

/// The ids the `(backlog <inner>)` markers in `subject` name as fixed.
fn subject_marker_ids(subject: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = subject;
    while let Some(i) = rest.find("(backlog ") {
        rest = &rest[i + "(backlog ".len()..];
        if let Some(id) = rest.find(')').and_then(|j| fix_marker_id(&rest[..j])) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// One commit of the [`fixed_task_ids`] log query.
#[derive(Debug)]
struct MarkerCommit {
    sha: String,
    /// The first parent; `None` for a root commit.
    first_parent: Option<String>,
    subject: String,
    /// Paths changed vs the first parent. `None` = the record could not be
    /// parsed, so what the commit changed is undetermined.
    paths: Option<Vec<String>>,
}

/// Parse `git log -z --format=%x01%H%x02%P%x02%s --name-only` output: each
/// record is `\x01<sha>\x02<parents>\x02<subject>\0`, followed (when the
/// commit changed paths) by `\n<path>\0<path>\0...`.
fn parse_marker_log(log: &str) -> Vec<MarkerCommit> {
    let mut out: Vec<MarkerCommit> = Vec::new();
    for token in log.split('\0') {
        if let Some(header) = token.strip_prefix('\x01') {
            let mut parts = header.splitn(3, '\x02');
            let sha = parts.next().unwrap_or("").to_string();
            let parents = parts.next();
            let subject = parts.next();
            let commit = match (parents, subject) {
                (Some(p), Some(s)) if !sha.is_empty() => MarkerCommit {
                    sha,
                    first_parent: p.split_whitespace().next().map(str::to_string),
                    subject: s.to_string(),
                    paths: Some(Vec::new()),
                },
                // Malformed header: what the commit changed is undetermined.
                _ => MarkerCommit {
                    sha,
                    first_parent: None,
                    subject: header.to_string(),
                    paths: None,
                },
            };
            out.push(commit);
            continue;
        }
        let path = token.strip_prefix('\n').unwrap_or(token);
        if path.is_empty() {
            continue;
        }
        // A path before any header has no commit to attach to; it cannot make
        // any commit a fix.
        if let Some(p) = out.last_mut().and_then(|c| c.paths.as_mut()) {
            p.push(path.to_string());
        }
    }
    out
}

/// Whether `path` is a test path (backlog 45ea34e7 extends the 363ad7da rule
/// with filename conventions). Two shapes count:
///
/// - a directory component `tests` or `test` (the same rule
///   [`parse_test_cmd`] uses for committed test scripts), e.g.
///   `tests/a.sh`, `scripts/tests/b.py`;
/// - a FILENAME in a common test-file convention: `test_*.py`, `*_test.py`,
///   `*_test.rs`, `*_test.go`, `*.test.ts` / `*.test.js`, `*.spec.ts` /
///   `*.spec.js` (see [`is_test_filename`]), e.g. `scripts/test_alpha.py`,
///   `src/alpha_test.rs`.
///
/// The filename rule is narrow: the prefix/suffix must be followed by a
/// non-empty stem, and only the listed extensions match. Product files such
/// as `src/contest.rs`, `src/testing_utils.rs`, `scripts/latest.py` and
/// `src/test_utils.rs` are NOT test paths. Misreading a product file as a
/// test is the opposite fail direction: a real fix would be ignored and its
/// task handed out again.
fn is_test_path(path: &str) -> bool {
    let p = Path::new(path);
    let in_test_dir = p
        .parent()
        .map(|d| {
            d.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s == "tests" || s == "test"
            })
        })
        .unwrap_or(false);
    if in_test_dir {
        return true;
    }
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_test_filename)
}

/// Filename suffixes of the `*_<x>` / `*.<x>` test conventions (the stem in
/// front of the suffix must be non-empty). The `test_*.py` prefix convention
/// is checked separately in [`is_test_filename`].
const TEST_FILENAME_SUFFIXES: &[&str] = &[
    "_test.py", "_test.rs", "_test.go", ".test.ts", ".test.js", ".spec.ts", ".spec.js",
];

/// Whether a bare file name follows a test-file convention (see
/// [`is_test_path`]). `test_.py` (empty stem) and `latest.py` are not.
fn is_test_filename(name: &str) -> bool {
    if name.starts_with("test_") && name.ends_with(".py") && name.len() > "test_.py".len() {
        return true;
    }
    TEST_FILENAME_SUFFIXES.iter().any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|stem| !stem.is_empty())
    })
}

/// Version metadata a RED-filing commit bumps alongside its test
/// (bump-on-change). Counted as non-product only when every changed line in it
/// is a `version` line ([`version_lines_only`]).
fn is_version_metadata_path(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|n| n.to_str()),
        Some("Cargo.toml" | "Cargo.lock" | "plugin.json" | "marketplace.json")
    )
}

/// Whether `commit` (which carries a `(backlog <id>)` subject marker) is
/// OBSERVED to change product code, i.e. counts as the landed fix of `<id>`
/// (backlog 363ad7da).
///
/// A commit whose diff vs its first parent touches ONLY test paths
/// ([`is_test_path`]), the backlog store (`.backlog/`), and version metadata
/// whose changed lines are all `version` lines is a RED-filing / test-only
/// commit -- e.g. `test: tag RED(<id>) (backlog <id>)` -- and is NOT a fix.
/// An empty diff is likewise not a fix: no change was observed. A MERGE
/// commit is judged by its diff vs its first parent (what it brought into the
/// mainline): a merge that only brought a RED test is not a fix, while the
/// merged branch's own fix commits are reachable from HEAD and count on their
/// own.
///
/// Restrictive side when what a single commit changed cannot be determined
/// (a malformed log record, a failed `git diff-tree` of its metadata files):
/// it is NOT counted as a fix, so the task stays in the queue. Counting a task
/// as fixed silently drops it from `next` / `next --claim` -- an unanswered
/// question read as "done", the fail-open CLAUDE.md section 3 forbids --
/// while a wrong "not fixed" only hands the task out again, and
/// `next --claim` then re-runs its stored repro, which cancels the task when
/// the bug no longer reproduces. (A failure of the whole log query is
/// different: [`fixed_task_ids`] returns `Err` and every caller refuses.)
fn commit_changes_product(root: &Path, commit: &MarkerCommit) -> bool {
    let Some(paths) = commit.paths.as_ref() else {
        return false;
    };
    let mut metadata: Vec<&str> = Vec::new();
    for p in paths {
        if is_test_path(p) || p.starts_with(".backlog/") {
            continue;
        }
        if is_version_metadata_path(p) {
            metadata.push(p);
            continue;
        }
        return true;
    }
    if metadata.is_empty() {
        return false;
    }
    // Undetermined (`None`) resolves to "not a fix", per the rationale above.
    matches!(version_lines_only(root, commit, &metadata), Some(false))
}

/// Whether every added/removed line of `paths` in `commit` (vs its first
/// parent; vs the empty tree for a root commit) is a `version` line
/// (`version = "..."` or `"version": "..."`). `None` when git's answer could
/// not be obtained.
fn version_lines_only(root: &Path, commit: &MarkerCommit, paths: &[&str]) -> Option<bool> {
    let mut args: Vec<&str> = vec![
        "diff-tree",
        "-p",
        "-U0",
        "--no-color",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--root",
    ];
    if let Some(p) = commit.first_parent.as_deref() {
        args.push(p);
    }
    args.push(&commit.sha);
    args.push("--");
    args.extend_from_slice(paths);
    let diff = git_ok(root, &args).ok()?;
    Some(diff.lines().all(|l| {
        if l.starts_with("+++ ") || l.starts_with("--- ") {
            return true;
        }
        let Some(body) = l.strip_prefix('+').or_else(|| l.strip_prefix('-')) else {
            return true;
        };
        let b = body.trim();
        b.starts_with("version = \"") || b.starts_with("\"version\": \"")
    }))
}

/// The id a `(backlog <inner>)` marker names as fixed, per the grammar on
/// [`fixed_task_ids`]; `None` for an empty id or any unrecognised suffix.
fn fix_marker_id(inner: &str) -> Option<&str> {
    let inner = inner.trim();
    let end = inner
        .find(|c: char| c.is_whitespace() || c == ';')
        .unwrap_or(inner.len());
    let (id, rest) = inner.split_at(end);
    if id.is_empty() || id.contains(',') {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with(';') {
        return Some(id);
    }
    let wave = rest.strip_prefix("wave ")?;
    let digits_end = wave
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(wave.len());
    if digits_end == 0 {
        return None;
    }
    let tail = wave[digits_end..].trim();
    if tail.is_empty() || tail.starts_with(';') {
        Some(id)
    } else {
        None
    }
}

/// `Ok(())` only when HEAD is positively observed to be an unborn branch:
/// `git symbolic-ref -q HEAD` exits 0 naming a ref, no loose ref file exists
/// at that ref's path, and `git for-each-ref` exits 0 listing nothing and
/// writing nothing to stderr (git warns about a broken ref there). Any other
/// observation -- detached HEAD, an existing-but-unreadable ref file, a broken
/// packed entry, a git failure -- is `Err`, never "unborn".
fn head_is_unborn(root: &Path) -> Result<(), String> {
    let refname = match git_out(root, &["symbolic-ref", "-q", "HEAD"], &[0]) {
        Determination::Known((_, s)) => s.trim().to_string(),
        Determination::Undetermined(why) => {
            return Err(format!(
                "HEAD is not a symbolic ref (git symbolic-ref -q HEAD): {why}"
            ));
        }
    };
    if !refname.starts_with("refs/") || refname.len() <= "refs/".len() {
        return Err(format!("HEAD names {refname:?}, which is not a branch ref"));
    }
    let loose = git_ok(root, &["rev-parse", "--git-path", &refname])?;
    let loose = root.join(loose.trim());
    match loose.symlink_metadata() {
        Ok(_) => {
            return Err(format!(
                "the ref {refname} exists ({}) but does not resolve to a commit",
                loose.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!("cannot inspect {}: {e}", loose.display()));
        }
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["for-each-ref", "--format=%(refname)", &refname])
        .stdin(Stdio::null());
    let out = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out,
        Determination::Undetermined(why) => {
            return Err(format!("git for-each-ref {refname} failed: {why}"));
        }
    };
    let stderr = out.stderr().trim().to_string();
    let listed = match out.stdout_allowing(&[0]) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => {
            return Err(format!("git for-each-ref {refname} failed: {why}"));
        }
    };
    if !listed.trim().is_empty() || !stderr.is_empty() {
        return Err(format!(
            "the ref {refname} is present or broken (for-each-ref listed {:?}, stderr {:?})",
            listed.trim(),
            stderr
        ));
    }
    Ok(())
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

/// The only caller environment variables an evidence run inherits
/// (backlog a14b2fa2). A closure / repro record stores the command text only,
/// so a run whose outcome depended on any other variable in the caller's env
/// (e.g. `AUDIT_RUN_OPEN=1` un-skipping a test at the red rev) would record a
/// RED that re-running the stored command does not reproduce. Every run is
/// therefore executed with the environment cleared down to this fixed set —
/// what locating the toolchain, the user's git config and a temp dir needs —
/// so the recorded command alone reproduces the observation. (`done` no
/// longer runs RED — user ruling 2026-11-08 — so for a close this now bounds
/// the GREEN run: an env-gated test that only runs with an extra variable
/// skips at HEAD and is refused for reporting no `<N> passed`.)
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// Execute `cmd` in `dir` with the bound, under the [`INHERITED_ENV`]
/// environment only. `Undetermined` = no conclusion.
fn execute(cmd: &TestCmd, dir: &Path, timeout: Duration) -> Determination<RunResult> {
    let mut c = Command::new(&cmd.argv[0]);
    c.args(&cmd.argv[1..]).current_dir(dir).stdin(Stdio::null());
    c.env_clear();
    for key in INHERITED_ENV {
        if let Some(v) = std::env::var_os(key) {
            c.env(key, v);
        }
    }
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
    count_of(output, "passed")
}

/// Sum of every `<N> <word>` in `output` (the word may carry trailing
/// punctuation), or `None` when no such count appears at all.
fn count_of(output: &str, word: &str) -> Option<u64> {
    let words: Vec<&str> = output
        .split(|c: char| c.is_whitespace() || c == ';' || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let mut total: Option<u64> = None;
    for pair in words.windows(2) {
        if pair[1].starts_with(word) {
            if let Ok(n) = pair[0].parse::<u64>() {
                total = Some(total.unwrap_or(0).saturating_add(n));
            }
        }
    }
    total
}

/// Why an exit-0 repro run executed no test, or `None` when it is shown to
/// have executed at least one.
///
/// Output is "recognisably libtest" when it contains a `test result:` line.
/// Such a run executed zero tests iff the sum of `passed + failed` over ALL
/// its `test result:` lines (one per test binary) is 0, e.g. `0 passed; 0
/// failed; 1 ignored` (an `#[ignore]`d repro run without `-- --ignored`; the
/// caller classifies that as undeterminable, see `ignored_in_results`) or
/// `... 1 filtered out` (a filter matching nothing). A `cargo test` run with
/// no `test result:` line at all shows no test ran either. Non-cargo output
/// that is not libtest is left alone (`None`): pytest / scripts keep their
/// exit-code polarity.
fn zero_tests_ran(cmd: &TestCmd, output: &str) -> Option<String> {
    let results: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("test result:"))
        .collect();
    if results.is_empty() {
        return match cmd.runner {
            Runner::CargoTest => Some(
                "cargo exited 0 without any `test result:` line, so no test is shown to have run"
                    .to_string(),
            ),
            _ => None,
        };
    }
    let ran: u64 = results
        .iter()
        .map(|l| {
            passed_count(l)
                .unwrap_or(0)
                .saturating_add(count_of(l, "failed").unwrap_or(0))
        })
        .fold(0u64, u64::saturating_add);
    if ran == 0 {
        Some(format!(
            "exit 0 but ZERO tests ran (passed + failed = 0 over {} `test result:` line(s)); \
             a run that executed no test is not a not-reproduced observation. An `#[ignore]`d \
             repro must be run with `-- --ignored` (e.g. `cargo test -p <crate> --test <file> \
             -- --ignored`), and a test-name filter must match the repro",
            results.len()
        ))
    } else {
        None
    }
}

/// Sum of `N ignored` over every `test result:` line of `output`: how many
/// tests exist but were skipped as `#[ignore]`d. 0 when none is reported.
fn ignored_in_results(output: &str) -> u64 {
    output
        .lines()
        .filter(|l| l.contains("test result:"))
        .map(|l| count_of(l, "ignored").unwrap_or(0))
        .fold(0u64, u64::saturating_add)
}

/// Sum of `N filtered out` over every `test result:` line of `output`: how
/// many tests exist but were not selected by the run's filter. 0 when none.
fn filtered_out_in_results(output: &str) -> u64 {
    output
        .lines()
        .filter(|l| l.contains("test result:"))
        .map(|l| count_of(l, "filtered").unwrap_or(0))
        .fold(0u64, u64::saturating_add)
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

/// Is a non-zero run a BEHAVIOURAL failure (the test ran and failed)? This is
/// `classify_red` of earlier builds, split three ways. Used by the repro run
/// at HEAD ([`run_repro`] / [`run_repro_classified`]: `add --repro-test`,
/// `edit --repro-test`, the `next --claim` re-check, `recheck`); `done` no
/// longer calls it, because it no longer runs RED (user ruling 2026-11-08).
///
/// For `cargo test`, a `test result: FAILED` line is checked **before** the
/// build-failure marker: that line is printed only by a test binary that ran
/// and had a failing test, so it is a behavioural RED even if an unrelated
/// package's build marker (e.g. "could not compile") also appears in the same
/// output (GitHub 1347). A cargo run with the marker and no FAILED line is
/// still refused as a build failure. For pytest and script runners the
/// build-failure marker is checked first, as before: they have no equivalent
/// "this test ran and failed" line to outrank it.
///
/// What a completed NON-ZERO run of a repro showed (user ruling 2026-11-13,
/// 「保留はありえない実証されないならclose,実証されたらopen」). Three answers:
///   - `Red`: the test ran and failed behaviourally — the defect is demonstrated.
///   - `NotDemonstrated`: the run completed and did not demonstrate the defect,
///     and the reason is a property of the repro or its environment that a
///     re-run would show again (the stored script declared its environment
///     unavailable with exit 2, the script/command could not even be started
///     with 126/127, pytest found no such test with 4/5). A ticket whose repro
///     is not demonstrated is CLOSED (`next --claim` / `recheck` auto-cancel).
///   - `Undeterminable`: the run did not show anything about the defect for
///     an INFRASTRUCTURE reason (killed by a signal, a build/compile failure
///     — a broken HEAD must never mass-close tickets — or a non-zero cargo /
///     pytest run with no recognisable test result). Never closes a ticket:
///     the caller refuses (CLAUDE.md §3, undetermined is not clean).
#[derive(Debug, Clone, PartialEq, Eq)]
enum NonZero {
    Red,
    NotDemonstrated(String),
    Undeterminable(String),
}

fn classify_nonzero(cmd: &TestCmd, r: &RunResult) -> NonZero {
    let build_failure = |m: &str| {
        NonZero::Undeterminable(format!(
            "the run failed at BUILD/compile time (output contains {m:?}), not behaviourally; a \
             build failure is not RED. Restructure the test so the pre-fix revision builds and \
             fails an assertion; if no behavioural RED can be shown, the item is cancelled \
             (`backlog cancel ID --reason R`)"
        ))
    };
    if !matches!(cmd.runner, Runner::CargoTest) {
        if let Some(m) = build_failure_marker(&r.output) {
            return build_failure(m);
        }
    }
    match cmd.runner {
        Runner::CargoTest => {
            if r.output.contains("test result: FAILED") {
                NonZero::Red
            } else if let Some(m) = build_failure_marker(&r.output) {
                build_failure(m)
            } else {
                // No "test result: FAILED" and no build marker. The test didn't
                // produce a conclusive failure output.
                NonZero::Undeterminable(format!(
                    "cargo exited {} without a `test result: FAILED` line, so no test is shown \
                     to have run and failed (a build/compile failure is not RED; if the RED \
                     cannot be behavioural, the item is cancelled)",
                    r.exit
                ))
            }
        }
        Runner::Pytest => match r.exit {
            1 => NonZero::Red,
            4 | 5 => NonZero::NotDemonstrated(format!(
                "pytest exited {} (4 = usage error such as a test path/node that does not exist, \
                 5 = no tests collected), so the repro no longer demonstrates the defect",
                r.exit
            )),
            _ => NonZero::Undeterminable(format!(
                "pytest exited {} (only 1 means tests ran and failed; 2 = interrupted / \
                 collection error, 3 = internal error), not RED",
                r.exit
            )),
        },
        Runner::Script => match r.exit {
            2 => NonZero::NotDemonstrated(
                "the script exited 2, which by this repo's script convention means its \
                 environment is UNAVAILABLE (it could not set up the observation), not that the \
                 defect was shown; a repro that is not demonstrated is not RED"
                    .to_string(),
            ),
            126 | 127 => NonZero::NotDemonstrated(format!(
                "the script exited {} (not executable / command not found): the repro cannot \
                 even be started, which is not a behavioural failure",
                r.exit
            )),
            e if e >= 128 => NonZero::Undeterminable(format!(
                "the script exited {e} (killed by a signal), which is not a behavioural failure"
            )),
            _ => NonZero::Red,
        },
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

/// The evidence of a `done --test` close: the observed GREEN, plus the
/// CLAIMED red revision (never executed, see [`run_green_with_claimed_red`]).
#[derive(Debug, Clone)]
pub struct TestClose {
    pub green: GreenRun,
    pub red: RedRun,
}

/// The `closure.red.note` of a close whose RED was not executed.
pub const RED_NOT_EXECUTED_NOTE: &str = "RED was NOT executed: backlog done's RED run was \
     abolished by user ruling 2026-11-08 (「backlog doneのRED実行はそもそも全面廃止でいい」); \
     rev is the caller's claim, resolved to a commit and checked to be an ancestor of HEAD, \
     never run";

/// Run a `done --test` close: GREEN at HEAD in the (clean) working tree.
///
/// The RED run is ABOLISHED (user ruling 2026-11-08: 「backlog doneのRED実行は
/// そもそも全面廃止でいい」): no temp worktree is created, no test file is
/// overlaid and `cmd` is never executed at `red_rev`. `red_rev` is still
/// resolved to a commit and required to be an ancestor of HEAD (cheap git
/// queries, no build) — either failure refuses — and is recorded as a CLAIM
/// only: `RedRun { executed: Some(false), kind: "not-executed", exit: None }`,
/// so the closure can never be read as a verified RED. Every failure to
/// observe GREEN refuses with its cause, exactly as before.
pub fn run_green_with_claimed_red(
    root: &Path,
    cmd: &TestCmd,
    red_rev: &str,
    now: i64,
) -> Result<TestClose, String> {
    let timeout = test_timeout()?;
    let head = head_commit(root)?;
    let red = resolve_commit(root, red_rev).map_err(|e| format!("--red-rev: {e}"))?;
    if !is_ancestor(root, &red, &head)? {
        return Err(format!(
            "--red-rev {red} is not an ancestor of HEAD ({head}); the claimed pre-fix revision \
             must be on this history"
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

    // Nothing the run did may have moved HEAD or dirtied the tree.
    let head_after = head_commit(root)?;
    if head_after != head {
        return Err(format!(
            "HEAD moved during the test run ({head} -> {head_after}); the observation is not about \
             a single commit"
        ));
    }
    require_clean_tree(root).map_err(|e| format!("after the test run: {e}"))?;

    Ok(TestClose {
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
            exit: None,
            kind: RED_KIND_NOT_EXECUTED.to_string(),
            overlay: None,
            executed: Some(false),
            note: Some(RED_NOT_EXECUTED_NOTE.to_string()),
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

// ---- findings: repro -------------------------------------------------------

/// Run a finding's repro test at HEAD and record the outcome. Polarity: a
/// repro that FAILS behaviourally reproduces the bug (same as a RED test);
/// exit 0 is not-reproduced only when a test is shown to have run (see
/// [`zero_tests_ran`]: an exit-0 libtest run with `passed + failed = 0`, or a
/// `cargo test` exit 0 with no `test result:` line, is undetermined);
/// for a STORED-repro re-run, a zero-test run reporting `N filtered out`
/// (N >= 1) is `Undeterminable` (never auto-discarded; ruling 2026-11-27);
/// everything that cannot be run to a conclusion is undetermined, with the
/// cause in `detail`. Never returns an error: the
/// caller (`add`) files a task only on `reproduced` and writes nothing for
/// any other outcome.
pub fn run_repro(cwd: &Path, text: &str, now: i64) -> Repro {
    run_repro_classified(cwd, text, now).repro
}

/// The verdict of re-running a STORED repro (`next --claim`, `backlog
/// recheck`; user ruling 2026-11-13 「保留はありえない実証されないならclose,
/// 実証されたらopen」). A row stays open only while its repro is
/// `Reproduced`. `NotReproduced` and `NotDemonstrated` close it (auto-cancel);
/// `Undeterminable` never does — the caller refuses and reports (CLAUDE.md §3).
///
/// The boundary between the last two is: NOT DEMONSTRATED = the repro itself
/// ran (or could not even be started) and its own answer is "the defect is
/// not shown" — exit 0 with zero tests run (and none `#[ignore]`d: ignored
/// tests that were skipped are UNDETERMINABLE below), a stored script that exits 2
/// (its environment is unavailable: e.g. "no v9fs dir"), 126/127 (cannot be
/// started), pytest 4/5, a stored command that no longer parses / is not on
/// the allowlist, a script no longer tracked at HEAD. UNDETERMINABLE = the
/// harness or host failed to carry the run to an answer — an exit-0 run whose
/// tests were all skipped as `#[ignore]`d (`N ignored`, N >= 1; the repro
/// lacks `-- --ignored`), a timeout (e.g. a
/// cargo lock wait), a signal, a spawn failure, a git query failure, a dirty
/// checkout, a bad `BACKLOG_TEST_TIMEOUT_SECS`, a build/compile failure (a
/// broken HEAD must never mass-close tickets), or a non-zero cargo/pytest run
/// without a recognisable test result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckVerdict {
    Reproduced,
    NotReproduced,
    NotDemonstrated,
    Undeterminable,
}

impl RecheckVerdict {
    pub fn label(self) -> &'static str {
        match self {
            RecheckVerdict::Reproduced => "reproduced",
            RecheckVerdict::NotReproduced => "not-reproduced",
            RecheckVerdict::NotDemonstrated => "not-demonstrated",
            RecheckVerdict::Undeterminable => "undeterminable",
        }
    }

    /// Whether this verdict closes the row (auto-cancel).
    pub fn closes(self) -> bool {
        matches!(
            self,
            RecheckVerdict::NotReproduced | RecheckVerdict::NotDemonstrated
        )
    }
}

/// A repro run with its [`RecheckVerdict`]. `repro.outcome` keeps the three
/// stored outcomes (`add` / `edit --repro-test` file only `reproduced`, so
/// `not-demonstrated` and `undeterminable` both appear there as
/// `undetermined`); the verdict carries the finer split. Nothing new is
/// serialised.
#[derive(Debug, Clone)]
pub struct ClassifiedRepro {
    pub repro: Repro,
    pub verdict: RecheckVerdict,
    /// The exit code of a run that completed, `None` when none did.
    pub exit: Option<i32>,
}

/// [`run_repro`] with the [`RecheckVerdict`]. When a run completed, `detail`
/// always starts `exit N` and carries the output tail.
pub fn run_repro_classified(cwd: &Path, text: &str, now: i64) -> ClassifiedRepro {
    let mk = |outcome: &str, detail: String, rev: Option<String>| Repro {
        outcome: outcome.to_string(),
        cmd: text.to_string(),
        rev,
        observed_at: now,
        detail,
    };
    let undet =
        |verdict: RecheckVerdict, detail: String, rev: Option<String>, exit| ClassifiedRepro {
            repro: mk(REPRO_UNDETERMINED, detail, rev),
            verdict,
            exit,
        };
    let cmd = match parse_test_cmd(text) {
        Ok(c) => c,
        // The stored command cannot even be started.
        Err(e) => return undet(RecheckVerdict::NotDemonstrated, e, None, None),
    };
    let timeout = match test_timeout() {
        Ok(t) => t,
        Err(e) => return undet(RecheckVerdict::Undeterminable, e, None, None),
    };
    let root = match repo_toplevel(cwd) {
        Ok(r) => r,
        Err(e) => return undet(RecheckVerdict::Undeterminable, e, None, None),
    };
    let head = match head_commit(&root) {
        Ok(h) => h,
        Err(e) => return undet(RecheckVerdict::Undeterminable, e, None, None),
    };
    if let Err(e) = require_clean_tree(&root) {
        return undet(RecheckVerdict::Undeterminable, e, Some(head), None);
    }
    if let Some(script) = &cmd.script {
        match tracked_at(&root, &head, script) {
            Ok(true) => {}
            Ok(false) => {
                return undet(
                    RecheckVerdict::NotDemonstrated,
                    format!(
                        "script {script} is not committed at HEAD ({head}); only a git-tracked, \
                         committed test script is evidence, so the stored repro cannot even be \
                         started"
                    ),
                    Some(head),
                    None,
                )
            }
            Err(e) => return undet(RecheckVerdict::Undeterminable, e, Some(head), None),
        }
    }
    let r = match execute(&cmd, &root, timeout) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => {
            return undet(
                RecheckVerdict::Undeterminable,
                format!("the repro run did not complete: {why}"),
                Some(head),
                None,
            )
        }
    };
    let tail = excerpt_short(&r.output);
    if r.exit == 0 {
        if let Some(why) = zero_tests_ran(&cmd, &r.output) {
            // Tests exist but were all skipped as `#[ignore]`d: the run
            // observed nothing about the defect, and the row's repro is
            // simply mis-specified (missing `-- --ignored`). That is
            // undetermined, never a close (CLAUDE.md §3).
            let ignored = ignored_in_results(&r.output);
            if ignored >= 1 {
                return undet(
                    RecheckVerdict::Undeterminable,
                    format!(
                        "exit 0 but ZERO tests ran: {ignored} test(s) exist but were skipped as \
                         `#[ignore]`d, so nothing was observed; run the repro with `-- --ignored` \
                         (e.g. `cargo test -p <crate> --test <file> -- --ignored`): {tail}"
                    ),
                    Some(head),
                    Some(0),
                );
            }
            // `N filtered out` (N >= 1): the named test EXISTS but the filter did
            // not select it (typically its `#[ignore]` was removed by the fix, so
            // `-- --ignored` now skips it). The run executed no test and observed
            // nothing about the defect: undetermined, never a discard (user
            // ruling 2026-11-27, CLAUDE.md §3). The no-`filtered out` zero-run
            // shape stays NotDemonstrated below.
            let filtered = filtered_out_in_results(&r.output);
            if filtered >= 1 {
                return undet(
                    RecheckVerdict::Undeterminable,
                    format!(
                        "exit 0 but ZERO tests ran: {filtered} test(s) were `filtered out`, so \
                         nothing was observed (the repro's test exists but was not selected; \
                         e.g. its `#[ignore]` was removed while the repro still passes \
                         `-- --ignored`); fix the stored repro command: {tail}"
                    ),
                    Some(head),
                    Some(0),
                );
            }
            return undet(
                RecheckVerdict::NotDemonstrated,
                format!("exit 0: {why}: {tail}"),
                Some(head),
                Some(0),
            );
        }
        return ClassifiedRepro {
            repro: mk(REPRO_NOT_REPRODUCED, format!("exit 0: {tail}"), Some(head)),
            verdict: RecheckVerdict::NotReproduced,
            exit: Some(0),
        };
    }
    match classify_nonzero(&cmd, &r) {
        NonZero::Red => ClassifiedRepro {
            repro: mk(
                REPRO_REPRODUCED,
                format!("exit {}: {tail}", r.exit),
                Some(head),
            ),
            verdict: RecheckVerdict::Reproduced,
            exit: Some(r.exit),
        },
        NonZero::NotDemonstrated(why) => undet(
            RecheckVerdict::NotDemonstrated,
            format!(
                "exit {} is not a behavioural failure: {why}: {tail}",
                r.exit
            ),
            Some(head),
            Some(r.exit),
        ),
        NonZero::Undeterminable(why) => undet(
            RecheckVerdict::Undeterminable,
            format!(
                "exit {} is not a behavioural failure: {why}: {tail}",
                r.exit
            ),
            Some(head),
            Some(r.exit),
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

    // `test_path_matches_the_gate_predicate` was removed with `is_test_path`:
    // that predicate only chose which HEAD test files the RED worktree got,
    // and the RED run is abolished (user ruling 2026-11-08: 「backlog doneの
    // RED実行はそもそも全面廃止でいい」), so nothing in this crate uses it.

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
