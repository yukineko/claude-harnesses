//! Check history, failure fingerprints and `sitecraft status` (rulings D11).
//!
//! # History
//!
//! `decide check`, `check pages`, `check security` and `check capability
//! <name>` — from the CLI and from `sitecraft gate` alike — run through
//! [`recorded`], which appends one JSON line per run to
//! `<state>/check-history/<root-key>.jsonl` (`<root-key>` as in
//! [`crate::capability::results::root_key`]):
//! `{"name","exit","fingerprint","elapsed_ms","timed_out","at"}` with `name`
//! = `decide` / `pages` / `security` / `capability:<name>`. A line that cannot
//! be appended (no state dir, unresolvable root, unreadable clock, IO
//! failure) makes that check exit 2: the run is reported undetermined.
//!
//! In the gate the check's report lines are captured (not printed) with the
//! same text the CLI prints, so the gate and the CLI fingerprint the same
//! failure alike. `elapsed_ms` of the gate's `pages` line covers the dev
//! server start and stop around the check, because the gate runs them as one
//! step.
//!
//! # Fingerprint
//!
//! [`fingerprint`]: exit 0 → `null`; a run during which a time limit expired
//! (see [`note_timeout`]) → `sha256("timeout")`; otherwise the sha256 hex of
//! the check's stdout followed by its stderr after [`normalize`].
//!
//! # `sitecraft status`
//!
//! [`status`] reads the history and prints
//! `{"checks":[{"name","last_exit","consecutive_same_failure","elapsed_ms","state"}]}`
//! (one entry per name, sorted by name). The state is the first of
//! `waiting-human` (the check needs a trust that is missing now: `pages` and
//! `builtin:threejs-render` capabilities need the `sitecraft.toml` trust,
//! `command` capabilities the D2-trust digest), `stuck`
//! (`consecutive_same_failure >= 3`), `failing` (last exit ≠ 0), `slow`
//! (last exit 0 and `elapsed_ms` at least half the check's time limit;
//! `decide` has none), `ok`. Exit 0 when every check is `ok` / `slow`, 1 when
//! any is `failing` / `stuck` / `waiting-human`, 2 (nothing on stdout) when
//! the state dir is unknown, the history is absent, empty or unreadable, a
//! line does not parse or breaks the line contract, or a trust / time limit
//! needed for a state cannot be determined.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, Loaded};
use crate::decide::capability::Verify;
use crate::decide::record;
use crate::trust::{self, Trust};
use crate::{Determination, Verdict, EXIT_UNDETERMINED};

/// Sub-directory of the state dir holding the history files.
pub const HISTORY_DIR: &str = "check-history";

/// Number of consecutive same-fingerprint failures that is `stuck`.
pub const STUCK_AT: u64 = 3;

/// Time limits that expired in this process so far (see [`note_timeout`]).
static TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// Record that a time limit expired (a subprocess was killed at its
/// deadline, a socket read passed its limit, …). [`recorded`] marks a run
/// `timed_out` when this was called while the run was in progress.
pub fn note_timeout() {
    TIMEOUTS.fetch_add(1, Ordering::SeqCst);
}

fn timeouts() -> u64 {
    TIMEOUTS.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// output capture
// ---------------------------------------------------------------------------

/// The output of one check: kept for the fingerprint and, when printing,
/// written to the process's stdout / stderr as it is produced.
#[derive(Debug, Default)]
pub struct Output {
    echo: bool,
    stdout: String,
    stderr: String,
}

impl Output {
    /// Print each line and keep it (the CLI).
    #[must_use]
    pub fn printing() -> Self {
        Output {
            echo: true,
            ..Output::default()
        }
    }

    /// Keep each line without printing it (the gate prints its own decision).
    #[must_use]
    pub fn capturing() -> Self {
        Output::default()
    }

    /// One stdout line.
    pub fn out(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if self.echo {
            println!("{line}");
        }
        self.stdout.push_str(line);
        self.stdout.push('\n');
    }

    /// One stderr line.
    pub fn err(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if self.echo {
            eprintln!("{line}");
        }
        self.stderr.push_str(line);
        self.stderr.push('\n');
    }

    /// The verdict line of [`crate::report`] (`<label>: clean` /
    /// `VIOLATION - …` / `UNDETERMINED - …`) on stderr.
    pub fn report(&mut self, label: &str, v: &Verdict) {
        match v {
            Verdict::Clean(_) => self.err(format!("{label}: clean")),
            Verdict::Violation(r) => self.err(format!("{label}: VIOLATION - {}", r.as_str())),
            Verdict::Undetermined(u) => {
                self.err(format!("{label}: UNDETERMINED - {}", u.as_str()));
            }
        }
    }

    /// stdout followed by stderr (the fingerprint input).
    #[must_use]
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

// ---------------------------------------------------------------------------
// fingerprint
// ---------------------------------------------------------------------------

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

struct Patterns {
    time: Regex,
    path: Regex,
    digits: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // RFC 3339 / ISO 8601 date-time (date, `T` or space, time, optional
        // fraction and zone).
        time: compile(
            r"[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt ][0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:[.,][0-9]+)?)?(?:[Zz]|[+-][0-9]{2}(?::?[0-9]{2})?)?",
        ),
        // An absolute path: `/` at the start or after a separator, up to
        // the next whitespace.
        path: compile(r#"(^|[\s"'`(\[{=:,;<>])/\S*"#),
        digits: compile(r"[0-9]+"),
    })
}

/// The three patterns are constants; a failure to compile one is a bug in
/// this file, caught by the unit tests.
#[allow(clippy::expect_used)]
fn compile(re: &str) -> Regex {
    Regex::new(re).expect("constant regex compiles")
}

/// The D11 normalization, in this order: RFC 3339 / ISO 8601 times →
/// `<time>`, absolute paths → `<path>`, digit runs → `0`.
#[must_use]
pub fn normalize(text: &str) -> String {
    let p = patterns();
    let t = p.time.replace_all(text, "<time>");
    let t = p.path.replace_all(&t, "${1}<path>");
    p.digits.replace_all(&t, "0").into_owned()
}

/// The D11 fingerprint of one run (module docs).
#[must_use]
pub fn fingerprint(exit: i32, timed_out: bool, text: &str) -> Option<String> {
    if exit == 0 {
        None
    } else if timed_out {
        Some(sha256_hex(b"timeout"))
    } else {
        Some(sha256_hex(normalize(text).as_bytes()))
    }
}

// ---------------------------------------------------------------------------
// history lines
// ---------------------------------------------------------------------------

/// One history line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `decide` / `pages` / `security` / `capability:<name>`.
    pub name: String,
    /// The check's exit code.
    pub exit: i64,
    /// `None` on exit 0, else a sha256 hex.
    pub fingerprint: Option<String>,
    /// Wall time of the run.
    pub elapsed_ms: u64,
    /// A time limit expired during the run.
    pub timed_out: bool,
    /// When the run finished (RFC 3339, UTC).
    pub at: String,
}

/// `<state>/check-history/<root-key>.jsonl`. `Err` when the root cannot be
/// canonicalized.
pub fn history_path(state: &Path, root: &Path) -> Result<PathBuf, String> {
    let key = crate::capability::results::root_key(root)?;
    Ok(state.join(HISTORY_DIR).join(format!("{key}.jsonl")))
}

/// Append `e` as one line (creating the directory). Any failure is `Err`.
pub fn append(state: &Path, root: &Path, e: &Entry) -> Result<(), String> {
    let path = history_path(state, root)?;
    let dir = state.join(HISTORY_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let line = serde_json::to_string(e).map_err(|err| format!("cannot encode the line: {err}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    f.write_all(format!("{line}\n").as_bytes())
        .map_err(|err| format!("cannot append to {}: {err}", path.display()))
}

/// Run one check, then append its history line.
///
/// `run` prints the check's report into `out` and returns its verdict. The
/// fingerprint is taken from what `run` wrote. When the line cannot be
/// appended (`state` is `Err`, or [`append`] fails) an `UNDETERMINED` line
/// naming the cause is written to `out` and the result is `Undetermined`
/// whatever `run` returned: the check exits 2.
pub fn recorded(
    state: Result<&Path, &str>,
    root: &Path,
    name: &str,
    label: &str,
    out: &mut Output,
    run: impl FnOnce(&mut Output) -> Verdict,
) -> Verdict {
    let before = timeouts();
    let start = Instant::now();
    let v = run(out);
    let elapsed = start.elapsed();
    let timed_out = timeouts() != before;
    let exit = crate::exit_code(&v);
    let written = state.map_err(str::to_string).and_then(|state| {
        let at = record::now_utc_rfc3339()?;
        let e = Entry {
            name: name.to_string(),
            exit: i64::from(exit),
            fingerprint: fingerprint(exit, timed_out, &out.text()),
            elapsed_ms: millis(elapsed),
            timed_out,
            at,
        };
        append(state, root, &e)
    });
    match written {
        Ok(()) => v,
        Err(e) => {
            let u = Verdict::undetermined(format!(
                "the check history cannot be appended ({e}), so `sitecraft status` could never see this run (its own result was exit {exit}); make the sitecraft state dir writable and rerun"
            ));
            out.report(label, &u);
            u
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// The state of one check (priority order, highest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Needs a human `sitecraft trust`.
    WaitingHuman,
    /// The same failure [`STUCK_AT`] or more times in a row.
    Stuck,
    /// The last run failed.
    Failing,
    /// The last run passed but took at least half its time limit.
    Slow,
    /// The last run passed.
    Ok,
}

impl State {
    /// `failing` / `stuck` / `waiting-human` make `status` exit 1.
    #[must_use]
    pub fn is_problem(self) -> bool {
        matches!(self, State::WaitingHuman | State::Stuck | State::Failing)
    }
}

/// One `checks` entry of `sitecraft status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckState {
    /// The history name.
    pub name: String,
    /// Exit code of the newest line.
    pub last_exit: i64,
    /// Newest lines (counted back) that failed with the newest fingerprint.
    pub consecutive_same_failure: u64,
    /// `elapsed_ms` of the newest line.
    pub elapsed_ms: u64,
    /// See [`State`].
    pub state: State,
}

/// The `sitecraft status` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// One entry per check name, sorted by name.
    pub checks: Vec<CheckState>,
}

impl Status {
    /// 1 when any check is a problem ([`State::is_problem`]), else 0.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.checks.iter().any(|c| c.state.is_problem()) {
            crate::EXIT_VIOLATION
        } else {
            crate::EXIT_CLEAN
        }
    }
}

/// What trust a check needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Nothing,
    Config,
    CommandDigest,
}

/// Which check a history name is.
enum Kind {
    Decide,
    Pages,
    Security,
    Capability(String),
}

fn kind_of(name: &str) -> Option<Kind> {
    match name {
        "decide" => Some(Kind::Decide),
        "pages" => Some(Kind::Pages),
        "security" => Some(Kind::Security),
        _ => name
            .strip_prefix("capability:")
            .filter(|n| !n.is_empty())
            .map(|n| Kind::Capability(n.to_string())),
    }
}

/// Parse the history text. Any unparseable line, unknown name or a line
/// whose fingerprint contradicts its exit is `Err`.
fn parse_history(path: &Path, text: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = || format!("{} line {}", path.display(), i + 1);
        let e: Entry =
            serde_json::from_str(line).map_err(|err| format!("{} does not parse ({err})", at()))?;
        if kind_of(&e.name).is_none() {
            return Err(format!("{}: unknown check name {:?}", at(), e.name));
        }
        match (&e.fingerprint, e.exit) {
            (None, 0) => {}
            (Some(f), x) if x != 0 && f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => {
                return Err(format!(
                    "{}: exit {} with fingerprint {:?} (exit 0 needs null, any other exit a sha256 hex)",
                    at(),
                    e.exit,
                    e.fingerprint
                ))
            }
        }
        out.push(e);
    }
    if out.is_empty() {
        return Err(format!(
            "{} has no history line; run the checks first",
            path.display()
        ));
    }
    Ok(out)
}

/// Lazily computed facts the states depend on.
struct Facts<'a> {
    state: &'a Path,
    root: &'a Path,
    config: Option<Result<(bool, Duration), String>>,
    caps: Option<Result<BTreeMap<String, crate::decide::capability::Capability>, String>>,
    digest_trusted: Option<Result<bool, String>>,
}

impl Facts<'_> {
    /// `(sitecraft.toml trusted, check pages deadline)`.
    fn config(&mut self) -> Result<(bool, Duration), String> {
        let (state, root) = (self.state, self.root);
        self.config
            .get_or_insert_with(|| {
                let path = config::path_for(root);
                match config::load(root) {
                    Loaded::Absent => Err(format!(
                        "{} does not exist, so whether it is trusted cannot be told",
                        path.display()
                    )),
                    Loaded::Undetermined(why) => Err(why),
                    Loaded::Config(cfg, bytes) => {
                        let limit = crate::check::pages::deadline_for(cfg.pages.len());
                        match trust::check_bytes(state, root, &path, &bytes) {
                            Trust::Trusted => Ok((true, limit)),
                            Trust::Untrusted(_) => Ok((false, limit)),
                            Trust::Undetermined(why) => Err(why),
                        }
                    }
                }
            })
            .clone()
    }

    fn caps(&mut self) -> Result<BTreeMap<String, crate::decide::capability::Capability>, String> {
        let root = self.root;
        self.caps
            .get_or_insert_with(|| match record::load(root) {
                Determination::Known(None) => Ok(BTreeMap::new()),
                Determination::Known(Some(d)) => d.parsed_capabilities(),
                Determination::Undetermined(why) => Err(why.as_str().to_string()),
            })
            .clone()
    }

    fn digest_trusted(&mut self) -> Result<bool, String> {
        if let Some(r) = &self.digest_trusted {
            return r.clone();
        }
        let r = self.caps().and_then(|caps| {
            match crate::decide::capability::command_digest(&caps) {
                // A command capability always yields a digest.
                None => Err("no command capability found for the D2-trust digest".to_string()),
                Some(d) => match trust::check_capabilities(self.state, self.root, &d) {
                    Trust::Trusted => Ok(true),
                    Trust::Untrusted(_) => Ok(false),
                    Trust::Undetermined(why) => Err(why),
                },
            }
        });
        self.digest_trusted = Some(r.clone());
        r
    }

    /// The trust a check needs and its time limit (`None` = no limit).
    fn need_and_limit(&mut self, kind: &Kind) -> Result<(Need, Option<Duration>), String> {
        match kind {
            Kind::Decide => Ok((Need::Nothing, None)),
            Kind::Security => Ok((Need::Nothing, Some(crate::check::security::AUDIT_TIMEOUT))),
            Kind::Pages => {
                let (_, limit) = self.config()?;
                Ok((Need::Config, Some(limit)))
            }
            Kind::Capability(n) => {
                let limit = Some(crate::capability::CHECK_LIMIT);
                // A capability not (or no longer) registered, or without a
                // verify table, is refused before any trust is consulted.
                let need = match self.caps()?.get(n).and_then(|c| c.verify.as_ref()) {
                    Some(Verify::Threejs { .. }) => Need::Config,
                    Some(Verify::Command { .. }) => Need::CommandDigest,
                    Some(Verify::Blender { .. }) | None => Need::Nothing,
                };
                Ok((need, limit))
            }
        }
    }

    fn waiting(&mut self, need: Need) -> Result<bool, String> {
        match need {
            Need::Nothing => Ok(false),
            Need::Config => self.config().map(|(trusted, _)| !trusted),
            Need::CommandDigest => self.digest_trusted().map(|trusted| !trusted),
        }
    }
}

/// Newest lines (counted back) with exit ≠ 0 and the newest fingerprint.
fn consecutive_same_failure(lines: &[&Entry]) -> u64 {
    let Some(newest) = lines.last() else {
        return 0;
    };
    if newest.exit == 0 {
        return 0;
    }
    let n = lines
        .iter()
        .rev()
        .take_while(|e| e.exit != 0 && e.fingerprint == newest.fingerprint)
        .count();
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The `sitecraft status` document for `root` (module docs). `Undetermined`
/// for every exit-2 case.
pub fn status(state: &Path, root: &Path) -> Determination<Status> {
    let path = match history_path(state, root) {
        Ok(p) => p,
        Err(e) => return Determination::undetermined(e),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::undetermined(format!(
                "no check history at {}; run `sitecraft decide check` and the other checks first",
                path.display()
            ))
        }
        Err(e) => {
            return Determination::undetermined(format!("cannot read {}: {e}", path.display()))
        }
    };
    let entries = match parse_history(&path, &text) {
        Ok(e) => e,
        Err(e) => return Determination::undetermined(e),
    };
    let mut by_name: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in &entries {
        by_name.entry(e.name.as_str()).or_default().push(e);
    }
    let mut facts = Facts {
        state,
        root,
        config: None,
        caps: None,
        digest_trusted: None,
    };
    let mut checks = Vec::new();
    for (name, lines) in by_name {
        let Some(newest) = lines.last() else {
            continue;
        };
        let Some(kind) = kind_of(name) else {
            return Determination::undetermined(format!("unknown check name {name:?}"));
        };
        let decided = facts
            .need_and_limit(&kind)
            .and_then(|(need, limit)| facts.waiting(need).map(|w| (w, limit)));
        let (waiting, limit) = match decided {
            Ok(x) => x,
            Err(e) => {
                return Determination::undetermined(format!(
                    "the state of `{name}` cannot be determined: {e}"
                ))
            }
        };
        let same = consecutive_same_failure(&lines);
        let slow = limit.is_some_and(|l| newest.elapsed_ms.saturating_mul(2) >= millis(l));
        let state = if waiting {
            State::WaitingHuman
        } else if same >= STUCK_AT {
            State::Stuck
        } else if newest.exit != 0 {
            State::Failing
        } else if slow {
            State::Slow
        } else {
            State::Ok
        };
        checks.push(CheckState {
            name: name.to_string(),
            last_exit: newest.exit,
            consecutive_same_failure: same,
            elapsed_ms: newest.elapsed_ms,
            state,
        });
    }
    Determination::known(Status { checks })
}

/// `sitecraft status`: the JSON on stdout and exit 0 / 1, or the reason on
/// stderr and exit 2 (nothing on stdout, so an empty `checks` list is never
/// shown as "no problem").
#[must_use]
pub fn cmd_status(state: &Path, root: &Path) -> i32 {
    match status(state, root) {
        Determination::Known(s) => match serde_json::to_string(&s) {
            Ok(json) => {
                println!("{json}");
                s.exit_code()
            }
            Err(e) => {
                eprintln!("sitecraft status: UNDETERMINED - cannot encode the status: {e}");
                EXIT_UNDETERMINED
            }
        },
        Determination::Undetermined(why) => {
            eprintln!("sitecraft status: UNDETERMINED - {}", why.as_str());
            EXIT_UNDETERMINED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_replaces_times_then_paths_then_digits() {
        assert_eq!(
            normalize("at 2026-11-03T10:00:00Z in /tmp/a1/b.txt: 42 items, id x7"),
            "at <time> in <path> 0 items, id x0"
        );
        assert_eq!(normalize("a/b and/or 1/2"), "a/b and/or 0/0");
        assert_eq!(normalize("(`/x/y`) =/z"), "(`<path> =<path>");
        assert_eq!(
            normalize("t 2025-01-02 03:04:05.123+09:00 ok"),
            "t <time> ok"
        );
    }

    #[test]
    fn fingerprint_rules() {
        assert_eq!(fingerprint(0, true, "x"), None);
        assert_eq!(fingerprint(2, true, "x"), Some(sha256_hex(b"timeout")));
        assert_eq!(fingerprint(1, false, "a 1"), fingerprint(1, false, "a 22"));
        assert_ne!(
            fingerprint(1, false, "alpha"),
            fingerprint(1, false, "beta")
        );
    }

    fn e(exit: i64, fp: Option<&str>) -> Entry {
        Entry {
            name: "decide".into(),
            exit,
            fingerprint: fp.map(str::to_string),
            elapsed_ms: 1,
            timed_out: false,
            at: "x".into(),
        }
    }

    #[test]
    fn consecutive_counts_only_the_newest_fingerprint() {
        let a = e(1, Some("a"));
        let b = e(1, Some("b"));
        let ok = e(0, None);
        assert_eq!(consecutive_same_failure(&[&a, &b, &b, &b]), 3);
        assert_eq!(consecutive_same_failure(&[&b, &b, &a]), 1);
        assert_eq!(consecutive_same_failure(&[&b, &ok, &b]), 1);
        assert_eq!(consecutive_same_failure(&[&b, &ok]), 0);
    }

    #[test]
    fn history_lines_must_keep_the_contract() {
        let p = Path::new("/h");
        let fp = "a".repeat(64);
        let good = format!(
            "{{\"name\":\"decide\",\"exit\":1,\"fingerprint\":\"{fp}\",\"elapsed_ms\":1,\"timed_out\":false,\"at\":\"t\"}}"
        );
        assert!(parse_history(p, &good).is_ok());
        assert!(parse_history(p, "").is_err());
        assert!(parse_history(p, "{").is_err());
        let null_fail = "{\"name\":\"decide\",\"exit\":1,\"fingerprint\":null,\"elapsed_ms\":1,\"timed_out\":false,\"at\":\"t\"}";
        assert!(parse_history(p, null_fail).is_err());
        let unknown = good.replace("\"decide\"", "\"other\"");
        assert!(parse_history(p, &unknown).is_err());
    }
}
