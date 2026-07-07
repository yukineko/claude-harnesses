//! End-to-end tests for the `curate` subcommand CLI: spawn the real built binary
//! and assert on stdout/stderr + exit code. curate is a plain CLI (not a hook).

use std::path::Path;
use std::process::Command;

/// Run `curate <args...>`. Returns (exit_code, stdout, stderr). curate writes its
/// human status lines to stderr, so tests inspect both streams.
fn run(args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_curate");
    let out = Command::new(bin).args(args).output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A unique empty temp dir; used to point `--store` at a guaranteed-missing file.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("curate-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

#[test]
fn help_lists_real_subcommands() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage: curate"), "got: {stdout}");
    assert!(stdout.contains("candidates"), "got: {stdout}");
    assert!(stdout.contains("promote"), "got: {stdout}");
}

#[test]
fn promote_writes_curated_golden_end_to_end() {
    // Prove the write path end-to-end: seed a temp playbook store with one
    // mechanical entry, run the real `curate promote` binary against a temp
    // --root, and assert the golden file was actually written with the id.
    let root = temp_root("promote");
    let store = root.join("playbooks.jsonl");
    std::fs::write(
        &store,
        // Seed shape read by seed::load: {ts, title, done_criteria}. The
        // backticked command makes done_criteria mechanical → a runnable case.
        "{\"ts\":100,\"title\":\"promote write path\",\"done_criteria\":\"`cargo test -p curate` passes\"}\n",
    )
    .expect("seed store");

    let (code, _stdout, stderr) = run(&[
        "promote",
        "promote write path",
        "--dataset",
        "e2e",
        "--store",
        store.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "promote should exit 0; stderr: {stderr}");

    let dataset = root.join("evals").join("curated").join("e2e.jsonl");
    assert!(
        dataset.exists(),
        "curated golden should be written at {}",
        dataset.display()
    );
    let text = std::fs::read_to_string(&dataset).expect("read curated dataset");
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .expect("one golden line");
    let v: serde_json::Value = serde_json::from_str(line).expect("golden is valid JSON");
    let id = v["id"].as_str().expect("golden has an id");
    // id = slug(title) + short hash → stem is stable for a fixed title.
    assert!(
        id.starts_with("promote-write-path-"),
        "unexpected golden id: {id}"
    );
    // Mechanical criterion → a runnable case (cmd + assert.exit 0), not a draft.
    assert_eq!(
        v["cmd"],
        serde_json::json!(["cargo", "test", "-p", "curate"])
    );
    assert_eq!(v["assert"]["exit"], serde_json::json!(0));
    assert!(v.get("draft").is_none(), "should be runnable, not a draft");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn candidates_on_missing_store_reports_none_and_exits_0() {
    // Read-only subcommand pointed at a store that doesn't exist: the source
    // prints "no playbooks found in <path>" to stderr and returns Ok(()).
    let root = temp_root("cand");
    let store = root.join("does-not-exist.jsonl");
    assert!(!Path::new(&store).exists());
    let (code, _stdout, stderr) = run(&["candidates", "--store", store.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "empty/missing store is not an error for candidates"
    );
    assert!(stderr.contains("no playbooks found"), "got: {stderr}");
}
