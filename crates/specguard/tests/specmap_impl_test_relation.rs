// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for backlog 64e52a8d: the spec-map `impl_and_test` relation must be
//! reachable. At HEAD `attribute_path` keys every new path by itself, so a test
//! file can never join an impl file's entry and `audit` reports every impl entry
//! as `untested`. Everything here goes through the built binary against a
//! throwaway git repo; the real repo's map state is never read.

use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Repo with three crates committed after a seed commit:
///   * `foo`  — src/foo.rs + tests/foo.rs (same stem) + inline nothing
///   * `qux`  — src/lib.rs + tests/integration.rs (differently named test)
///   * `bar`  — src/bar.rs, NO test of any kind (control)
///
/// Returns the seed sha to use as `--baseline`.
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(repo, "README.md", "seed\n");
    write(
        repo,
        "specguard.toml",
        r#"
[project]
name = "Demo"
root = "."

[output]
report_dir = "reports"
sentinel = ".pending"

[[area]]
name = "crates"
globs = ["crates/**"]
canon = ["docs/spec.md"]
"#,
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let seed = git(repo, &["rev-parse", "HEAD"]);

    write(repo, "crates/foo/src/foo.rs", "pub fn foo() -> u32 { 1 }\n");
    write(
        repo,
        "crates/foo/tests/foo.rs",
        "#[test]\nfn foo_works() { assert_eq!(foo::foo(), 1); }\n",
    );
    write(repo, "crates/qux/src/lib.rs", "pub fn qux() -> u32 { 2 }\n");
    write(
        repo,
        "crates/qux/tests/integration.rs",
        "#[test]\nfn qux_works() { assert_eq!(qux::qux(), 2); }\n",
    );
    write(repo, "crates/bar/src/bar.rs", "pub fn bar() -> u32 { 3 }\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "crates"]);
    seed
}

fn specguard(repo: &Path, seed: &str, sub: &[&str]) -> std::process::Output {
    let home = repo.join(".fake-home");
    fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .env("HOME", &home)
        .env_remove("SPECGUARD_BASELINE_REF")
        .args([
            "--config",
            "specguard.toml",
            "--baseline",
            seed,
            "--date",
            "2026-01-01",
        ])
        .args(sub)
        .output()
        .expect("specguard runs")
}

fn built_map(repo: &Path, seed: &str) -> serde_json::Value {
    let b = specguard(repo, seed, &["map", "build"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let l = specguard(repo, seed, &["map", "list", "--json"]);
    assert!(l.status.success(), "{}", String::from_utf8_lossy(&l.stderr));
    serde_json::from_slice(&l.stdout).expect("map list --json is JSON")
}

fn files(e: &serde_json::Value, field: &str) -> Vec<String> {
    e[field]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// Entries (key, impl_files, test_files) that reference `path` anywhere.
fn entries_with(map: &serde_json::Value, path: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
    map["entries"]
        .as_object()
        .unwrap()
        .iter()
        .filter_map(|(k, e)| {
            let (i, t) = (files(e, "impl_files"), files(e, "test_files"));
            (i.iter().chain(t.iter()).any(|f| f == path)).then(|| (k.clone(), i, t))
        })
        .collect()
}

#[test]
fn same_stem_test_file_joins_its_impl_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = fixture(repo);
    let map = built_map(repo, &seed);
    let map_dump = serde_json::to_string_pretty(&map).unwrap();

    let owners = entries_with(&map, "crates/foo/src/foo.rs");
    assert_eq!(owners.len(), 1, "impl file owned by one entry:\n{map_dump}");
    let (_, i, t) = &owners[0];
    assert!(
        i.iter().any(|f| f == "crates/foo/src/foo.rs")
            && t.iter().any(|f| f == "crates/foo/tests/foo.rs"),
        "foo's entry must hold BOTH src/foo.rs and tests/foo.rs (impl_and_test):\n{map_dump}"
    );
}

#[test]
fn crate_integration_test_joins_a_crate_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = fixture(repo);
    let map = built_map(repo, &seed);
    let map_dump = serde_json::to_string_pretty(&map).unwrap();

    let both = map["entries"].as_object().unwrap().values().any(|e| {
        files(e, "impl_files")
            .iter()
            .any(|f| f == "crates/qux/src/lib.rs")
            && files(e, "test_files")
                .iter()
                .any(|f| f == "crates/qux/tests/integration.rs")
    });
    assert!(
        both,
        "an entry must relate qux's src/lib.rs to its tests/integration.rs:\n{map_dump}"
    );
}

#[test]
fn audit_reports_untested_only_for_the_crate_without_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = fixture(repo);
    let _ = built_map(repo, &seed);
    let out = specguard(repo, &seed, &["audit"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let untested: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with("[untested]"))
        .collect();

    // Control (green at HEAD and after a correct fix): bar has no test at all.
    assert!(
        untested.iter().any(|l| l.contains("crates/bar/")),
        "bar has no test and MUST stay reported untested:\n{stdout}"
    );
    // RED at HEAD: foo and qux do have tests, so must not be reported.
    for tested in ["crates/foo/", "crates/qux/"] {
        assert!(
            !untested.iter().any(|l| l.contains(tested)),
            "{tested} has a test file and must not be reported untested:\n{stdout}"
        );
    }
}

/// Control: a lone impl file with no test stays `impl_only` (no test files) —
/// a fix that attaches tests to everything would fail this.
#[test]
fn control_impl_without_any_test_has_no_test_files() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = fixture(repo);
    let map = built_map(repo, &seed);
    let owners = entries_with(&map, "crates/bar/src/bar.rs");
    assert_eq!(owners.len(), 1, "bar impl must be mapped exactly once");
    assert!(
        owners[0].2.is_empty(),
        "bar has no test; its entry must have no test_files: {:?}",
        owners[0]
    );
    for (_, _, t) in &owners {
        assert!(!t
            .iter()
            .any(|f| f.contains("crates/foo/") || f.contains("crates/qux/")));
    }
}
