// IMPLEMENTER-WRITTEN: these tests were written by the implementer of backlog
// 64e52a8d (not by the independent RED-test author). Each was observed failing
// against a mutation of the production code before being accepted.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Implementer tests for the spec-map impl↔test relation (backlog 64e52a8d):
//! inline `#[test]` credit, affix-named sibling tests, the unique-name and
//! ambiguity rules, and the explicit `specguard map link` writer. Everything
//! runs the built binary against a throwaway git repo.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

/// Init a repo with a seed commit; returns the seed sha (the `--baseline`).
fn init(repo: &Path) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\n\
         sentinel = \".pending\"\n\n[[area]]\nname = \"all\"\nglobs = [\"**\"]\n\
         canon = [\"docs/spec.md\"]\n",
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    git(repo, &["rev-parse", "HEAD"])
}

fn commit(repo: &Path, files: &[(&str, &str)]) {
    for (p, b) in files {
        write(repo, p, b);
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
}

fn specguard(repo: &Path, seed: &str, sub: &[&str]) -> Output {
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

/// Like [`specguard`] but WITHOUT `--baseline`, so `map sync` anchors on the
/// map's own `last_synced` (an up-to-date map then sees no change at all).
fn specguard_unpinned(repo: &Path, sub: &[&str]) -> Output {
    let home = repo.join(".fake-home");
    fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .env("HOME", &home)
        .env_remove("SPECGUARD_BASELINE_REF")
        .args(["--config", "specguard.toml", "--date", "2026-01-01"])
        .args(sub)
        .output()
        .expect("specguard runs")
}

fn ok(o: Output) -> Output {
    assert!(
        o.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn list(repo: &Path, seed: &str) -> serde_json::Value {
    let l = ok(specguard(repo, seed, &["map", "list", "--json"]));
    serde_json::from_slice(&l.stdout).expect("map list --json is JSON")
}

fn strs(e: &serde_json::Value, field: &str) -> Vec<String> {
    e[field]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// (impl_files, test_files) of the entry holding `impl_path` in impl_files.
fn owner_of(map: &serde_json::Value, impl_path: &str) -> (String, Vec<String>, Vec<String>) {
    let hits: Vec<_> = map["entries"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, e)| strs(e, "impl_files").iter().any(|f| f == impl_path))
        .map(|(k, e)| (k.clone(), strs(e, "impl_files"), strs(e, "test_files")))
        .collect();
    assert_eq!(hits.len(), 1, "{impl_path} owned once: {map:#}");
    hits.into_iter().next().unwrap()
}

const INLINE: &str = "pub fn a() -> u32 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_is_one() { assert_eq!(super::a(), 1); }\n}\n";
const INLINE_TOKIO: &str =
    "pub fn b() {}\n\n#[cfg(test)]\nmod tests {\n    #[tokio::test]\n    async fn b_runs() {}\n}\n";
const NO_TESTS: &str = "pub fn c() {}\n// mentions #[test] only in a comment\n/// and `#[cfg(test)]` in a doc\n#[cfg(test)]\nuse std::fmt;\n";

#[test]
fn inline_test_module_credits_the_impl_file_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("crates/a/src/a.rs", INLINE),
            ("crates/a/src/b.rs", INLINE_TOKIO),
            ("crates/a/src/c.rs", NO_TESTS),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/a/src/a.rs");
    assert_eq!(t, vec!["crates/a/src/a.rs".to_string()], "{map:#}");
    let (_, _, t) = owner_of(&map, "crates/a/src/b.rs");
    assert_eq!(t, vec!["crates/a/src/b.rs".to_string()], "{map:#}");
    // Attributes that only appear in comments / a cfg(test) `use` are not tests.
    let (_, _, t) = owner_of(&map, "crates/a/src/c.rs");
    assert!(t.is_empty(), "c.rs has no test functions: {map:#}");

    let audit = String::from_utf8_lossy(&ok(specguard(repo, &seed, &["audit"])).stdout).to_string();
    let untested: Vec<&str> = audit
        .lines()
        .filter(|l| l.trim_start().starts_with("[untested]"))
        .collect();
    assert!(
        untested.iter().any(|l| l.contains("crates/a/src/c.rs")),
        "{audit}"
    );
    assert!(
        !untested.iter().any(|l| l.contains("crates/a/src/a.rs")),
        "{audit}"
    );
}

#[test]
fn inline_credit_is_withdrawn_when_the_tests_go_away() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(repo, &[("crates/a/src/a.rs", INLINE)]);
    ok(specguard(repo, &seed, &["map", "build"]));
    commit(repo, &[("crates/a/src/a.rs", "pub fn a() -> u32 { 1 }\n")]);
    ok(specguard(repo, &seed, &["map", "sync"]));
    let (_, _, t) = owner_of(&list(repo, &seed), "crates/a/src/a.rs");
    assert!(t.is_empty(), "tests removed, credit must go: {t:?}");
}

#[test]
fn unreadable_impl_file_gets_no_inline_credit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[("crates/a/src/a.rs", INLINE), ("crates/a/src/b.rs", INLINE)],
    );
    // b.rs: committed with tests, but gone from the working tree before the
    // first build: the check cannot read it, and "could not check" must not
    // read as "tested".
    fs::remove_file(repo.join("crates/a/src/b.rs")).unwrap();
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/a/src/b.rs");
    assert!(t.is_empty(), "unreadable file must not be credited: {t:?}");
    let (_, _, t) = owner_of(&map, "crates/a/src/a.rs");
    assert_eq!(t, vec!["crates/a/src/a.rs".to_string()]);

    // a.rs: credited, then becomes unreadable with no new commit. The next
    // sync (anchored on the map's own last_synced = HEAD) applies no change to
    // it, yet must withdraw the credit.
    fs::remove_file(repo.join("crates/a/src/a.rs")).unwrap();
    ok(specguard_unpinned(repo, &["map", "sync"]));
    let (_, _, t) = owner_of(&list(repo, &seed), "crates/a/src/a.rs");
    assert!(t.is_empty(), "credit must go once unreadable: {t:?}");
}

#[test]
fn affix_named_sibling_test_joins_its_impl_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("pkg/foo.go", "package pkg\n"),
            ("pkg/foo_test.go", "package pkg\n"),
            ("web/button.ts", "export {}\n"),
            ("web/button.test.ts", "export {}\n"),
            ("py/test_util.py", "\n"),
            ("py/util.py", "\n"),
            // a test with no sibling stays test-only
            ("web/orphan.test.ts", "export {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    for (imp, test) in [
        ("pkg/foo.go", "pkg/foo_test.go"),
        ("web/button.ts", "web/button.test.ts"),
        ("py/util.py", "py/test_util.py"),
    ] {
        let (_, _, t) = owner_of(&map, imp);
        assert_eq!(t, vec![test.to_string()], "{imp}: {map:#}");
    }
    let orphan = &map["entries"]["web/orphan.test.ts"];
    assert_eq!(strs(orphan, "test_files"), vec!["web/orphan.test.ts"]);
    assert!(strs(orphan, "impl_files").is_empty());
}

#[test]
fn unique_name_under_src_joins_but_ambiguous_name_falls_to_crate_root() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("crates/k/src/lib.rs", "pub mod gate;\n"),
            ("crates/k/src/gate/run.rs", "pub fn r() {}\n"),
            ("crates/k/src/a/dup.rs", "pub fn d() {}\n"),
            ("crates/k/src/b/dup.rs", "pub fn d() {}\n"),
            ("crates/k/tests/run.rs", "#[test]\nfn t() {}\n"),
            ("crates/k/tests/dup.rs", "#[test]\nfn t() {}\n"),
            // mirrored path wins over name ambiguity
            ("crates/k/tests/a/dup.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/k/src/gate/run.rs");
    assert_eq!(t, vec!["crates/k/tests/run.rs".to_string()], "{map:#}");
    let (_, _, t) = owner_of(&map, "crates/k/src/a/dup.rs");
    assert_eq!(t, vec!["crates/k/tests/a/dup.rs".to_string()], "{map:#}");
    let (_, _, t) = owner_of(&map, "crates/k/src/b/dup.rs");
    assert!(
        t.is_empty(),
        "ambiguous name must not pick b/dup.rs: {map:#}"
    );
    let (_, _, t) = owner_of(&map, "crates/k/src/lib.rs");
    assert_eq!(t, vec!["crates/k/tests/dup.rs".to_string()], "{map:#}");
}

#[test]
fn crate_without_a_root_file_keeps_its_integration_test_test_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("crates/n/src/util.rs", "pub fn u() {}\n"),
            ("crates/n/tests/e2e.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/n/src/util.rs");
    assert!(t.is_empty(), "{map:#}");
    let e = &map["entries"]["crates/n/tests/e2e.rs"];
    assert_eq!(
        strs(e, "test_files"),
        vec!["crates/n/tests/e2e.rs"],
        "{map:#}"
    );
}

#[test]
fn map_link_relates_a_test_explicitly_and_survives_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("crates/n/src/util.rs", "pub fn u() {}\n"),
            ("crates/n/tests/e2e.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    ok(specguard(
        repo,
        &seed,
        &[
            "map",
            "link",
            "crates/n/tests/e2e.rs",
            "crates/n/src/util.rs",
        ],
    ));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/n/src/util.rs");
    assert_eq!(t, vec!["crates/n/tests/e2e.rs".to_string()], "{map:#}");
    assert!(
        map["entries"].get("crates/n/tests/e2e.rs").is_none(),
        "the test-only skeleton must be dropped: {map:#}"
    );

    commit(repo, &[("crates/n/tests/e2e.rs", "#[test]\nfn t2() {}\n")]);
    ok(specguard(repo, &seed, &["map", "sync"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/n/src/util.rs");
    assert_eq!(t, vec!["crates/n/tests/e2e.rs".to_string()], "{map:#}");
    assert!(
        map["entries"].get("crates/n/tests/e2e.rs").is_none(),
        "{map:#}"
    );
}

#[test]
fn map_link_refuses_unknown_key_and_missing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            ("crates/n/src/util.rs", "pub fn u() {}\n"),
            ("crates/n/tests/e2e.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let before = list(repo, &seed);

    let o = specguard(
        repo,
        &seed,
        &["map", "link", "crates/n/tests/e2e.rs", "nope"],
    );
    assert!(!o.status.success(), "unknown key must fail");
    let o = specguard(
        repo,
        &seed,
        &[
            "map",
            "link",
            "crates/n/tests/gone.rs",
            "crates/n/src/util.rs",
        ],
    );
    assert!(!o.status.success(), "missing test file must fail");
    assert_eq!(list(repo, &seed), before, "a refused link writes nothing");
}

#[test]
fn crate_root_is_lib_then_main_and_generic_names_never_match_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(
        repo,
        &[
            // binary-only crate: integration test goes to src/main.rs
            ("crates/b/src/main.rs", "fn main() {}\n"),
            ("crates/b/tests/cli.rs", "#[test]\nfn t() {}\n"),
            // lib crate with a nested bin main.rs: tests/main.rs is NOT matched
            // to src/bin/tool/main.rs by name (generic), it goes to lib.rs
            ("crates/m/src/lib.rs", "pub fn l() {}\n"),
            ("crates/m/src/bin/tool/main.rs", "fn main() {}\n"),
            ("crates/m/tests/main.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    ok(specguard(repo, &seed, &["map", "build"]));
    let map = list(repo, &seed);
    let (_, _, t) = owner_of(&map, "crates/b/src/main.rs");
    assert_eq!(t, vec!["crates/b/tests/cli.rs".to_string()], "{map:#}");
    let (_, _, t) = owner_of(&map, "crates/m/src/lib.rs");
    assert_eq!(t, vec!["crates/m/tests/main.rs".to_string()], "{map:#}");
    let (_, _, t) = owner_of(&map, "crates/m/src/bin/tool/main.rs");
    assert!(t.is_empty(), "{map:#}");
}

/// A test entry a consumer has authored (spec-doc, endpoint data, extra
/// files) is not a skeleton and must never be merged away by a sync.
#[test]
fn consumer_authored_test_entries_are_not_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init(repo);
    commit(
        repo,
        &[
            ("crates/q/src/lib.rs", "pub fn q() {}\n"),
            ("crates/q/tests/spec.rs", "#[test]\nfn t() {}\n"),
            ("crates/q/tests/api.rs", "#[test]\nfn t() {}\n"),
            ("crates/q/tests/ep.rs", "#[test]\nfn t() {}\n"),
            ("crates/q/tests/client.rs", "#[test]\nfn t() {}\n"),
            ("crates/q/tests/pair.rs", "#[test]\nfn t() {}\n"),
            ("crates/q/tests/pair2.rs", "#[test]\nfn t() {}\n"),
        ],
    );
    let head = git(repo, &["rev-parse", "HEAD"]);
    // Hand-written map: the test files are already mapped as consumer entries.
    let entry = |key: &str, tests: &[&str], extra: &str| {
        let tests: Vec<String> = tests.iter().map(|t| format!("\"{t}\"")).collect();
        format!(
            "[entries.\"{key}\"]\nkey = \"{key}\"\nstatus = \"tracked\"\nimpl_files = []\n\
             test_files = [{}]\n{extra}\n",
            tests.join(", ")
        )
    };
    let map_toml = format!(
        "last_synced = \"{head}\"\n\n{}{}{}{}{}",
        entry(
            "crates/q/tests/spec.rs",
            &["crates/q/tests/spec.rs"],
            "spec_doc = \"docs/q.md\""
        ),
        entry(
            "crates/q/tests/api.rs",
            &["crates/q/tests/api.rs"],
            "[entries.\"crates/q/tests/api.rs\".api]\nmethod = \"GET\"\nroute = \"/q\""
        ),
        entry(
            "crates/q/tests/ep.rs",
            &["crates/q/tests/ep.rs"],
            "kind = \"endpoint\""
        ),
        entry(
            "crates/q/tests/client.rs",
            &["crates/q/tests/client.rs"],
            "client_refs = [\"web/q.ts\"]"
        ),
        entry(
            "crates/q/tests/pair.rs",
            &["crates/q/tests/pair.rs", "crates/q/tests/pair2.rs"],
            ""
        ),
    );
    write(repo, ".specguard/spec-map.toml", &map_toml);
    // Touch the impl and every test so the sync sees them.
    commit(
        repo,
        &[
            ("crates/q/src/lib.rs", "pub fn q() { }\n"),
            ("crates/q/tests/spec.rs", "#[test]\nfn t2() {}\n"),
            ("crates/q/tests/api.rs", "#[test]\nfn t2() {}\n"),
            ("crates/q/tests/ep.rs", "#[test]\nfn t2() {}\n"),
            ("crates/q/tests/client.rs", "#[test]\nfn t2() {}\n"),
            ("crates/q/tests/pair.rs", "#[test]\nfn t2() {}\n"),
        ],
    );
    ok(specguard(repo, &head, &["map", "sync"]));
    let map = list(repo, &head);
    for key in [
        "crates/q/tests/spec.rs",
        "crates/q/tests/api.rs",
        "crates/q/tests/ep.rs",
        "crates/q/tests/client.rs",
        "crates/q/tests/pair.rs",
    ] {
        assert!(
            map["entries"].get(key).is_some(),
            "{key} merged away: {map:#}"
        );
    }
    let (_, _, t) = owner_of(&map, "crates/q/src/lib.rs");
    assert!(
        t.is_empty(),
        "no consumer test may move into lib.rs: {map:#}"
    );
}

/// A present but unreadable impl file cannot be checked for inline tests: it
/// gets no credit (reads as untested) and the build says which file it was.
#[cfg(unix)]
#[test]
fn unreadable_present_file_is_not_credited_and_is_named() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let seed = init(repo);
    commit(repo, &[("crates/a/src/a.rs", INLINE)]);
    let f = repo.join("crates/a/src/a.rs");
    fs::set_permissions(&f, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_to_string(&f).is_ok() {
        // Running with privileges that ignore file modes: the condition this
        // test needs cannot be produced here, so there is nothing to observe.
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        panic!("cannot make a file unreadable in this environment (root?)");
    }
    let out = ok(specguard(repo, &seed, &["map", "build"]));
    fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("inline tests not checked") && stderr.contains("crates/a/src/a.rs"),
        "stderr must name the unchecked file: {stderr}"
    );
    let (_, _, t) = owner_of(&list(repo, &seed), "crates/a/src/a.rs");
    assert!(t.is_empty(), "unreadable file must not be credited: {t:?}");
}
