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
//! - Every other failure to write back — a `cargo test` repro with no
//!   `--test`, several `--test`s, no/several matching files, an unreadable or
//!   unwritable file, no `#[ignore = "RED` attribute, several candidates that
//!   the repro's test-name filters do not narrow to one — is NOT success: an
//!   `error:` line on stderr and exit 1. The row is already filed and is NOT
//!   rolled back; the message says so, so the caller does not re-add.
//! - Idempotent: a file already containing `RED(<id>)` is left unchanged.

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
    let args = CargoArgs::parse(&cmd.argv);
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
    if body.contains(&format!("RED({id})")) {
        return Ok(Done::AlreadyTagged(rel));
    }
    let at = choose(&body, &args.filters).map_err(|e| format!("{rel}: {e}"))?;
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

/// Options of `cargo test` / libtest that take a separate value word.
const VALUE_FLAGS: &[&str] = &[
    "-p",
    "--package",
    "--test",
    "--bin",
    "--example",
    "--bench",
    "--features",
    "-F",
    "--target",
    "--target-dir",
    "--manifest-path",
    "-j",
    "--jobs",
    "--profile",
    "--color",
    "--message-format",
    "--exclude",
    "-Z",
    "--config",
    "--skip",
    "--test-threads",
    "--format",
    "--logfile",
    "--report-time",
    "--shuffle-seed",
];

impl CargoArgs {
    fn parse(argv: &[String]) -> CargoArgs {
        let mut out = CargoArgs::default();
        let mut words = argv.iter().skip(2); // `cargo test`
        while let Some(w) = words.next() {
            let (flag, inline) = match w.split_once('=') {
                Some((f, v)) if f.starts_with('-') => (f, Some(v.to_string())),
                _ => (w.as_str(), None),
            };
            if VALUE_FLAGS.contains(&flag) {
                let value = inline.or_else(|| words.next().cloned());
                match (flag, value) {
                    ("--test", Some(v)) => out.tests.push(v),
                    ("-p" | "--package", Some(v)) => out.package = Some(v),
                    _ => {}
                }
            } else if !w.starts_with('-') {
                out.filters.push(w.clone());
            }
        }
        out
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

/// Does `dir/Cargo.toml` declare `name = "<pkg>"`? An unreadable manifest
/// is an error (cannot tell), not a "no".
fn package_is(dir: &Path, pkg: &str) -> Result<bool, String> {
    let want = format!("name = \"{pkg}\"");
    let manifest = dir.join("Cargo.toml");
    match std::fs::read_to_string(&manifest) {
        Ok(t) => Ok(t.lines().any(|l| l.trim() == want)),
        Err(e) => Err(format!(
            "cannot read {} to tell whether it is package {pkg}: {e}",
            manifest.display()
        )),
    }
}

/// Byte offsets just after `"RED` of every `#[ignore = "RED...` attribute that
/// carries no id yet (the next char is not `(`, nor part of a longer word).
fn untagged(body: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (start, _) in body.match_indices("#[ignore") {
        let rest = &body[start + "#[ignore".len()..];
        let r = rest.trim_start();
        let Some(r) = r.strip_prefix('=') else {
            continue;
        };
        let r = r.trim_start();
        let Some(after) = r.strip_prefix("\"RED") else {
            continue;
        };
        match after.chars().next() {
            Some(c) if c == '(' || c == '_' || c.is_alphanumeric() => continue,
            None => continue,
            _ => {}
        }
        out.push(body.len() - after.len());
    }
    out
}

/// The name of the first `fn` after byte offset `at`.
fn next_fn_name(body: &str, at: usize) -> Option<&str> {
    let rest = &body[at..];
    let i = rest.find("fn ")?;
    let tail = rest[i + 3..].trim_start();
    let end = tail
        .find(|c: char| !(c == '_' || c.is_alphanumeric()))
        .unwrap_or(tail.len());
    Some(&tail[..end])
}

/// The one attribute to tag. Several candidates are narrowed by the repro's
/// libtest filters (a test runs iff its name contains a filter); anything but
/// exactly one is refused.
fn choose(body: &str, filters: &[String]) -> Result<usize, String> {
    let all = untagged(body);
    if all.is_empty() {
        return Err("no untagged #[ignore = \"RED...\"] attribute in the file".to_string());
    }
    if all.len() == 1 {
        return Ok(all[0]);
    }
    let narrowed: Vec<usize> = all
        .iter()
        .copied()
        .filter(|&at| {
            next_fn_name(body, at).is_some_and(|n| filters.iter().any(|f| n.contains(f.as_str())))
        })
        .collect();
    match narrowed.as_slice() {
        [one] => Ok(*one),
        _ => Err(format!(
            "{} untagged #[ignore = \"RED...\"] attributes and the repro's test-name filters \
             {filters:?} do not select exactly one",
            all.len()
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
