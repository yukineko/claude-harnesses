//! `backlog add` writes the minted id back into the RED test its repro names
//! (backlog 3d5285d5; user ruling 2026-11-16, option 1).
//!
//! The RED test is committed BEFORE `add` mints the id (the repro must fail at
//! a clean HEAD), so without this nothing ever puts the id into the test. After
//! the row is filed, the test file the repro names has its
//! `#[ignore = "RED..."]` rewritten to `#[ignore = "RED(<id>): ..."]`.
//!
//! What this does and does not do:
//! - It edits the WORKING-TREE file only. It does not stage or commit: `add`
//!   does not own the caller's index or branch (the row itself lands through
//!   its own path), and committing from inside `add` would sweep in whatever
//!   else the caller has staged. The change is printed on stderr and shows in
//!   `git status`; the caller commits it. Until then the tree is dirty, and
//!   commands that require a clean tree (`edit --repro-test`, `recheck`) refuse.
//! - It applies only to a `cargo test` repro, and only through the file the
//!   repro names with `--test <name>` (`tests/<name>.rs` or
//!   `tests/<name>/main.rs`, git-tracked). No other file is guessed at.
//! - `pytest` / script repros have no `#[ignore]` convention: skipped, with a
//!   note on stderr (exit unaffected).
//! - Which attribute: only `#[ignore` attributes in CODE count (a mention in
//!   a comment or a string/char literal is skipped; lexical scan, see
//!   [`code_mask`]). If the repro carries libtest name filters, ONLY the
//!   `#[ignore]` of the test they select (fn name contains a filter) is a
//!   target: exactly one must be selected and it must be an id-less
//!   `"RED...`; another id, a non-RED reason, none selected or several
//!   selected is refused. Without filters, the file must hold exactly one
//!   id-less `#[ignore = "RED...`.
//! - Every other failure to write back — a `cargo test` repro with no
//!   `--test`, several `--test`s, no/several matching files, an unreadable or
//!   unwritable file, no id-less `#[ignore = "RED` attribute, a refused
//!   selection above, a `cargo test` option whose arity cannot be determined
//!   (a flag in neither the value nor the no-value table for its level, without
//!   `=`, see [`CARGO_VALUE_FLAGS`] and [`LIBTEST_VALUE_FLAGS`]; a value flag
//!   with no value word) — is NOT success: an `error:` line on stderr and exit 1.
//!   Determined forms: `--flag=value` (inline value); an optional-value flag
//!   (`--timings`, libtest `--report-time`/`--ensure-time`) bare (no value) or
//!   `=` (inline value); combined/repeated known short no-value flags (`-vv`).
//!   The row is already filed and is NOT rolled back; the message says so, so
//!   the caller does not re-add.
//! - Idempotent: when the target (the filter-selected attribute, or with no
//!   filters: no id-less RED and some attribute already `RED(<id>)`) already
//!   carries this id, the file is left unchanged.
//! - Residual: when exactly one tracked file matches `--test <name>`, `-p` is
//!   not cross-checked against its `Cargo.toml` (the repro already ran under
//!   that `-p`); a `[[test]] path = ...` target outside `tests/` is not found
//!   and is refused.

use crate::evidence;
use harness_core::boundary;
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// What the write-back did when it did not fail.
#[derive(Debug, PartialEq, Eq)]
enum Done {
    /// The attribute in this repo-relative file was rewritten.
    Written(String),
    /// The file already carried `RED(<id>)`; nothing changed.
    AlreadyTagged(String),
    /// The repro's runner has no `#[ignore]` convention.
    NotApplicable(String),
}

/// Run the write-back after `add` filed `id` with `repro` and report it. On
/// failure prints `error:` and exits 1 (the row stays filed).
pub fn after_add(repro: Option<&str>, id: &str) {
    let Some(repro) = repro else { return };
    match write_back(repro, id) {
        Ok(Done::Written(p)) => eprintln!(
            "backlog add: wrote RED({id}) into the #[ignore] of {p} (working tree only, NOT \
             committed: commit it)"
        ),
        Ok(Done::AlreadyTagged(p)) => {
            eprintln!("backlog add: {p} already carries RED({id}); left unchanged")
        }
        Ok(Done::NotApplicable(why)) => eprintln!("backlog add: id write-back skipped: {why}"),
        Err(e) => {
            eprintln!(
                "error: backlog add: {id} IS filed, but the id was NOT written back into the RED \
                 test: {e}. Do not re-add; change the test's #[ignore = \"RED...\"] to \
                 #[ignore = \"RED({id}): ...\"] by hand."
            );
            std::process::exit(1);
        }
    }
}

fn write_back(repro: &str, id: &str) -> Result<Done, String> {
    let cmd = evidence::parse_test_cmd(repro)?;
    if !matches!(cmd.runner, evidence::Runner::CargoTest) {
        return Ok(Done::NotApplicable(format!(
            "the {} repro has no #[ignore = \"RED...\"] convention",
            cmd.runner.label()
        )));
    }
    // An undetermined option is an error, never "no filter": the write-back is refused.
    let args = CargoArgs::parse(&cmd.argv)
        .map_err(|e| format!("the repro {repro:?}: {e}; no RED test was tagged"))?;
    let name = match args.tests.as_slice() {
        [one] => one.clone(),
        [] => {
            return Err(format!(
                "the repro {repro:?} names no `--test <file>`, so which file holds the RED test \
                 cannot be determined"
            ))
        }
        many => {
            return Err(format!(
                "the repro {repro:?} names several `--test` targets {many:?}; which one to tag \
                 cannot be determined"
            ))
        }
    };
    let root = evidence::repo_toplevel(&std::env::current_dir().map_err(|e| e.to_string())?)?;
    let rel = locate(&root, &name, args.package.as_deref())?;
    let path = root.join(&rel);
    let body = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    let at = match choose(&body, &args.filters, id).map_err(|e| format!("{rel}: {e}"))? {
        Pick::Insert(at) => at,
        Pick::AlreadyTagged => return Ok(Done::AlreadyTagged(rel)),
    };
    let new = insert_id(&body, at, id);
    std::fs::write(&path, new).map_err(|e| format!("cannot write {rel}: {e}"))?;
    Ok(Done::Written(rel))
}

/// The parts of a `cargo test` argv the write-back uses.
#[derive(Debug, Default, PartialEq, Eq)]
struct CargoArgs {
    tests: Vec<String>,
    package: Option<String>,
    /// Positional words (libtest name filters), before or after `--`.
    filters: Vec<String>,
}

/// `cargo test` options (before `--`) that take a separate value word, from
/// `cargo help test` (cargo 1.97.1) plus `--lockfile-path` (a real cargo CLI
/// option, hidden from that help, see the cargo CHANGELOG).
const CARGO_VALUE_FLAGS: &[&str] = &[
    "-p",
    "--package",
    "--exclude",
    "--test",
    "--bin",
    "--example",
    "--bench",
    "-F",
    "--features",
    "--target",
    "--profile",
    "--target-dir",
    "--color",
    "--message-format",
    "--manifest-path",
    "--config",
    "-C",
    "-Z",
    "-j",
    "--jobs",
    "--lockfile-path",
];

/// `cargo test` options (before `--`) whose value is optional and given only in
/// the `=` form (`--timings` / `--timings=html`). The bare form has no value.
const CARGO_OPTIONAL_VALUE_FLAGS: &[&str] = &["--timings"];

/// `cargo test` options (before `--`) that take no value.
const CARGO_NO_VALUE_FLAGS: &[&str] = &[
    "--no-run",
    "--no-fail-fast",
    "--workspace",
    "--all",
    "--lib",
    "--bins",
    "--tests",
    "--benches",
    "--examples",
    "--all-targets",
    "--doc",
    "--all-features",
    "--no-default-features",
    "-r",
    "--release",
    "-v",
    "--verbose",
    "-q",
    "--quiet",
    "--ignore-rust-version",
    "--locked",
    "--offline",
    "--frozen",
    "--keep-going",
    "--future-incompat-report",
    "-h",
    "--help",
];

/// libtest options (after `--`) that take a separate value word.
const LIBTEST_VALUE_FLAGS: &[&str] = &[
    "--test-threads",
    "--skip",
    "--color",
    "--format",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

/// libtest options (after `--`) whose value is optional and given only in the
/// `=` form (`--report-time` / `--report-time=TIMES`). The bare form has no value.
const LIBTEST_OPTIONAL_VALUE_FLAGS: &[&str] = &["--report-time", "--ensure-time"];

/// libtest options (after `--`) that take no value.
const LIBTEST_NO_VALUE_FLAGS: &[&str] = &[
    "--include-ignored",
    "--ignored",
    "--exact",
    "--exclude-should-panic",
    "--nocapture",
    "--show-output",
    "--list",
    "--force-run-in-process",
    "--shuffle",
    "--test",
    "--bench",
    "-q",
    "--quiet",
    "-h",
    "--help",
];

/// Single-letter cargo options (before `--`) that take no value; combinable and
/// repeatable (`-v`, `-vv`, `-rv`).
const CARGO_SHORT_NO_VALUE: &[char] = &['v', 'q', 'r', 'h'];

/// Single-letter libtest options (after `--`) that take no value; combinable
/// and repeatable (`-q`, `-qq`).
const LIBTEST_SHORT_NO_VALUE: &[char] = &['q', 'h'];

/// How many value words one flag word consumes, as far as the word itself
/// shows it.
#[derive(Debug, PartialEq, Eq)]
enum Arity {
    /// No value; the flag stands alone.
    NoValue,
    /// The value is the next word.
    NextWord,
    /// The value is inside this word (`--flag=value`, `-Zvalue`).
    Inline(String),
}

/// The arity of one option word, or an error when it cannot be determined.
/// A flag not in the table for its level (cargo before `--`, libtest after) is
/// undetermined: its next word might be a value or a test-name filter, and
/// guessing either way can tag the wrong test.
fn flag_arity(w: &str, libtest: bool) -> Result<(String, Arity), String> {
    let (value_flags, optional_flags, no_value_flags, short_no_value) = if libtest {
        (
            LIBTEST_VALUE_FLAGS,
            LIBTEST_OPTIONAL_VALUE_FLAGS,
            LIBTEST_NO_VALUE_FLAGS,
            LIBTEST_SHORT_NO_VALUE,
        )
    } else {
        (
            CARGO_VALUE_FLAGS,
            CARGO_OPTIONAL_VALUE_FLAGS,
            CARGO_NO_VALUE_FLAGS,
            CARGO_SHORT_NO_VALUE,
        )
    };
    let level = if libtest {
        "libtest (after `--`)"
    } else {
        "cargo test (before `--`)"
    };
    if w.starts_with("--") {
        if let Some((flag, value)) = w.split_once('=') {
            // `--flag=value` has a determined arity: the value is inline. Only a
            // flag that never takes a value is refused here.
            if no_value_flags.contains(&flag) {
                return Err(format!("`{flag}` takes no value, but `{w}` gives one"));
            }
            return Ok((flag.to_string(), Arity::Inline(value.to_string())));
        }
        if value_flags.contains(&w) {
            return Ok((w.to_string(), Arity::NextWord));
        }
        if no_value_flags.contains(&w) || optional_flags.contains(&w) {
            return Ok((w.to_string(), Arity::NoValue));
        }
        return Err(format!(
            "`{w}` is not a known {level} option, so whether its next word is a value or a test \
             filter cannot be determined"
        ));
    }
    // Short options: `-p`, `-pVALUE`, `-Z`, `-Zvalue`. `-x=` is not modelled.
    if w.contains('=') {
        return Err(format!("short option `{w}` with `=` cannot be determined"));
    }
    // A combination of known no-value short flags (`-v`, `-vv`, `-qv`) is a
    // repeat/combination of flags that never take a value.
    if w.len() > 1 && w[1..].chars().all(|c| short_no_value.contains(&c)) {
        return Ok((w.to_string(), Arity::NoValue));
    }
    // `w.get(..2)` (not `w[..2]`): a non-ASCII word must not panic on a char boundary.
    if let Some(short) = w
        .get(..2)
        .filter(|p| w.len() > 2 && value_flags.contains(p))
    {
        return Ok((short.to_string(), Arity::Inline(w[2..].to_string())));
    }
    if value_flags.contains(&w) {
        return Ok((w.to_string(), Arity::NextWord));
    }
    if no_value_flags.contains(&w) {
        return Ok((w.to_string(), Arity::NoValue));
    }
    Err(format!(
        "`{w}` is not a known {level} option, so whether its next word is a value or a test \
         filter cannot be determined"
    ))
}

impl CargoArgs {
    /// Parse the `cargo test` argv. An option whose arity cannot be determined
    /// (an unknown flag, a value flag with no value word) is an `Err`: a caller
    /// must refuse the write-back, never read it as "no filter".
    fn parse(argv: &[String]) -> Result<CargoArgs, String> {
        let mut out = CargoArgs::default();
        let mut words = argv.iter().skip(2); // `cargo test`
        let mut libtest = false;
        while let Some(w) = words.next() {
            if w == "--" {
                if libtest {
                    return Err("a second `--` in the repro's argv cannot be determined".into());
                }
                libtest = true;
                continue;
            }
            if !w.starts_with('-') {
                out.filters.push(w.clone());
                continue;
            }
            let (flag, arity) = flag_arity(w, libtest)?;
            let value = match arity {
                Arity::NoValue => continue,
                Arity::Inline(v) => v,
                Arity::NextWord => match words.next() {
                    Some(v) if v != "--" => v.clone(),
                    _ => return Err(format!("`{flag}` is given no value word")),
                },
            };
            if libtest {
                continue; // libtest values are never a target or a package
            }
            match flag.as_str() {
                "--test" => out.tests.push(value),
                "-p" | "--package" => out.package = Some(value),
                _ => {}
            }
        }
        Ok(out)
    }
}

/// The single git-tracked file `--test <name>` builds, repo-relative.
fn locate(root: &Path, name: &str, package: Option<&str>) -> Result<String, String> {
    if name.is_empty() || name.contains(['/', '\\', '*', '?', '[', ']', ':']) {
        return Err(format!("`--test {name}` is not a plain test target name"));
    }
    let specs = [
        format!(":(glob)**/tests/{name}.rs"),
        format!(":(glob)**/tests/{name}/main.rs"),
    ];
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .args(&specs)
        .stdin(Stdio::null());
    let listed = match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => out.stdout_allowing(&[0]),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    };
    let listed = match listed {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => {
            return Err(format!("cannot list the files of `--test {name}`: {why}"))
        }
    };
    let mut found: Vec<String> = listed
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if found.len() > 1 {
        if let Some(pkg) = package {
            let mut kept = Vec::new();
            for f in found {
                if let Some(d) = crate_dir(&f) {
                    // An unreadable Cargo.toml is an error, never "not this package".
                    if package_is(&root.join(d), pkg)? {
                        kept.push(f);
                    }
                }
            }
            found = kept;
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no git-tracked tests/{name}.rs or tests/{name}/main.rs{}",
            package
                .map(|p| format!(" for package {p}"))
                .unwrap_or_default()
        )),
        many => Err(format!(
            "`--test {name}` matches several files {many:?}; which one to tag cannot be determined"
        )),
    }
}

/// The crate directory of a `.../tests/...` path (the part before `tests/`).
fn crate_dir(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    p.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "tests"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Does `dir/Cargo.toml` declare `name = "<pkg>"` in its `[package]` table?
/// An unreadable manifest is an error (cannot tell), not a "no".
fn package_is(dir: &Path, pkg: &str) -> Result<bool, String> {
    let want = format!("name = \"{pkg}\"");
    let manifest = dir.join("Cargo.toml");
    let text = match std::fs::read_to_string(&manifest) {
        Ok(t) => t,
        Err(e) => {
            return Err(format!(
                "cannot read {} to tell whether it is package {pkg}: {e}",
                manifest.display()
            ))
        }
    };
    let mut in_package = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && line == want {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Which bytes of a Rust source are code, i.e. not inside a comment, a string
/// / byte-string / raw-string literal, or a char literal. Lifetimes (`'a`) are
/// code. A lexical approximation, not a parser.
fn code_mask(body: &str) -> Vec<bool> {
    let b = body.as_bytes();
    let n = b.len();
    let mut code = vec![true; n];
    let ident = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    let raw_open = |i: usize| -> Option<(usize, usize)> {
        // `r#*"` or `br#*"` not preceded by an identifier char:
        // (offset of the opening quote, number of hashes).
        if i > 0 && ident(b[i - 1]) {
            return None;
        }
        let mut j = match (b.get(i), b.get(i + 1)) {
            (Some(b'r'), _) => i + 1,
            (Some(b'b'), Some(b'r')) => i + 2,
            _ => return None,
        };
        let h = j;
        while j < n && b[j] == b'#' {
            j += 1;
        }
        (j < n && b[j] == b'"').then_some((j, j - h))
    };
    let mut i = 0;
    while i < n {
        let start = i;
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0usize;
            while i < n {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth = depth.saturating_sub(1);
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if let Some((q, hashes)) = raw_open(i) {
            i = q + 1;
            while i < n {
                if b[i] == b'"' && b[i + 1..].iter().take_while(|&&c| c == b'#').count() >= hashes {
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
        } else if b[i] == b'"' {
            i += 1;
            while i < n && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if b[i] == b'\'' {
            // `'\..'` or `'x'` is a char literal; anything else is a lifetime.
            if b.get(i + 1) == Some(&b'\\') {
                i += 3; // past the quote, the backslash and the escaped char
                while i < n && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            } else {
                let width = body[i + 1..].chars().next().map_or(0, char::len_utf8);
                if width > 0 && b.get(i + 1 + width) == Some(&b'\'') {
                    i += 2 + width;
                } else {
                    i += 1;
                    continue;
                }
            }
        } else {
            i += 1;
            continue;
        }
        for c in code.iter_mut().take(i.min(n)).skip(start) {
            *c = false;
        }
    }
    code
}

/// The reason text of one `#[ignore ...]` attribute in code.
#[derive(Debug, PartialEq, Eq)]
enum Reason {
    /// `"RED...` with no id; the offset just after `"RED`.
    Untagged(usize),
    /// `"RED(<id>)...`.
    Tagged(String),
    /// Bare `#[ignore]` or a reason that is not `RED`.
    NotRed,
}

/// One `#[ignore]` attribute (in code) and the `fn` it is attached to.
#[derive(Debug)]
struct IgnoreAttr {
    reason: Reason,
    fn_name: Option<String>,
}

/// `(start, after_kw)` of every `#` `[` `ignore` token run, allowing
/// whitespace after `#` and after `[` (`# [ignore`, `#[ ignore` are valid
/// Rust). `after_kw` is just past the `ignore` keyword.
fn ignore_attr_starts(body: &str) -> Vec<(usize, usize)> {
    let b = body.as_bytes();
    let skip_ws = |mut i: usize| {
        while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    for (h, _) in body.match_indices('#') {
        let i = skip_ws(h + 1);
        if b.get(i) != Some(&b'[') {
            continue;
        }
        let j = skip_ws(i + 1);
        if body[j..].starts_with("ignore") {
            out.push((h, j + "ignore".len()));
        }
    }
    out
}

/// Every `#[ignore` attribute that sits in code (comments and literals are
/// skipped), with its reason and the name of the next `fn` in code.
fn ignore_attrs(body: &str) -> Vec<IgnoreAttr> {
    let mask = code_mask(body);
    let mut out = Vec::new();
    for (start, after_kw) in ignore_attr_starts(body) {
        if mask.get(start) != Some(&true) {
            continue;
        }
        let rest = &body[after_kw..];
        if rest
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_alphanumeric())
        {
            continue; // `#[ignore_something]`, a different attribute
        }
        let reason = match rest.trim_start().strip_prefix('=').map(str::trim_start) {
            Some(v) => match v.strip_prefix("\"RED") {
                Some(after) => match after.chars().next() {
                    Some('(') => match after[1..].find(')') {
                        Some(e) => Reason::Tagged(after[1..1 + e].to_string()),
                        None => Reason::NotRed,
                    },
                    Some(c) if c == '_' || c.is_alphanumeric() => Reason::NotRed,
                    Some(_) => Reason::Untagged(body.len() - after.len()),
                    None => Reason::NotRed,
                },
                None => Reason::NotRed,
            },
            None => Reason::NotRed,
        };
        let fn_name = next_fn_name(body, &mask, after_kw);
        if fn_name.is_some() && attr_on_non_fn_item(body, &mask, after_kw) {
            // An attribute on a struct/mod/impl/const/... is not a test
            // ignore and must not bind to a later fn.
            continue;
        }
        out.push(IgnoreAttr { reason, fn_name });
    }
    out
}

/// True iff, between `at` and the next `fn`, code contains a `;`, `{` or `}`:
/// the attribute then belongs to an item that ended (or opened a body) before
/// that fn, i.e. a non-fn item. Attribute arguments, `pub(..)` and
/// `extern "C"` contain none of these in code (strings are masked out).
fn attr_on_non_fn_item(body: &str, mask: &[bool], at: usize) -> bool {
    let b = body.as_bytes();
    let ident = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    for (i, _) in body[at..].match_indices("fn") {
        let i = at + i;
        let before_ok = i == 0 || !ident(b[i - 1]);
        let after_ok = b.get(i + 2).is_some_and(|c| c.is_ascii_whitespace());
        if !(mask.get(i) == Some(&true) && before_ok && after_ok) {
            continue;
        }
        return (at..i).any(|j| mask.get(j) == Some(&true) && matches!(b[j], b';' | b'{' | b'}'));
    }
    false
}

/// The name of the first `fn` keyword in code after byte offset `at`.
fn next_fn_name(body: &str, mask: &[bool], at: usize) -> Option<String> {
    let b = body.as_bytes();
    let ident = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    for (i, _) in body[at..].match_indices("fn") {
        let i = at + i;
        let before_ok = i == 0 || !ident(b[i - 1]);
        let after_ok = b.get(i + 2).is_some_and(|c| c.is_ascii_whitespace());
        if !(mask.get(i) == Some(&true) && before_ok && after_ok) {
            continue;
        }
        let tail = body[i + 2..].trim_start();
        let end = tail
            .find(|c: char| !(c == '_' || c.is_alphanumeric()))
            .unwrap_or(tail.len());
        return (end > 0).then(|| tail[..end].to_string());
    }
    None
}

/// What to do with the file: rewrite at an offset, or leave it (the target
/// already carries this id). Every other answer is an error.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    Insert(usize),
    AlreadyTagged,
}

/// The one attribute to tag.
///
/// With libtest name filters, ONLY the `#[ignore]` of the test the filters
/// select (a test is selected iff its fn name contains a filter) is a target:
/// exactly one must be selected and it must be an id-less RED (one already
/// tagged with `id` is left unchanged); another id, a non-RED reason, no
/// selected `#[ignore]`, or several selected is refused. Without filters,
/// exactly one id-less RED attribute in the file is required (none, with one
/// already tagged with `id`, is left unchanged).
fn choose(body: &str, filters: &[String], id: &str) -> Result<Pick, String> {
    let attrs = ignore_attrs(body);
    if !filters.is_empty() {
        let selected: Vec<&IgnoreAttr> = attrs
            .iter()
            .filter(|a| {
                a.fn_name
                    .as_deref()
                    .is_some_and(|n| filters.iter().any(|f| n.contains(f.as_str())))
            })
            .collect();
        return match selected.as_slice() {
            [one] => match &one.reason {
                Reason::Untagged(at) => Ok(Pick::Insert(*at)),
                Reason::Tagged(t) if t == id => Ok(Pick::AlreadyTagged),
                Reason::Tagged(t) => Err(format!(
                    "the #[ignore] of the test the repro's filters {filters:?} select already \
                     carries another id RED({t})"
                )),
                Reason::NotRed => Err(format!(
                    "the #[ignore] of the test the repro's filters {filters:?} select is not a \
                     #[ignore = \"RED...\"]"
                )),
            },
            [] => Err(format!(
                "no #[ignore] test in the file is selected by the repro's filters {filters:?}"
            )),
            many => Err(format!(
                "the repro's filters {filters:?} select {} #[ignore] tests; which one to tag \
                 cannot be determined",
                many.len()
            )),
        };
    }
    let untagged: Vec<usize> = attrs
        .iter()
        .filter_map(|a| match a.reason {
            Reason::Untagged(at) => Some(at),
            _ => None,
        })
        .collect();
    let already = attrs
        .iter()
        .any(|a| matches!(&a.reason, Reason::Tagged(t) if t == id));
    match untagged.as_slice() {
        [one] => Ok(Pick::Insert(*one)),
        [] if already => Ok(Pick::AlreadyTagged),
        [] => Err("no id-less #[ignore = \"RED...\"] attribute in the file".to_string()),
        many => Err(format!(
            "{} id-less #[ignore = \"RED...\"] attributes and the repro names no test filter to \
             select one",
            many.len()
        )),
    }
}

/// Insert `(<id>)` after `"RED` at `at`, normalising to `RED(<id>): ...`.
fn insert_id(body: &str, at: usize, id: &str) -> String {
    let after = &body[at..];
    let tag = match after.chars().next() {
        Some(':') | Some('"') => format!("({id})"),
        Some(c) if c.is_whitespace() => format!("({id}):"),
        _ => format!("({id}): "),
    };
    format!("{}{tag}{after}", &body[..at])
}
