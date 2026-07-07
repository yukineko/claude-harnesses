//! End-to-end test for `fugu-router code-index build|search` (code-RAG
//! slice-1): a deterministic lexical symbol index, per-repo at
//! `<root>/.fugu/code-index.jsonl`. No embeddings/external API.
//!
//! `code-index build` enumerates *git-tracked* files, so the fixture repo
//! used here is a real (temp) git repo with the seeded source committed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A unique, isolated temp dir containing a tiny git repo with one seeded
/// `.rs` file, so `git ls-files` has something deterministic to enumerate.
fn seeded_repo(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-router-code-index-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp repo dir");

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    };

    run_git(&["init", "-q"]);
    run_git(&["config", "user.email", "test@example.com"]);
    run_git(&["config", "user.name", "test"]);

    std::fs::write(
        dir.join("lib.rs"),
        "pub fn extract_symbols(contents: &str) -> i32 {\n    0\n}\n\nstruct Widget {\n    id: i32,\n}\n",
    )
    .expect("write seeded source");

    run_git(&["add", "-A"]);
    run_git(&["commit", "-q", "-m", "seed"]);

    dir
}

/// Run the binary with `args`. Returns (exit_code, stdout).
fn run(args: &[&str]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_fugu-router");
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn search_before_build_is_fail_soft_empty_array() {
    let dir = seeded_repo("presearch");
    let root = dir.to_string_lossy().into_owned();
    let (code, stdout) = run(&[
        "code-index",
        "search",
        "--query",
        "extract symbols",
        "--root",
        &root,
    ]);
    assert_eq!(
        code, 0,
        "search on a never-built index must exit 0, got {code}: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(
        v,
        serde_json::json!([]),
        "missing index must yield []: {stdout}"
    );
}

#[test]
fn build_then_search_round_trip_finds_indexed_symbol() {
    let dir = seeded_repo("roundtrip");
    let root = dir.to_string_lossy().into_owned();

    let (code, stdout) = run(&["code-index", "build", "--root", &root]);
    assert_eq!(code, 0, "build must exit 0, got {code}: {stdout}");
    let summary: serde_json::Value = serde_json::from_str(&stdout).expect("build prints JSON");
    assert_eq!(summary["files_scanned"], serde_json::json!(1));
    assert_eq!(summary["symbols_indexed"], serde_json::json!(2));

    // The index file was written at the documented convention path.
    assert!(
        dir.join(".fugu").join("code-index.jsonl").exists(),
        "index file must exist at <root>/.fugu/code-index.jsonl"
    );

    let (code, stdout) = run(&[
        "code-index",
        "search",
        "--query",
        "extract symbols",
        "--root",
        &root,
    ]);
    assert_eq!(code, 0, "search must exit 0, got {code}: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let arr = v.as_array().expect("search prints a JSON array");
    assert!(
        !arr.is_empty(),
        "a matching query must find the indexed symbol: {stdout}"
    );
    assert_eq!(
        arr[0]["name"].as_str(),
        Some("extract_symbols"),
        "the found symbol must be the seeded fn: {stdout}"
    );

    // An unrelated query yields [].
    let (code, stdout) = run(&[
        "code-index",
        "search",
        "--query",
        "quarterly billing invoice",
        "--root",
        &root,
    ]);
    assert_eq!(
        code, 0,
        "unrelated search must exit 0, got {code}: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(
        v,
        serde_json::json!([]),
        "an unrelated query must yield []: {stdout}"
    );
}

/// code-RAG slice-3: `build --if-stale` is a no-op on an unchanged tree and
/// rebuilds when a `.rs` file changes.
#[test]
fn build_if_stale_noops_when_unchanged_and_rebuilds_on_change() {
    let dir = seeded_repo("ifstale");
    let root = dir.to_string_lossy().into_owned();
    let index = dir.join(".fugu").join("code-index.jsonl");

    // First --if-stale build: no prior meta → must rebuild.
    let (code, stdout) = run(&["code-index", "build", "--if-stale", "--root", &root]);
    assert_eq!(code, 0, "first if-stale build must exit 0: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("build prints JSON");
    assert_eq!(
        v["rebuilt"],
        serde_json::json!(true),
        "first build (no meta) must rebuild: {stdout}"
    );
    let bytes_after_build = std::fs::read(&index).expect("index exists after build");

    // Second --if-stale build with no changes: must be a no-op, and the index
    // file bytes must be untouched.
    let (code, stdout) = run(&["code-index", "build", "--if-stale", "--root", &root]);
    assert_eq!(code, 0, "second if-stale build must exit 0: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("build prints JSON");
    assert_eq!(
        v["rebuilt"],
        serde_json::json!(false),
        "unchanged tree must be a no-op: {stdout}"
    );
    let bytes_unchanged = std::fs::read(&index).expect("index still exists");
    assert_eq!(
        bytes_after_build, bytes_unchanged,
        "a no-op must not rewrite the index file"
    );

    // Change a tracked .rs file → --if-stale must rebuild again.
    std::fs::write(
        dir.join("lib.rs"),
        "pub fn extract_symbols(contents: &str) -> i32 {\n    1\n}\n\nstruct Widget {\n    id: i32,\n}\n\npub fn brand_new_fn() {}\n",
    )
    .expect("edit seeded source");
    let (code, stdout) = run(&["code-index", "build", "--if-stale", "--root", &root]);
    assert_eq!(code, 0, "post-edit if-stale build must exit 0: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("build prints JSON");
    assert_eq!(
        v["rebuilt"],
        serde_json::json!(true),
        "an edited .rs file must trigger a rebuild: {stdout}"
    );
    assert_eq!(
        v["symbols_indexed"],
        serde_json::json!(3),
        "the rebuilt index must include the newly-added fn: {stdout}"
    );
}

/// Plain `build` (no `--if-stale`) always rebuilds and reports `rebuilt:true`
/// (back-compat: the flagless path is unconditional).
#[test]
fn plain_build_always_rebuilds() {
    let dir = seeded_repo("plainbuild");
    let root = dir.to_string_lossy().into_owned();
    for _ in 0..2 {
        let (code, stdout) = run(&["code-index", "build", "--root", &root]);
        assert_eq!(code, 0, "plain build must exit 0: {stdout}");
        let v: serde_json::Value = serde_json::from_str(&stdout).expect("build prints JSON");
        assert_eq!(
            v["rebuilt"],
            serde_json::json!(true),
            "plain build must always rebuild: {stdout}"
        );
        assert_eq!(v["files_scanned"], serde_json::json!(1));
        assert_eq!(v["symbols_indexed"], serde_json::json!(2));
    }
}
