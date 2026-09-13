//! Pins the two-sided run-namespacing contract of `condukt shadow-run`.
//!
//! `shadow-run exec` cuts a worktree and `shadow-run finish` discards it —
//! including a force-delete of the branch. Namespacing is therefore a
//! **two-sided** property: if `exec` starts writing `condukt/<run>/<branch>`
//! while `finish` keeps force-deleting the caller's raw `--branch`, the
//! real branch is stranded on disk and the mirror gap has merely MOVED.
//!
//! These tests express the desired end state:
//!
//! 1. `namespaced_round_trip_leaves_no_branch_behind` — both sides agree, so a
//!    create/finish round trip under a run namespace leaves the repo with the
//!    branch list it started with. Asserted against real `git branch` output,
//!    never against a return value.
//! 2. `finish_with_mismatched_run_namespace_fails_observably` — a `finish`
//!    whose namespace disagrees with the one `exec` used must NOT report
//!    success. A silent no-op there is the exact defect.
//! 3. `legacy_unnamespaced_shadow_run_keeps_identical_path_and_ref` —
//!    ANTI-VACUITY CONTROL. The un-namespaced path must keep producing
//!    byte-identical paths and refs. Without this, tests 1 and 2 say nothing
//!    about what the namespacing change broke.
//! 4. `no_unnamespaced_worktree_create_in_production_code` — a source-level
//!    scan proving the un-namespaced constructor has no production callers
//!    left. `#[cfg(test)]` blocks and comments are masked out, so neither
//!    satisfies nor trips the scan.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

fn condukt() -> Command {
    Command::new(BIN)
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run")
}

/// A one-commit repo plus a worktree base that is a SIBLING of it under the
/// same process-private TempDir — outside the repo (`worktree::create`'s
/// anti-nesting guard) and not shared with concurrently-running tests.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    worktree_base: PathBuf,
    flag_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        let worktree_base = tmp.path().join("wt-base");
        let home = tmp.path().join("home");
        let flag_dir = tmp.path().join("flag");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&flag_dir).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        let f = Fixture {
            _tmp: tmp,
            home,
            repo,
            worktree_base,
            flag_dir,
        };
        let enable = f
            .cmd(&["shadow-run", "enable"])
            .output()
            .expect("spawn condukt shadow-run enable");
        assert!(
            enable.status.success(),
            "shadow-run enable failed: {}",
            String::from_utf8_lossy(&enable.stderr)
        );
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = condukt();
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_SHADOW_RUN_DIR", &self.flag_dir)
            .env("CONDUKT_WORKTREE_BASE", &self.worktree_base);
        c
    }

    /// Every local branch, one per line, sorted — read from git itself.
    fn branches(&self) -> Vec<String> {
        let out = git(&self.repo, &["branch", "--format=%(refname:short)"]);
        assert!(
            out.status.success(),
            "git branch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        v.sort();
        v
    }
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A clap arg-parse rejection is NOT evidence of the behaviour under test —
/// it is evidence the flag does not exist. Any assertion of the form "this
/// invocation must fail" has to exclude it, or the test passes vacuously.
fn assert_not_an_arg_parse_error(out: &std::process::Output, what: &str) {
    let combined = format!("{}{}", stdout_of(out), stderr_of(out));
    for needle in [
        "unexpected argument",
        "unrecognized subcommand",
        "invalid subcommand",
        "USAGE:",
        "Usage:",
    ] {
        assert!(
            !combined.contains(needle),
            "{what}: the command failed at ARGUMENT PARSING ({needle:?}), \
             which proves nothing about the behaviour under test:\n{combined}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// 1. Both-sides round trip.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn namespaced_round_trip_leaves_no_branch_behind() {
    let f = Fixture::new();
    let before = f.branches();
    assert_eq!(before, vec!["main".to_string()]);

    let exec = f
        .cmd(&[
            "shadow-run",
            "exec",
            "--run",
            "run-A",
            "--topic",
            "t1-shadow",
            "--branch",
            "shadow/t1-opus",
            "--model",
            "opus",
        ])
        .output()
        .expect("spawn condukt shadow-run exec");
    assert_not_an_arg_parse_error(&exec, "shadow-run exec --run");
    assert!(
        exec.status.success(),
        "exec must accept a run namespace: stdout={} stderr={}",
        stdout_of(&exec),
        stderr_of(&exec)
    );
    let path = stdout_of(&exec).trim().to_string();
    assert!(
        Path::new(&path).exists(),
        "printed shadow worktree path must exist: {path}"
    );

    // The shadow worker's committed-but-never-merged change.
    std::fs::write(Path::new(&path).join("shadow.txt"), "shadow output\n").unwrap();
    git(Path::new(&path), &["add", "."]);
    git(Path::new(&path), &["commit", "-m", "shadow attempt"]);

    let during = f.branches();
    assert_ne!(
        during, before,
        "exec must have created a branch for the shadow worktree"
    );

    let finish = f
        .cmd(&[
            "shadow-run",
            "finish",
            "--run",
            "run-A",
            "--path",
            &path,
            "--branch",
            "shadow/t1-opus",
            "--title",
            "t1 shadow attempt",
            "--model",
            "opus",
            "--pass",
            "--cost",
            "0.42",
            "--duration",
            "12.5",
        ])
        .output()
        .expect("spawn condukt shadow-run finish");
    assert_not_an_arg_parse_error(&finish, "shadow-run finish --run");
    assert!(
        finish.status.success(),
        "finish must succeed for the namespace exec actually used: stdout={} stderr={}",
        stdout_of(&finish),
        stderr_of(&finish)
    );

    assert!(
        !Path::new(&path).exists(),
        "shadow worktree dir must be removed by finish"
    );
    // The load-bearing assertion: real `git branch` output, not a return value.
    assert_eq!(
        f.branches(),
        before,
        "finish must leave NO branch behind — a stranded namespaced branch is \
         exactly the two-sided defect this test pins"
    );
    assert!(
        !f.repo.join("shadow.txt").exists(),
        "shadow content must never land on main"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 2. A namespace mismatch is never a silent success.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn finish_with_mismatched_run_namespace_fails_observably() {
    let f = Fixture::new();

    let exec = f
        .cmd(&[
            "shadow-run",
            "exec",
            "--run",
            "run-A",
            "--topic",
            "t9-shadow",
            "--branch",
            "shadow/t9-opus",
            "--model",
            "opus",
        ])
        .output()
        .expect("spawn condukt shadow-run exec");
    assert_not_an_arg_parse_error(&exec, "shadow-run exec --run");
    assert!(
        exec.status.success(),
        "exec must accept a run namespace: stdout={} stderr={}",
        stdout_of(&exec),
        stderr_of(&exec)
    );
    let path = stdout_of(&exec).trim().to_string();
    let after_exec = f.branches();

    // Finish under a DIFFERENT run namespace than the one exec used.
    let finish = f
        .cmd(&[
            "shadow-run",
            "finish",
            "--run",
            "run-B",
            "--path",
            &path,
            "--branch",
            "shadow/t9-opus",
            "--title",
            "t9 shadow attempt",
            "--model",
            "opus",
            "--pass",
            "--cost",
            "0.1",
            "--duration",
            "1.0",
        ])
        .output()
        .expect("spawn condukt shadow-run finish");
    assert_not_an_arg_parse_error(&finish, "shadow-run finish --run");
    assert!(
        !finish.status.success(),
        "a namespace mismatch must be observable as a non-zero exit, not a \
         quiet success that strands the real branch: stdout={} stderr={}",
        stdout_of(&finish),
        stderr_of(&finish)
    );
    assert!(
        !stdout_of(&finish).contains("discarded"),
        "a failed finish must not print a success line: {}",
        stdout_of(&finish)
    );
    // Nothing may be destroyed silently either: the branch exec really made is
    // still there for the operator to clean up, and the failure told them so.
    assert_eq!(
        f.branches(),
        after_exec,
        "a rejected finish must not have force-deleted some other branch"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 3. ANTI-VACUITY CONTROL — the legacy un-namespaced path is unchanged.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn legacy_unnamespaced_shadow_run_keeps_identical_path_and_ref() {
    let f = Fixture::new();
    let before = f.branches();

    let exec = f
        .cmd(&[
            "shadow-run",
            "exec",
            "--topic",
            "t2-shadow",
            "--branch",
            "shadow/t2-haiku",
            "--model",
            "haiku",
        ])
        .output()
        .expect("spawn condukt shadow-run exec");
    assert!(
        exec.status.success(),
        "legacy exec must keep working: stdout={} stderr={}",
        stdout_of(&exec),
        stderr_of(&exec)
    );
    let path = stdout_of(&exec).trim().to_string();

    // Byte-identical PATH: <worktree_base>/<topic>, no run segment folded in.
    assert_eq!(
        Path::new(&path),
        f.worktree_base.join("t2-shadow"),
        "an un-namespaced exec must land on exactly the legacy layout"
    );
    // Byte-identical REF: the caller's branch verbatim.
    let mut expected = before.clone();
    expected.push("shadow/t2-haiku".to_string());
    expected.sort();
    assert_eq!(
        f.branches(),
        expected,
        "an un-namespaced exec must create exactly the caller's branch ref"
    );

    std::fs::write(Path::new(&path).join("shadow.txt"), "legacy shadow\n").unwrap();
    git(Path::new(&path), &["add", "."]);
    git(Path::new(&path), &["commit", "-m", "legacy shadow attempt"]);

    let finish = f
        .cmd(&[
            "shadow-run",
            "finish",
            "--path",
            &path,
            "--branch",
            "shadow/t2-haiku",
            "--title",
            "t2 shadow attempt",
            "--model",
            "haiku",
            "--pass",
            "--cost",
            "0.05",
            "--duration",
            "3.2",
        ])
        .output()
        .expect("spawn condukt shadow-run finish");
    assert!(
        finish.status.success(),
        "legacy finish must keep working (fugu-router is a soft dep): stdout={} stderr={}",
        stdout_of(&finish),
        stderr_of(&finish)
    );
    assert!(
        !Path::new(&path).exists(),
        "legacy finish must still remove the worktree dir"
    );
    assert_eq!(
        f.branches(),
        before,
        "legacy finish must still force-delete exactly the caller's branch"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 4. No un-namespaced production call sites.
// ─────────────────────────────────────────────────────────────────────────

/// Replace every comment body and string/char literal body with spaces,
/// preserving byte offsets (and therefore line numbers). Doc comments are
/// comments: a `worktree::create` mentioned in prose must neither trip this
/// scan nor satisfy it.
fn mask_comments_and_literals(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for x in out.iter_mut().take(to).skip(from) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    while i < b.len() {
        // Raw string: r"..." / r#"..."# / br#"..."#
        let raw_start = if b[i] == b'r' {
            Some(i + 1)
        } else if b[i] == b'b' && i + 1 < b.len() && b[i + 1] == b'r' {
            Some(i + 2)
        } else {
            None
        };
        if let Some(hs) = raw_start {
            // Must not be part of a longer identifier.
            let prev_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
            let mut h = hs;
            while h < b.len() && b[h] == b'#' {
                h += 1;
            }
            if prev_ok && h < b.len() && b[h] == b'"' {
                let hashes = h - hs;
                let mut j = h + 1;
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == b'"' {
                        let mut k = j + 1;
                        let mut n = 0;
                        while k < b.len() && b[k] == b'#' && n < hashes {
                            k += 1;
                            n += 1;
                        }
                        if n == hashes {
                            j = k;
                            break;
                        }
                    }
                    j += 1;
                }
                blank(&mut out, h + 1, j.min(b.len()));
                i = j.min(b.len());
                continue;
            }
        }
        match b[i] {
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                let mut j = i;
                while j < b.len() && b[j] != b'\n' {
                    j += 1;
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                let mut depth = 1usize;
                let mut j = i + 2;
                while j < b.len() && depth > 0 {
                    if b[j] == b'/' && j + 1 < b.len() && b[j + 1] == b'*' {
                        depth += 1;
                        j += 2;
                    } else if b[j] == b'*' && j + 1 < b.len() && b[j + 1] == b'/' {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() {
                    if b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if b[j] == b'"' {
                        break;
                    }
                    j += 1;
                }
                blank(&mut out, i + 1, j.min(b.len()));
                i = (j + 1).min(b.len());
            }
            b'\'' => {
                // Char literal vs lifetime. `'\x'` / `'x'` are literals;
                // anything else is a lifetime and must not open a state.
                if i + 1 < b.len() && b[i + 1] == b'\\' {
                    let mut j = i + 2;
                    while j < b.len() && b[j] != b'\'' {
                        j += 1;
                    }
                    blank(&mut out, i + 1, j.min(b.len()));
                    i = (j + 1).min(b.len());
                } else if i + 2 < b.len() && b[i + 2] == b'\'' {
                    blank(&mut out, i + 1, i + 2);
                    i += 3;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Blank out every `#[cfg(test)]`-attributed item, brace-matched.
fn strip_cfg_test(mut masked: Vec<u8>) -> Vec<u8> {
    const NEEDLE: &[u8] = b"#[cfg(test)]";
    let mut from = 0usize;
    while let Some(rel) = masked[from..]
        .windows(NEEDLE.len())
        .position(|w| w == NEEDLE)
    {
        let start = from + rel;
        let mut j = start + NEEDLE.len();
        // Walk to the item's body: `{ ... }` for a mod/fn, `;` for a `use`.
        while j < masked.len() && masked[j] != b'{' && masked[j] != b';' {
            j += 1;
        }
        let end = if j < masked.len() && masked[j] == b'{' {
            let mut depth = 0usize;
            let mut k = j;
            loop {
                if k >= masked.len() {
                    break k;
                }
                if masked[k] == b'{' {
                    depth += 1;
                } else if masked[k] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break k + 1;
                    }
                }
                k += 1;
            }
        } else {
            (j + 1).min(masked.len())
        };
        for x in masked.iter_mut().take(end).skip(start) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
        from = end;
        if from + NEEDLE.len() > masked.len() {
            break;
        }
    }
    masked
}

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut i = 0usize;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            v.push(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    v
}

fn line_of(src: &[u8], off: usize) -> usize {
    src[..off].iter().filter(|c| **c == b'\n').count() + 1
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read_dir") {
        let p = e.expect("dir entry").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// What a scan of one file found: the offending (un-namespaced, non-test,
/// non-comment) `create(` call lines, plus witnesses that the two legitimate
/// sites were actually SEEN rather than masked away. A scan that sees nothing
/// at all is inert, and an inert scan reports `clean`.
struct ScanResult {
    offenders: Vec<usize>,
    saw_definition: bool,
    saw_delegation: bool,
}

fn scan_create_calls(text: &str, is_worktree_rs: bool) -> ScanResult {
    let code = strip_cfg_test(mask_comments_and_literals(text));
    let mut r = ScanResult {
        offenders: Vec::new(),
        saw_definition: false,
        saw_delegation: false,
    };

    // `create_namespaced`'s body: the one place a bare `create(` call is
    // legitimate (it is the delegation the namespaced constructor makes).
    let delegation_span: Option<(usize, usize)> = if is_worktree_rs {
        find_all(&code, b"fn create_namespaced").first().map(|&s| {
            let mut j = s;
            while j < code.len() && code[j] != b'{' {
                j += 1;
            }
            let mut depth = 0usize;
            let mut k = j;
            let end = loop {
                if k >= code.len() {
                    break k;
                }
                if code[k] == b'{' {
                    depth += 1;
                } else if code[k] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break k + 1;
                    }
                }
                k += 1;
            };
            (s, end)
        })
    } else {
        None
    };

    for off in find_all(&code, b"create") {
        // Whole-identifier match only: excludes `create_namespaced`,
        // `create_dir_all`, `recreate`, ...
        let after = off + b"create".len();
        if after < code.len() && (code[after].is_ascii_alphanumeric() || code[after] == b'_') {
            continue;
        }
        if off > 0 && (code[off - 1].is_ascii_alphanumeric() || code[off - 1] == b'_') {
            continue;
        }
        // It must be a call: `create(` possibly with whitespace.
        let mut c = after;
        while c < code.len() && (code[c] == b' ' || code[c] == b'\n' || code[c] == b'\t') {
            c += 1;
        }
        if c >= code.len() || code[c] != b'(' {
            continue;
        }
        // A method call (`.create(true)` on OpenOptions) is unrelated.
        if off > 0 && code[off - 1] == b'.' {
            continue;
        }

        // Allow-list #1: the definition itself.
        let trimmed = String::from_utf8_lossy(&code[..off]).trim_end().to_string();
        if is_worktree_rs && trimmed.ends_with("fn") {
            r.saw_definition = true;
            continue;
        }
        // Allow-list #2: create_namespaced's own delegation.
        if let Some((s, e)) = delegation_span {
            if off > s && off < e {
                r.saw_delegation = true;
                continue;
            }
        }
        r.offenders.push(line_of(&code, off));
    }
    r
}

/// ANTI-VACUITY CONTROL for the scanner itself. Before trusting the source
/// scan's verdict on the real tree, prove on synthetic input that it
/// discriminates the shapes it must: a real call is caught, a doc-comment
/// mention is not, a `#[cfg(test)]` call is not, and lookalike identifiers
/// (`create_namespaced`, `create_dir_all`, `.create(true)`) are not.
#[test]
fn scanner_discriminates_real_calls_from_comments_and_cfg_test() {
    // A real production call IS caught.
    let caught = scan_create_calls(
        "fn run() {\n    let p = worktree::create(&repo, &base, &t, &b)?;\n}\n",
        false,
    );
    assert_eq!(
        caught.offenders,
        vec![2],
        "a live production call to the un-namespaced constructor must be caught"
    );

    // A doc comment / line comment / block comment mention is NOT caught --
    // and must not be mistaken for coverage either.
    let commented = scan_create_calls(
        "/// See worktree::create(&repo, ...) for the legacy shape.\n\
         // let p = worktree::create(&repo, &base, &t, &b)?;\n\
         /* worktree::create(a, b) */\n\
         fn run() {}\n",
        false,
    );
    assert!(
        commented.offenders.is_empty(),
        "prose must not trip the scan, got {:?}",
        commented.offenders
    );

    // A `#[cfg(test)]` call is NOT caught.
    let in_test = scan_create_calls(
        "fn run() {}\n\
         #[cfg(test)]\n\
         mod tests {\n\
         use super::*;\n\
         #[test]\n\
         fn t() {\n\
         let p = worktree::create(&repo, &base, \"t1\", \"b\").unwrap();\n\
         assert!(p.exists());\n\
         }\n\
         }\n",
        false,
    );
    assert!(
        in_test.offenders.is_empty(),
        "a #[cfg(test)] call must not trip the scan, got {:?}",
        in_test.offenders
    );

    // ...but a production call in the SAME file as a #[cfg(test)] block still is.
    let mixed = scan_create_calls(
        "fn run() {\n    worktree::create(&r, &b, &t, &br)?;\n}\n\
         #[cfg(test)]\n\
         mod tests {\n    fn t() { worktree::create(&r, &b, &t, &br).unwrap(); }\n}\n",
        false,
    );
    assert_eq!(
        mixed.offenders,
        vec![2],
        "stripping #[cfg(test)] must not also blank the production code above it"
    );

    // Lookalikes are NOT caught.
    let lookalikes = scan_create_calls(
        "fn run() {\n\
         worktree::create_namespaced(&r, &b, run, &t, &br)?;\n\
         std::fs::create_dir_all(&d)?;\n\
         OpenOptions::new().create(true).open(&p)?;\n\
         }\n",
        false,
    );
    assert!(
        lookalikes.offenders.is_empty(),
        "create_namespaced / create_dir_all / .create(true) must not trip the scan, got {:?}",
        lookalikes.offenders
    );

    // The two allow-listed sites in worktree.rs are recognised as such.
    let wt = scan_create_calls(
        "pub fn create_namespaced(run: Option<&str>) -> Result<()> {\n\
         match run {\n\
         None => create(repo, base, topic, branch),\n\
         Some(run) => create(repo, base, &nt, &nb),\n\
         }\n\
         }\n\
         pub fn create(repo: &Path) -> Result<PathBuf> { Ok(PathBuf::new()) }\n",
        true,
    );
    assert!(wt.saw_delegation, "delegation site must be recognised");
    assert!(wt.saw_definition, "definition site must be recognised");
    assert!(
        wt.offenders.is_empty(),
        "the two allow-listed sites must not be reported, got {:?}",
        wt.offenders
    );
}

#[test]
fn no_unnamespaced_worktree_create_in_production_code() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src_root, &mut files);
    files.sort();
    assert!(
        files.len() > 10,
        "sanity: the scan must actually find condukt's sources, found {}",
        files.len()
    );

    let mut offenders: Vec<String> = Vec::new();
    // Proof the scan is not inert: the masking must NOT have eaten the known
    // legitimate sites in worktree.rs.
    let mut saw_definition = false;
    let mut saw_delegation = false;

    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let is_worktree_rs = file
            .file_name()
            .map(|n| n == "worktree.rs")
            .unwrap_or(false);
        let r = scan_create_calls(&text, is_worktree_rs);
        saw_definition |= r.saw_definition;
        saw_delegation |= r.saw_delegation;
        for line in r.offenders {
            offenders.push(format!(
                "{}:{}",
                file.strip_prefix(&src_root).unwrap_or(file).display(),
                line
            ));
        }
    }

    assert!(
        saw_definition,
        "scan is inert: it did not even see `pub fn create(` in worktree.rs"
    );
    assert!(
        saw_delegation,
        "scan is inert: it did not see create_namespaced's delegation to create()"
    );
    assert!(
        offenders.is_empty(),
        "production code must call worktree::create_namespaced, never the \
         un-namespaced worktree::create. Offending call sites (relative to \
         crates/condukt/src/): {offenders:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// 1b. The mirror gap must CLOSE, not MOVE.
//
// This is the same property as (1) but expressed entirely through flags that
// exist TODAY, so it fails for the real reason rather than at argument
// parsing: `condukt worktree create --run` already namespaces (main.rs:3142),
// while `shadow-run finish` force-deletes whatever raw `--branch` the caller
// hands it (shadow_run.rs -> worktree::discard). Namespacing only `exec`
// would reproduce exactly this shape: the worktree goes away, the real
// namespaced branch is stranded, and the discard aims at a ref that does not
// exist.
//
// The desired end state asserted here is the invariant, not the mechanism:
// after a namespaced create, a finish handed the SAME logical branch must
// leave the repository's branch list where it started.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn namespaced_create_then_finish_must_not_strand_the_namespaced_branch() {
    let f = Fixture::new();
    let before = f.branches();
    assert_eq!(before, vec!["main".to_string()]);

    // Namespaced create — the side that is already migrated.
    let create = f
        .cmd(&[
            "worktree",
            "create",
            "--run",
            "run-A",
            "--topic",
            "t5-shadow",
            "--branch",
            "shadow/t5-opus",
        ])
        .output()
        .expect("spawn condukt worktree create");
    assert_not_an_arg_parse_error(&create, "worktree create --run");
    assert!(
        create.status.success(),
        "namespaced worktree create must succeed: stdout={} stderr={}",
        stdout_of(&create),
        stderr_of(&create)
    );
    let path = stdout_of(&create).trim().to_string();
    assert!(
        Path::new(&path).exists(),
        "worktree path must exist: {path}"
    );
    assert_eq!(
        f.branches(),
        vec!["main".to_string(), "shadow/run-A/t5-opus".to_string()],
        "a namespaced create must produce the run-scoped ref"
    );

    // Finish handed the SAME logical branch the caller asked for.
    let finish = f
        .cmd(&[
            "shadow-run",
            "finish",
            "--path",
            &path,
            "--branch",
            "shadow/t5-opus",
            "--title",
            "t5 shadow attempt",
            "--model",
            "opus",
            "--pass",
            "--cost",
            "0.2",
            "--duration",
            "4.0",
        ])
        .output()
        .expect("spawn condukt shadow-run finish");

    let leftover = f.branches();
    assert_eq!(
        leftover,
        before,
        "the run-scoped branch was STRANDED: finish force-deleted a ref that \
         does not exist while the real one survives. finish exit={:?} \
         stdout={} stderr={}",
        finish.status.code(),
        stdout_of(&finish),
        stderr_of(&finish)
    );
    assert!(
        !Path::new(&path).exists(),
        "the shadow worktree dir must still be removed"
    );
}
