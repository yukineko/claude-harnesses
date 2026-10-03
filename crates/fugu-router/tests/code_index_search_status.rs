//! Independent verification of the `fugu-router code-index search` exit-status
//! contract (backlog 3f3a0e90), written by a verifier that did not author the
//! implementation:
//!
//! * index read → JSON array on stdout, exit 0 (`[]` = read, nothing matched);
//! * index absent → empty stdout, exit 3;
//! * index unreadable (non-NotFound IO error, a non-blank line that is not a
//!   symbol record, or a zero-symbol body the build meta does not vouch for)
//!   → empty stdout, exit 4.
//!
//! Every fixture lives in its own temp dir (`--root`) with an isolated HOME,
//! so nothing here reads or writes the live repo's `.fugu/` index.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EXIT_ABSENT: i32 = 3;
const EXIT_UNREADABLE: i32 = 4;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-cis-status-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the binary with an isolated HOME and cwd. Returns (exit, stdout, stderr).
fn run(home: &Path, cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn search(root: &Path, query: &str) -> (i32, String, String) {
    let home = temp_dir("home");
    run(
        &home,
        root,
        &[
            "code-index",
            "search",
            "--query",
            query,
            "--root",
            root.to_str().unwrap(),
        ],
    )
}

fn build(root: &Path) {
    let home = temp_dir("home");
    let (rc, out, err) = run(
        &home,
        root,
        &["code-index", "build", "--root", root.to_str().unwrap()],
    );
    assert_eq!(rc, 0, "build failed: stdout={out:?} stderr={err:?}");
}

/// A temp git repo with the given tracked files committed.
fn git_repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = temp_dir(tag);
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);
    dir
}

fn rust_repo(tag: &str) -> PathBuf {
    git_repo(
        tag,
        &[(
            "lib.rs",
            "pub fn extract_symbols(contents: &str) -> i32 {\n    0\n}\n\nstruct Widget {\n    id: i32,\n}\n",
        )],
    )
}

fn index_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.jsonl")
}

fn meta_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.meta.json")
}

fn assert_failed_with(code: i32, want: i32, stdout: &str, stderr: &str, what: &str) {
    assert_eq!(
        code, want,
        "{what}: expected exit {want}, got {code}; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.trim().is_empty(),
        "{what}: a non-result must print nothing on stdout (a `[]` would read as \
         'searched, nothing found'), got {stdout:?}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "{what}: a non-result must carry a stderr diagnostic"
    );
}

fn parse_array(stdout: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout:?}"));
    v.as_array()
        .unwrap_or_else(|| panic!("stdout is not a JSON array: {stdout:?}"))
        .clone()
}

#[test]
fn absent_index_exits_3_with_empty_stdout() {
    let root = temp_dir("absent");
    let (code, out, err) = search(&root, "extract_symbols");
    assert_failed_with(code, EXIT_ABSENT, &out, &err, "absent index");
}

#[test]
fn corrupt_line_in_built_index_exits_4() {
    // A real build, then one corrupt line appended: the valid records would
    // still match, so a lenient loader would answer with a (partial) hit list.
    let root = rust_repo("corrupt");
    build(&root);
    let mut body = std::fs::read_to_string(index_path(&root)).unwrap();
    assert!(!body.trim().is_empty(), "sanity: build produced symbols");
    body.push_str("{this is not a symbol record\n");
    std::fs::write(index_path(&root), body).unwrap();
    let (code, out, err) = search(&root, "extract_symbols");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "corrupt line");
}

#[test]
fn only_corrupt_line_exits_4_not_empty_array() {
    let root = temp_dir("corrupt-only");
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), "garbage\n").unwrap();
    let (code, out, err) = search(&root, "zzz_nothing");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "corrupt-only index");
}

#[test]
fn empty_index_without_meta_exits_4() {
    let root = temp_dir("empty-nometa");
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), "").unwrap();
    assert!(!meta_path(&root).exists());
    let (code, out, err) = search(&root, "anything");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "empty index, no meta");
}

#[test]
fn empty_index_with_meta_claiming_symbols_exits_4() {
    // The meta says the build produced symbols, the body has none: truncated.
    let root = temp_dir("empty-meta-nonzero");
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), "\n").unwrap();
    std::fs::write(
        meta_path(&root),
        r#"{"fingerprint":"0000000000000000","files":1,"symbols":2}"#,
    )
    .unwrap();
    let (code, out, err) = search(&root, "anything");
    assert_failed_with(
        code,
        EXIT_UNREADABLE,
        &out,
        &err,
        "empty index, meta symbols:2",
    );
}

#[test]
fn empty_index_with_meta_symbols_zero_is_empty_array_exit_0() {
    let root = temp_dir("empty-meta-zero");
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), "").unwrap();
    std::fs::write(
        meta_path(&root),
        r#"{"fingerprint":"0000000000000000","files":0,"symbols":0}"#,
    )
    .unwrap();
    let (code, out, err) = search(&root, "anything");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(parse_array(&out).is_empty(), "expected [], got {out:?}");
}

#[test]
fn real_build_of_repo_without_rust_is_empty_array_exit_0() {
    // The vouched-empty case produced by the actual build path, not a
    // hand-written meta: a repo whose tracked files contain no `.rs`.
    let root = git_repo("no-rust", &[("README.md", "# hi\n")]);
    build(&root);
    let (code, out, err) = search(&root, "anything");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(parse_array(&out).is_empty(), "expected [], got {out:?}");
}

#[test]
fn index_path_is_a_directory_exits_4() {
    let root = temp_dir("dir");
    std::fs::create_dir_all(index_path(&root)).unwrap();
    let (code, out, err) = search(&root, "anything");
    assert_failed_with(
        code,
        EXIT_UNREADABLE,
        &out,
        &err,
        "index path is a directory",
    );
}

#[test]
fn non_utf8_index_exits_4() {
    let root = temp_dir("non-utf8");
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), [0xff_u8, 0xfe, 0x00, 0x80, b'\n']).unwrap();
    let (code, out, err) = search(&root, "anything");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "non-UTF-8 index");
}

#[test]
fn valid_index_no_match_is_empty_array_exit_0() {
    let root = rust_repo("nomatch");
    build(&root);
    let (code, out, err) = search(&root, "zzzqqq_nonexistent");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(parse_array(&out).is_empty(), "expected [], got {out:?}");
}

#[test]
fn valid_index_with_hit_is_nonempty_array_exit_0() {
    let root = rust_repo("hit");
    build(&root);
    let (code, out, err) = search(&root, "extract symbols");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    let arr = parse_array(&out);
    assert!(!arr.is_empty(), "expected a hit, got {out:?}");
    assert!(
        arr.iter().any(|h| h["name"] == "extract_symbols"),
        "expected extract_symbols among hits: {out}"
    );
}

#[test]
fn blank_lines_in_valid_index_are_tolerated() {
    let root = rust_repo("blank");
    build(&root);
    let body = std::fs::read_to_string(index_path(&root)).unwrap();
    std::fs::write(index_path(&root), format!("\n  \n{body}\n\n")).unwrap();
    let (code, out, err) = search(&root, "extract symbols");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(!parse_array(&out).is_empty(), "expected a hit, got {out:?}");
}

// ---- build-meta consistency (1a18b8c7) ----

const ALPHA: &str =
    r#"{"name":"alpha_widget","kind":"fn","file":"a.rs","line":1,"signature":"fn alpha_widget()"}"#;

/// A hand-written index: `.fugu/code-index.jsonl` holding exactly one valid
/// symbol, plus the given meta body (or no meta file at all).
fn one_symbol_index(tag: &str, meta: Option<&str>) -> PathBuf {
    let root = temp_dir(tag);
    std::fs::create_dir_all(root.join(".fugu")).unwrap();
    std::fs::write(index_path(&root), format!("{ALPHA}\n")).unwrap();
    if let Some(m) = meta {
        std::fs::write(meta_path(&root), m).unwrap();
    }
    root
}

#[test]
fn meta_count_exceeds_body_count_exits_4_naming_both_counts() {
    // Truncated body: one valid line survives, meta says the build wrote two.
    // A query for the lost symbol must not read as "read, nothing matched".
    let root = one_symbol_index(
        "meta2-body1",
        Some(r#"{"fingerprint":"x","files":1,"symbols":2}"#),
    );
    let (code, out, err) = search(&root, "beta");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "meta 2 vs body 1");
    assert!(
        err.contains('1') && err.contains('2'),
        "diagnostic must name both counts (body 1, meta 2): {err:?}"
    );
    assert!(
        err.contains("holds 1 symbols") && err.contains("records 2"),
        "diagnostic must attribute each count: {err:?}"
    );
}

#[test]
fn truncated_real_build_exits_4() {
    // Same shape produced by the real build path: drop the last record.
    let root = rust_repo("trunc-real");
    build(&root);
    let body = std::fs::read_to_string(index_path(&root)).unwrap();
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "sanity: build produced >=2 symbols: {body}"
    );
    std::fs::write(index_path(&root), format!("{}\n", lines[0])).unwrap();
    let (code, out, err) = search(&root, "zzzqqq_nonexistent");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "truncated real build");
}

#[test]
fn unparseable_meta_with_valid_body_exits_4() {
    let root = one_symbol_index("meta-garbage", Some("{not json"));
    let (code, out, err) = search(&root, "alpha widget");
    assert_failed_with(code, EXIT_UNREADABLE, &out, &err, "unparseable meta");
}

#[test]
fn meta_matching_body_with_hit_exits_0() {
    let root = one_symbol_index(
        "meta1-hit",
        Some(r#"{"fingerprint":"x","files":1,"symbols":1}"#),
    );
    let (code, out, err) = search(&root, "alpha widget");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    let arr = parse_array(&out);
    assert!(
        arr.iter().any(|h| h["name"] == "alpha_widget"),
        "expected alpha_widget hit: {out}"
    );
}

#[test]
fn meta_matching_body_no_match_is_empty_array_exit_0() {
    let root = one_symbol_index(
        "meta1-nomatch",
        Some(r#"{"fingerprint":"x","files":1,"symbols":1}"#),
    );
    let (code, out, err) = search(&root, "zzzqqq_nonexistent");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(parse_array(&out).is_empty(), "expected [], got {out:?}");
}

#[test]
fn no_meta_nonempty_body_is_accepted_exit_0() {
    // Documented limit: with no meta there is nothing to compare against,
    // so a fully parseable non-empty body is served as read.
    let root = one_symbol_index("nometa-body1", None);
    let (code, out, err) = search(&root, "alpha widget");
    assert_eq!(code, 0, "stdout={out:?} stderr={err:?}");
    assert!(
        parse_array(&out)
            .iter()
            .any(|h| h["name"] == "alpha_widget"),
        "expected alpha_widget hit: {out}"
    );
}
