//! PreToolUse gate: deny edits to registered targets that are not ratified,
//! edits at the former path of a registered target that is gone, and the
//! human-only `fde forget`.
//!
//! # Decision
//!
//! [`evaluate`] judges one PreToolUse payload and answers a [`Verdict`]; the
//! CLI (`run_gate` in `main.rs`) prints it as
//! `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":
//! "allow"|"deny","permissionDecisionReason":...}}` via [`decision_json`]:
//! `Clean` is `allow` (exit 0); `Violation` and `Undetermined` are both `deny`
//! (exit 2). There is no path from `Undetermined` to `allow`.
//!
//! # Order
//!
//! 1. The payload is parsed first, so a malformed payload is denied even when
//!    nothing is registered. It must be a JSON object with a non-empty string
//!    `tool_name`. For `Edit` / `Write` / `MultiEdit` the path is
//!    `tool_input.file_path`, for `NotebookEdit` `tool_input.notebook_path`
//!    (a non-empty string; a relative one is joined onto the payload `cwd`,
//!    which must then be absolute). For `Bash`, `tool_input.command` must be a
//!    string and the payload `cwd` an absolute path. Anything missing or
//!    mistyped is `Undetermined`.
//! 2. The state directory ([`crate::target::state_dir`]) and the registered
//!    targets ([`crate::ratify::registered_targets`]) are read; an unresolved
//!    state dir, an unreadable listing or any corrupt registration is
//!    `Undetermined`. No registered target at all is `Clean` for every tool.
//!    A corrupt registration therefore denies everything until the human
//!    removes it with `fde forget`.
//! 3. `fde forget` is human-only (like `fde ratify`): while any target is
//!    registered (gone ones included), a Bash call whose command invokes it
//!    ([`invokes_fde_forget`]: among the [`lenient_words`] — split on
//!    whitespace and `;&|()<>`, quotes and backslashes deleted — a word `fde`
//!    or ending in `/fde` followed later by the word `forget`) is a
//!    `Violation`, wherever it runs and whatever it names. Textual and
//!    conservative: a quoted string that merely contains those words is
//!    denied too.
//! 4. Any other `tool_name` (outside the hook matcher
//!    `Bash|Edit|Write|MultiEdit|NotebookEdit`) is `Undetermined` while a
//!    target is registered: the gate does not know whether it edits files.
//! 5. Each registered target's root is stat'ed ([`crate::target::presence`],
//!    `symlink_metadata`, before its ratification is looked at):
//!    * `NotFound` — the target is *gone*, a determined fact. It is not
//!      guarded and no git probe is made for it. Its own former path stays
//!      denied, as a `Violation` whose reason
//!      says it is gone and names `fde forget <root>`: an Edit-tool path
//!      lying (as given, or canonicalized) under the recorded root, or a Bash
//!      call whose cwd (as given, or canonicalized) lies under it, whose
//!      command text contains it, or one of whose resolvable words (step 6,
//!      Bash) lies under it — joined onto the cwd lexically or through
//!      [`canonical_lenient`]. Otherwise, a word whose canonicalization fails
//!      with an error other than `NotFound` (e.g. `notes.txt/x`, `notes.txt` a
//!      file), or an unresolvable word (step 6: `$`, backtick, unquoted glob or
//!      brace, `~user`, `~` with `HOME` unset, a dangling backslash, an
//!      unterminated quote; user ruling 2026-11-04), makes the call
//!      `Undetermined` (deny) for the gone target, with a reason naming the
//!      word and asking the human to run `! fde forget <root>`: the gate cannot
//!      show the call avoids the former path. The step-6
//!      `fde status|report|begin` exception applies to the `Violation` only.
//!    * any other stat error — `Undetermined` (deny), whatever the call
//!      touches: the root may still be there.
//! 6. Of the present targets, every one whose [`crate::ratify::status`] is
//!    `Ratified` is out of the question. Every other one (`Unratified`, or an
//!    `Undetermined` status) is *guarded*. A guarded target's area is its
//!    registered canonical root ALONE. The target repository's other git
//!    work trees are not enumerated (no `git worktree list`; user ruling
//!    2026-11-08: a gate must not look at any other worktree — "残しているのは
//!    worktreeの最適化だけとした"), so a linked worktree created outside the
//!    target dir is NOT inside, and nothing else checks it.
//!    * Edit tools: the path is canonicalized ([`canonical_lenient`]: the
//!      nearest existing ancestor is canonicalized — symlinks resolved — and
//!      the not-yet-existing rest is appended, with `.` / `..` applied
//!      lexically). It is inside a target when it lies under the root.
//!    * Bash: inside when the canonicalized payload `cwd` lies in the area,
//!      when the command string contains the text of the root, or when one of the
//!      command's words resolves into the area. Words come from a small
//!      conservative lexer ([`crate::bashwords`]): split on unquoted
//!      whitespace (newline included) and `;&|()<>` (so `>file` / `2>file`
//!      yield `file`), single quotes literal, double quotes keep their content,
//!      backslash escapes; a leading `~` / `~/` is `$HOME`; for a word with an
//!      `=` (`--opt=value`, `NAME=value`) the value is a further word. Each
//!      word is joined onto the canonical cwd (unless absolute) and passed
//!      through [`canonical_lenient`], so `../proj/x`, `proj`, `link/x`
//!      (symlink into the target) are all inside. A word is *unresolvable* when it holds an unquoted or
//!      double-quoted `$` or backtick, an unquoted `*` `?` `[` `{`, a dangling
//!      backslash, a `~user` prefix, or `~` with `HOME` unset / empty; an
//!      unterminated quote makes the whole command unresolvable.
//!    * Decision: inside an `Unratified` target is a `Violation`; inside a
//!      target whose status is `Undetermined` is `Undetermined`. Failing to
//!      canonicalize the path is `Undetermined` — even for a path that might
//!      lie elsewhere.
//!      For Bash, when nothing touches the area but a word is unresolvable or
//!      its path cannot be canonicalized (an error other than `NotFound`, e.g.
//!      `notes.txt/x` with `notes.txt` a file), the call is `Undetermined` for
//!      every guarded target, with a reason naming the word. With no guarded
//!      and no gone target (none registered, or all present and ratified)
//!      words are never judged.
//!    * Not seen (limits, not fixed): aliases, shell functions and `PATH`
//!      resolution of a different binary; directory changes inside the
//!      command (`cd x && ...` — every relative word resolves against the
//!      payload cwd); paths a program constructs at run time; here-doc bodies
//!      are lexed as words like the rest of the command.
//!    * Exception (user ruling 2026-11-03), Bash only: a Bash call that
//!      touches a target whose status is `Unratified` is not a violation when
//!      its command is an exact single `fde status|report|begin` invocation
//!      ([`is_allowed_fde_invocation`]). `fde ratify` stays human-only and
//!      denied. The exception does not apply to a target whose status is
//!      `Undetermined` (still `Undetermined`, i.e. deny — the gate does not
//!      know the target is merely unratified), nor to an area that cannot be
//!      determined, nor to Edit / Write / MultiEdit / NotebookEdit.
//! 7. Nothing guarded or gone touched: `Clean`.

use std::path::{Component, Path, PathBuf};

use harness_core::verdict::{Determination, Verdict};
use serde_json::Value;

use crate::ratify;
use crate::target::{self, ResolvedTarget};

/// Tools whose path is in `tool_input.file_path`.
const FILE_PATH_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit"];

/// One parsed PreToolUse request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    /// A file-editing tool acting on `path` (absolute, not yet canonicalized).
    Edit { tool: String, path: PathBuf },
    /// A Bash command run in `cwd` (absolute, not yet canonicalized).
    Bash { cwd: PathBuf, command: String },
    /// A tool this gate does not judge.
    Other { tool: String },
}

/// Parse a PreToolUse payload (see the module doc, step 1).
#[track_caller]
fn parse(payload: &str) -> Determination<Request> {
    if payload.trim().is_empty() {
        return Determination::undetermined("the PreToolUse payload on stdin is empty");
    }
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            return Determination::undetermined(format!(
                "the PreToolUse payload is not valid JSON: {e}"
            ))
        }
    };
    if !v.is_object() {
        return Determination::undetermined("the PreToolUse payload is not a JSON object");
    }
    let tool = match v.get("tool_name").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return Determination::undetermined(
                "the PreToolUse payload has no non-empty string `tool_name`",
            )
        }
    };
    let cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    let input = v.get("tool_input");

    let field = if FILE_PATH_TOOLS.contains(&tool.as_str()) {
        Some("file_path")
    } else if tool == "NotebookEdit" {
        Some("notebook_path")
    } else {
        None
    };
    if let Some(field) = field {
        let raw = match input.and_then(|i| i.get(field)).and_then(Value::as_str) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => {
                return Determination::undetermined(format!(
                    "the {tool} payload has no non-empty string `tool_input.{field}`"
                ))
            }
        };
        let path = if raw.is_absolute() {
            raw
        } else {
            match &cwd {
                Some(c) if c.is_absolute() => c.join(raw),
                _ => {
                    return Determination::undetermined(format!(
                        "the {tool} path {} is relative and the payload has no absolute `cwd`",
                        raw.display()
                    ))
                }
            }
        };
        return Determination::known(Request::Edit { tool, path });
    }
    if tool == "Bash" {
        let command = match input.and_then(|i| i.get("command")).and_then(Value::as_str) {
            Some(c) => c.to_string(),
            None => {
                return Determination::undetermined(
                    "the Bash payload has no string `tool_input.command`",
                )
            }
        };
        let cwd = match cwd {
            Some(c) if c.is_absolute() => c,
            _ => {
                return Determination::undetermined("the Bash payload has no absolute `cwd`");
            }
        };
        return Determination::known(Request::Bash { cwd, command });
    }
    Determination::known(Request::Other { tool })
}

/// Canonicalize the absolute path `p`, which need not exist: the longest
/// existing prefix is canonicalized (symlinks resolved) and the remaining,
/// not-yet-existing components are appended, `.` skipped and `..` popping one
/// component. Lexical handling of the rest is sound because none of it exists,
/// so none of it can be a symlink.
///
/// `Undetermined` when `p` is not absolute or any canonicalize error other
/// than `NotFound` occurs (permission denied, a file used as a directory, …).
#[track_caller]
pub fn canonical_lenient(p: &Path) -> Determination<PathBuf> {
    if !p.is_absolute() {
        return Determination::undetermined(format!("path {} is not absolute", p.display()));
    }
    let comps: Vec<Component<'_>> = p.components().collect();
    for n in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..n].iter().collect();
        match std::fs::canonicalize(&prefix) {
            Ok(mut base) => {
                for c in &comps[n..] {
                    match c {
                        Component::Normal(name) => base.push(name),
                        Component::CurDir => {}
                        Component::ParentDir => {
                            base.pop();
                        }
                        Component::RootDir | Component::Prefix(_) => {
                            return Determination::undetermined(format!(
                                "path {} has a root component after its start",
                                p.display()
                            ))
                        }
                    }
                }
                return Determination::known(base);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "path {} cannot be canonicalized: {e}",
                    prefix.display()
                ))
            }
        }
    }
    Determination::undetermined(format!(
        "no prefix of path {} could be canonicalized",
        p.display()
    ))
}

/// The area of a guarded target: its canonical root (for the path checks) and
/// the root as spelled (for the Bash text match). Only the registered dir:
/// the target's other git work trees are not enumerated (user ruling
/// 2026-11-08: a gate must not look at any other worktree — "残しているのは
/// worktreeの最適化だけとした").
struct Area {
    canonical: Vec<PathBuf>,
    spelled: Vec<PathBuf>,
}

/// The registered root of `t`; see the module doc, step 6. No git probe.
fn area(t: &ResolvedTarget) -> Area {
    Area {
        canonical: vec![t.root.clone()],
        spelled: vec![t.root.clone()],
    }
}

/// Whether a request touches an area.
enum Touch {
    /// It does (description).
    Hit(String),
    /// It cannot be told (reason): a Bash word is unresolvable or its path
    /// cannot be canonicalized.
    Unknown(String),
    /// It does not.
    Miss,
}

/// Whether the request touches `area` (step 6).
fn touches(req: &Checked, area: &Area) -> Touch {
    match req {
        Checked::Path { tool, path, .. } => area
            .canonical
            .iter()
            .find(|a| path.starts_with(a))
            .map_or(Touch::Miss, |a| {
                Touch::Hit(format!(
                    "{tool} of {} lies in {}",
                    path.display(),
                    a.display()
                ))
            }),
        Checked::Bash {
            cwd,
            command,
            words,
            ..
        } => {
            if let Some(a) = area.canonical.iter().find(|a| cwd.starts_with(a)) {
                return Touch::Hit(format!(
                    "Bash runs in {} inside {}",
                    cwd.display(),
                    a.display()
                ));
            }
            if let Some(a) = area
                .canonical
                .iter()
                .chain(area.spelled.iter())
                .filter_map(|a| a.to_str())
                .find(|a| command.contains(a))
            {
                return Touch::Hit(format!("the Bash command names {a}"));
            }
            let mut unknown = None;
            for w in words {
                match w {
                    BashWord::Path {
                        spelled,
                        canonical: Determination::Known(p),
                        ..
                    } => {
                        if let Some(a) = area.canonical.iter().find(|a| p.starts_with(a)) {
                            return Touch::Hit(format!(
                                "the Bash word `{spelled}` resolves to {} inside {}",
                                p.display(),
                                a.display()
                            ));
                        }
                    }
                    BashWord::Path {
                        spelled,
                        canonical: Determination::Undetermined(why),
                        ..
                    } => {
                        unknown.get_or_insert_with(|| {
                            format!("the Bash word `{spelled}` cannot be resolved: {why}")
                        });
                    }
                    BashWord::Unresolvable(why) => {
                        unknown.get_or_insert_with(|| why.clone());
                    }
                }
            }
            unknown.map_or(Touch::Miss, Touch::Unknown)
        }
    }
}

/// `fde` subcommands the model may run via Bash while a target is unratified.
const ALLOWED_FDE_SUBCOMMANDS: &[&str] = &["status", "report", "begin"];

/// Characters a word of an allowed invocation may contain: ASCII
/// alphanumerics and `-_./=:,@+%`. None of them is a shell metacharacter,
/// quote, expansion, glob or redirect character, so a command made of such
/// words separated by spaces / tabs is a single simple command whose words
/// reach the program verbatim.
fn is_safe_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-_./=:,@+%".contains(c)
}

/// Whether `command` is exactly one `fde status|report|begin` invocation
/// (user ruling 2026-11-03). A conservative allowlist, not a shell parser:
///
/// * Leading / trailing spaces and tabs are ignored; any other character
///   (newline included) is judged below.
/// * Every remaining character is a space, a tab or [`is_safe_word_char`]:
///   no quotes, `$`, backtick, `;&|<>()`, newline, backslash, glob, brace,
///   `~`, `!`, `#` — so no chaining, redirect, substitution, expansion or
///   comment can occur.
/// * The words are the runs between spaces / tabs. The first is exactly
///   `fde` (not a path, not an env assignment, not a wrapper such as
///   `command` / `exec` / `env` / `sh -c`); the second is exactly one of
///   `status`, `report`, `begin`.
/// * No later word contains `ratify` anywhere (so `fde status ratify` and a
///   path spelled with `ratify` in it are both denied).
///
/// Anything else answers `false` (stay denied). Not covered: the gate cannot
/// see which binary the shell resolves `fde` to through `PATH`, aliases or
/// functions; that resolution is outside the command text.
pub fn is_allowed_fde_invocation(command: &str) -> bool {
    let trimmed = command.trim_matches(|c| c == ' ' || c == '\t');
    if !trimmed
        .chars()
        .all(|c| c == ' ' || c == '\t' || is_safe_word_char(c))
    {
        return false;
    }
    let mut words = trimmed.split([' ', '\t']).filter(|w| !w.is_empty());
    if words.next() != Some("fde") {
        return false;
    }
    match words.next() {
        Some(sub) if ALLOWED_FDE_SUBCOMMANDS.contains(&sub) => {}
        _ => return false,
    }
    words.all(|w| !w.contains("ratify"))
}

/// Split `command` into words for conservative textual checks — not a shell
/// parser. Separators are whitespace (newline and tab included) and the shell
/// operator characters `;&|()<>`; quote characters (`'`, `"`) and backslashes
/// are deleted from each word (so `'fde'`, `"forget"`, `f\orget` read as the
/// bare words), and empty words are dropped. Over-matching (a quoted string
/// that merely contains the words) is accepted: callers use it to deny.
pub(crate) fn lenient_words(command: &str) -> Vec<String> {
    command
        .split(|c: char| c.is_whitespace() || ";&|()<>".contains(c))
        .map(|w| w.replace(['\'', '"', '\\'], ""))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `command` invokes `fde forget` (step 3, human-only): among its
/// [`lenient_words`], a word that is `fde` or ends with `/fde` is followed,
/// anywhere later, by the word `forget`.
pub fn invokes_fde_forget(command: &str) -> bool {
    let words = lenient_words(command);
    words
        .iter()
        .position(|w| w == "fde" || w.ends_with("/fde"))
        .is_some_and(|i| words[i + 1..].iter().any(|w| w == "forget"))
}

/// A request with its path / cwd canonicalized (`path` / `cwd`) and as given
/// (`raw_*`, absolute; used for the lexical match against a gone root).
enum Checked {
    Path {
        tool: String,
        path: PathBuf,
        raw_path: PathBuf,
    },
    Bash {
        cwd: PathBuf,
        raw_cwd: PathBuf,
        command: String,
        words: Vec<BashWord>,
    },
}

/// One path candidate of a Bash command ([`crate::bashwords::word_paths`]).
enum BashWord {
    /// A resolvable word: `canonical` is `cwd.join(word)` (cwd canonical)
    /// through [`canonical_lenient`]; `raw` is `raw_cwd.join(word)` with `.` /
    /// `..` applied lexically (for the gone-root match).
    Path {
        spelled: String,
        canonical: Determination<PathBuf>,
        raw: PathBuf,
    },
    /// A word the gate cannot resolve (reason names the word).
    Unresolvable(String),
}

/// `base.join(rel)` with `.` skipped and `..` popping, purely lexically.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolve the path words of `command` against `cwd` (canonical) and
/// `raw_cwd` (as given); `HOME` from the environment expands `~`.
fn bash_words(command: &str, cwd: &Path, raw_cwd: &Path) -> Vec<BashWord> {
    let home = std::env::var("HOME").ok();
    crate::bashwords::word_paths(command, home.as_deref())
        .into_iter()
        .map(|w| match w {
            crate::bashwords::WordPath::Literal { spelled, path } => {
                let rel = Path::new(&path);
                BashWord::Path {
                    spelled,
                    canonical: canonical_lenient(&cwd.join(rel)),
                    raw: lexical_join(raw_cwd, rel),
                }
            }
            crate::bashwords::WordPath::Unresolvable { why } => BashWord::Unresolvable(why),
        })
        .collect()
}

/// Whether the request touches the gone root `root`: `Hit` when the raw or
/// canonicalized path / cwd lies under it, the Bash command text contains
/// it, or a resolvable Bash word lies under it; otherwise `Unknown` when a
/// Bash word is unresolvable or cannot be canonicalized (fail-closed, user
/// ruling 2026-11-04); otherwise `Miss`. No git probe: the repository is
/// gone with it.
fn touches_gone(req: &Checked, root: &Path) -> Touch {
    match req {
        Checked::Path {
            tool,
            path,
            raw_path,
        } => [raw_path, path]
            .into_iter()
            .find(|p| p.starts_with(root))
            .map_or(Touch::Miss, |p| {
                Touch::Hit(format!(
                    "{tool} of {} lies in {}",
                    p.display(),
                    root.display()
                ))
            }),
        Checked::Bash {
            cwd,
            raw_cwd,
            command,
            words,
        } => {
            if let Some(c) = [raw_cwd, cwd].into_iter().find(|c| c.starts_with(root)) {
                return Touch::Hit(format!(
                    "Bash runs in {} inside {}",
                    c.display(),
                    root.display()
                ));
            }
            if let Some(r) = root.to_str().filter(|r| command.contains(r)) {
                return Touch::Hit(format!("the Bash command names {r}"));
            }
            let mut unknown = None;
            for w in words {
                let (spelled, canonical, raw) = match w {
                    BashWord::Path {
                        spelled,
                        canonical,
                        raw,
                    } => (spelled, canonical, raw),
                    // Fail-closed (user ruling 2026-11-04): the gate cannot
                    // show an unresolvable word avoids the gone root.
                    BashWord::Unresolvable(why) => {
                        unknown.get_or_insert_with(|| why.clone());
                        continue;
                    }
                };
                if raw.starts_with(root) {
                    return Touch::Hit(format!(
                        "the Bash word `{spelled}` resolves to {} inside {}",
                        raw.display(),
                        root.display()
                    ));
                }
                match canonical {
                    Determination::Known(p) if p.starts_with(root) => {
                        return Touch::Hit(format!(
                            "the Bash word `{spelled}` resolves to {} inside {}",
                            p.display(),
                            root.display()
                        ));
                    }
                    Determination::Known(_) => {}
                    // Fail-closed: the gate cannot show this word avoids the
                    // gone root.
                    Determination::Undetermined(why) => {
                        unknown.get_or_insert_with(|| {
                            format!("the Bash word `{spelled}` cannot be resolved: {why}")
                        });
                    }
                }
            }
            unknown.map_or(Touch::Miss, Touch::Unknown)
        }
    }
}

/// Judge one PreToolUse `payload` (the raw stdin JSON) against the targets
/// registered in the state directory ([`crate::target::state_dir`]).
///
/// * `Clean` — allow (exit 0).
/// * `Violation` — deny (exit 2): a Bash `fde forget` while a target is
///   registered (step 3); or the call touches an unratified registered
///   target, or the former path of a gone one (except an exact single Bash
///   `fde status|report|begin`; see the module doc, steps 5 and 6).
/// * `Undetermined` — deny (exit 2): see the module doc for every case.
#[track_caller]
pub fn evaluate(payload: &str) -> Verdict {
    let req = match parse(payload) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    let state = match target::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    evaluate_with(req, &state)
}

fn evaluate_with(req: Request, state: &Path) -> Verdict {
    let targets = match ratify::registered_targets(state) {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return why.into_verdict(),
    };
    if targets.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    if let Request::Bash { command, .. } = &req {
        if invokes_fde_forget(command) {
            return Verdict::violation(
                "the Bash command invokes `fde forget`, which is human-only (like \
                 `fde ratify`): it drops a target's registration, ratification and \
                 evidence. Ask the human to run `! fde forget <TARGET>` in this session \
                 or `fde forget <TARGET>` in their own terminal",
            );
        }
    }
    let checked = match req {
        Request::Other { tool } => {
            return Verdict::undetermined(format!(
                "tool {tool} is not one the fde gate judges, and targets are registered; \
                 it cannot tell whether {tool} edits one"
            ))
        }
        Request::Edit { tool, path } => match canonical_lenient(&path) {
            Determination::Known(p) => Checked::Path {
                tool,
                path: p,
                raw_path: path,
            },
            Determination::Undetermined(why) => return why.into_verdict(),
        },
        Request::Bash { cwd, command } => match canonical_lenient(&cwd) {
            Determination::Known(c) => Checked::Bash {
                words: bash_words(&command, &c, &cwd),
                cwd: c,
                raw_cwd: cwd,
                command,
            },
            Determination::Undetermined(why) => return why.into_verdict(),
        },
    };

    // Step 6 exception: only a Bash command, only an exact single
    // `fde status|report|begin`, only against an `Unratified` status or a
    // gone root (step 5).
    let fde_allowance = match &checked {
        Checked::Bash { command, .. } => is_allowed_fde_invocation(command),
        Checked::Path { .. } => false,
    };

    let mut verdicts = Vec::new();
    for t in &targets {
        // Step 5: a gone root is a determined fact; only its former path is
        // still denied. A root that cannot be stat'ed is undetermined.
        match target::presence(&t.root) {
            Determination::Known(target::Presence::Present) => {}
            Determination::Known(target::Presence::Gone) => {
                match touches_gone(&checked, &t.root) {
                    Touch::Hit(hit) => {
                        if !fde_allowance {
                            verdicts.push(Verdict::violation(format!(
                                "{hit}: registered target {root} is gone (its directory no \
                                 longer exists) but its fde state remains; nothing may be \
                                 created there until the human runs `fde forget {root}` \
                                 (human-only: ask them to run `! fde forget {root}`)",
                                root = t.root.display()
                            )));
                        }
                    }
                    Touch::Unknown(why) => verdicts.push(Verdict::undetermined(format!(
                        "{why}; registered target {root} is gone (its directory no longer \
                         exists) and the gate cannot tell whether the call reaches its former \
                         path; until its state is dropped (human-only), ask the human to run \
                         `! fde forget {root}`",
                        root = t.root.display()
                    ))),
                    Touch::Miss => {}
                }
                continue;
            }
            Determination::Undetermined(why) => {
                verdicts.push(why.into_verdict());
                continue;
            }
        }
        let unratified_reason = match ratify::status(t, state) {
            Determination::Known(ratify::Status::Ratified { .. }) => continue,
            Determination::Known(ratify::Status::Unratified { reason }) => Ok(reason),
            Determination::Undetermined(why) => Err(why),
        };
        let hit = match touches(&checked, &area(t)) {
            Touch::Hit(h) => h,
            Touch::Miss => continue,
            Touch::Unknown(why) => {
                verdicts.push(Verdict::undetermined(format!(
                    "{why}; registered target {} is guarded (not ratified, or its \
                     ratification cannot be determined), so the gate cannot tell whether the \
                     call touches it",
                    t.root.display()
                )));
                continue;
            }
        };
        if fde_allowance && unratified_reason.is_ok() {
            continue;
        }
        verdicts.push(match unratified_reason {
            Ok(reason) => Verdict::violation(format!(
                "{hit}: registered target {} is not ratified ({reason}); \
                 its spec must be ratified first — `fde ratify` is human-only: \
                 ask the human to approve and run it themselves",
                t.root.display()
            )),
            Err(why) => Verdict::undetermined(format!(
                "{hit}: the ratification of registered target {} cannot be determined: {why}",
                t.root.display()
            )),
        });
    }
    if verdicts.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    Verdict::worst_of(verdicts)
}

/// The PreToolUse decision JSON for `verdict`: `allow` for `Clean`, `deny`
/// for `Violation` and `Undetermined`.
pub fn decision_json(verdict: &Verdict) -> String {
    let (decision, reason) = match verdict {
        Verdict::Clean(_) => (
            "allow",
            "fde: the call touches no unratified registered target".to_string(),
        ),
        Verdict::Violation(why) => ("deny", format!("fde: denied: {why}")),
        Verdict::Undetermined(why) => ("deny", format!("fde: undetermined, so denied: {why}")),
    };
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}
