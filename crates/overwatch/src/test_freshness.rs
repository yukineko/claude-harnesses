//! Regression-test freshness: reverse-lookup a `#[ignore = "<finding-id>: ..."]`
//! test from a `finding_id`, and re-run it to see whether the bug it pins
//! still reproduces. This is the strongest triage signal `bridge.rs` can add
//! to a backlog task: FAIL means the bug is still live (a strong reason to
//! fix), PASS means it may already be resolved (a reason to deprioritize).
//!
//! Convention (see `docs/DESIGN-continuous-audit-triage.md` §2.5): the
//! `#[ignore]` reason string must start with `"<finding-id>: "` so the link
//! between a finding and its regression test is a structural, greppable
//! convention rather than free-text prose. Tests written before this
//! convention (free-text `#[ignore]` reasons) simply won't be found — that's
//! fail-soft, not an error.
//!
//! Split into pure parsing (testable with in-memory fixtures, no I/O) and I/O
//! (directory walk, `cargo test` spawn) so the matching logic itself doesn't
//! need a real crate on disk to test.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Max time to wait on the reverse-looked-up regression test before giving
/// up. `cargo test` here includes a build step, so this is longer than the
/// 8s used for the plain `overwatch lease` subprocess call in ctxrot's
/// `run_with_timeout` — but it must still be bounded, since this runs inside
/// `overwatch review-queue --to-backlog`'s per-finding loop and a single
/// hung/deadlocking ignored test must not wedge the whole bridge.
const RUN_IGNORED_TEST_TIMEOUT_SECS: u64 = 60;

/// The outcome of re-running a regression test that was reverse-looked-up for
/// a finding-id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestFreshness {
    /// The test passed (the bug may already be fixed).
    Passing,
    /// The test failed (the bug still reproduces); carries the failure output.
    Failing { message: String },
    /// `cargo test` ran but reported zero matching tests (the fn name didn't
    /// resolve, e.g. it was renamed or removed).
    NotFound,
    /// `cargo` isn't on PATH, the crate doesn't build, or the process
    /// otherwise couldn't be spawned/completed. Fail-soft: never propagated
    /// as an error to the caller.
    ExecutionError,
    /// The source-tree scan that reverse-looks-up the regression test could
    /// not be COMPLETED — a directory or `.rs` file that exists was unreadable
    /// (PermissionDenied / IO error), so a "no matching test" answer cannot be
    /// trusted. Distinct from `NotFound` (a complete walk that genuinely found
    /// no test) precisely because it must NOT be rendered as "該当テストなし"
    /// (a deprioritize signal): an incomplete search is *undetermined*, and
    /// collapsing it into "no test" is the fail-open this state closes
    /// (backlog 50ad2c1e; twin of specguard testaudit/gather rounds #7/#11).
    ScanIncomplete { detail: String },
}

/// The result of reverse-looking-up an `#[ignore = "<finding-id>: ..."]` test
/// for a finding. A tri-state so the caller can tell "genuinely no test"
/// (`NotFound`) apart from "couldn't finish looking" (`ScanIncomplete`) — the
/// distinction the old `Option` return collapsed into a single `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoredTestLookup {
    /// A matching test was found: `(crate_name, test_path, fn_name)`.
    Found {
        crate_name: String,
        test_path: String,
        fn_name: String,
    },
    /// The walk completed and no `#[ignore]`d test matched the finding-id.
    NotFound,
    /// The source walk could not be completed (an existing dir/file was
    /// unreadable), so `NotFound` cannot be trusted. Carries a human-readable
    /// detail for the triage note. Fail-closed: surfaced, never folded away.
    ScanIncomplete { detail: String },
}

/// Search `search_root` for a Rust source file containing a test whose
/// `#[ignore = "<finding_id>: ...` reason matches the given `finding_id`, and
/// return the located test if found. `test_path` is the source file path
/// (relative to `search_root` when the file is under it).
///
/// Distinguishes three outcomes (backlog 50ad2c1e — the fail-open this closes):
/// - `Found` — a matching test resolved to a crate.
/// - `NotFound` — the walk COMPLETED and nothing matched (a *legitimate* empty:
///   a `.rs` that vanished mid-walk, non-UTF8 content, or a file with no crate
///   manifest above it are all "keep looking").
/// - `ScanIncomplete` — an existing directory or `.rs` file was UNREADABLE, so
///   the walk could not finish. This must not masquerade as `NotFound`: an
///   unreadable subtree could hold the very test we're looking for, and
///   reporting "no test" would falsely deprioritize the finding.
pub fn find_ignored_test(finding_id: &str, search_root: &Path) -> IgnoredTestLookup {
    let prefix = format!("{finding_id}: ");
    let files = match rust_source_files(search_root) {
        Ok(f) => f,
        Err(e) => {
            return IgnoredTestLookup::ScanIncomplete {
                detail: format!("source walk failed: {e}"),
            }
        }
    };
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // A file that vanished between listing and reading (TOCTOU) is a
            // legitimate "nothing here" — keep looking. Any OTHER read error
            // (PermissionDenied, IO) on a file that WAS listed means the scan
            // is incomplete: that file could hold the matching test.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return IgnoredTestLookup::ScanIncomplete {
                    detail: format!("cannot read '{}': {e}", path.display()),
                }
            }
        };
        let Some(fn_name) = extract_ignored_fn_name(&text, &prefix) else {
            continue;
        };
        // Matched the finding in this file — resolve its crate. A missing
        // manifest (`Ok(None)`) is "keep looking"; an unreadable-but-present
        // manifest (`Err`) is undetermined — we found the test but can't name
        // its crate, so we must not fall through to a false `NotFound`.
        match nearest_crate_name(&path) {
            Ok(Some(crate_name)) => {
                let test_path = path
                    .strip_prefix(search_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                return IgnoredTestLookup::Found {
                    crate_name,
                    test_path,
                    fn_name,
                };
            }
            Ok(None) => continue,
            Err(e) => {
                return IgnoredTestLookup::ScanIncomplete {
                    detail: format!("cannot resolve crate for '{}': {e}", path.display()),
                }
            }
        }
    }
    IgnoredTestLookup::NotFound
}

/// Run the reverse-looked-up test via `cargo test -p <crate_name> --
/// --ignored <fn_name>` and classify the outcome. Fail-soft: any spawn/IO
/// failure becomes `ExecutionError`, never a panic or `Err`.
pub fn run_ignored_test(crate_name: &str, fn_name: &str) -> TestFreshness {
    let child = match Command::new("cargo")
        .args(["test", "-p", crate_name, "--", "--ignored", fn_name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return TestFreshness::ExecutionError,
    };
    match run_with_timeout(child, Duration::from_secs(RUN_IGNORED_TEST_TIMEOUT_SECS)) {
        Some((success, stdout)) => classify_test_output(success, &String::from_utf8_lossy(&stdout)),
        None => TestFreshness::ExecutionError,
    }
}

/// Wait on an already-spawned child for at most `timeout`, killing (and
/// reaping) it on timeout so it never lingers. Returns `(success, stdout)` on
/// a completed exit; `None` on a timeout or wait error. Mirrors
/// `ctxrot::hooks::guard::run_with_timeout`.
fn run_with_timeout(mut child: std::process::Child, timeout: Duration) -> Option<(bool, Vec<u8>)> {
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let out = child.wait_with_output().ok()?;
            Some((status.success(), out.stdout))
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        Err(_) => None,
    }
}

// --- pure helpers (no I/O — directly unit-testable) -------------------------

/// Scan `source` for a line containing `#[ignore = "<prefix>...` and return
/// the name of the `fn` that follows it (the next `fn <name>(` line after the
/// `#[ignore]` attribute, skipping any other attributes like `#[test]` in
/// between). Returns `None` if no line matches the prefix, or a matching
/// `#[ignore]` isn't followed by a `fn` within a few lines.
fn extract_ignored_fn_name(source: &str, prefix: &str) -> Option<String> {
    let needle = format!("#[ignore = \"{prefix}");
    let lines: Vec<&str> = source.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with(&needle) {
            continue;
        }
        // Look ahead a handful of lines for the `fn <name>(` this attribute
        // annotates (other attributes, e.g. `#[test]`, may sit in between).
        for line in lines.iter().skip(i + 1).take(5) {
            if let Some(name) = parse_fn_name(line) {
                return Some(name);
            }
        }
    }
    None
}

/// Parse `fn <name>(` (optionally `async fn`/`pub fn`) out of a source line.
fn parse_fn_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let after_fn = trimmed
        .strip_prefix("async fn ")
        .or_else(|| trimmed.strip_prefix("pub fn "))
        .or_else(|| trimmed.strip_prefix("fn "))?;
    let name: String = after_fn
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Walk up from `file`'s directory looking for the nearest `Cargo.toml`, and
/// parse its `[package].name`.
///
/// Returns `Ok(None)` when no ancestor `Cargo.toml` exists, or the nearest one
/// exists but has no parseable `[package].name` — both are legitimate "can't
/// name a crate here, keep looking". Returns `Err` only when a `Cargo.toml`
/// that EXISTS is unreadable (PermissionDenied / IO): that's undetermined, not
/// "no crate", and must not silently become a `NotFound` upstream. Reading the
/// file directly (rather than `is_file()` then read) also avoids a TOCTOU:
/// `NotFound` means the manifest isn't there and we walk up.
fn nearest_crate_name(file: &Path) -> std::io::Result<Option<String>> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        match std::fs::read_to_string(&candidate) {
            // Exists and readable: this is the nearest manifest. Its parsed
            // name (or `None` if unparseable) is the answer — do not keep
            // walking up past a real Cargo.toml.
            Ok(text) => return Ok(parse_crate_name(&text)),
            // No manifest at this level — walk up to the parent.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Present but unreadable → undetermined, fail closed.
            Err(e) => return Err(e),
        }
        dir = d.parent();
    }
    Ok(None)
}

/// Parse `[package].name` out of a `Cargo.toml` document.
fn parse_crate_name(cargo_toml_text: &str) -> Option<String> {
    let value: toml::Value = cargo_toml_text.parse().ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// Classify a completed `cargo test -- --ignored <fn>` invocation. Pure so it
/// can be tested against captured fixture output without spawning `cargo`.
///
/// `cargo test -p <crate>` runs one "test result:" block PER TARGET (lib,
/// bin, each integration test file), so a plain substring search for
/// `"0 passed; 0 failed"` is unreliable: an unrelated target with zero tests
/// (e.g. the lib target when the fixture lives in the bin target) produces
/// that exact substring even though another target's block reports a real
/// failure. Distinguish by looking for an explicit `test result: FAILED`
/// block first (a genuine test failure, regardless of what other targets
/// report), then fall back to "did anything actually run" for the
/// NotFound/Passing split, and treat a non-zero exit with no FAILED block at
/// all (e.g. unknown package, compile error) as an execution error rather
/// than a test failure.
fn classify_test_output(success: bool, stdout: &str) -> TestFreshness {
    if stdout.contains("test result: FAILED") {
        return TestFreshness::Failing {
            message: extract_failure_message(stdout),
        };
    }
    if !success {
        return TestFreshness::ExecutionError;
    }
    let ran_something = stdout.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("test result: ok.") && !line.contains("0 passed; 0 failed")
    });
    if ran_something {
        TestFreshness::Passing
    } else {
        TestFreshness::NotFound
    }
}

/// Best-effort extraction of the failing test's panic/assertion message from
/// `cargo test` stdout, for inclusion in triage notes. Falls back to a fixed
/// string if the format doesn't match what's expected (never panics).
fn extract_failure_message(stdout: &str) -> String {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(msg) = trimmed.strip_prefix("thread '") {
            // e.g. "thread 'foo' panicked at ...: <message>"
            if let Some(idx) = msg.find("panicked at") {
                return msg[idx..].trim().to_string();
            }
        }
    }
    "test failed (no panic message captured)".to_string()
}

/// Recursively collect `.rs` file paths under `root`, skipping build/VCS
/// directories that would otherwise make the walk slow and noisy.
///
/// Fails closed on an incomplete walk (backlog 50ad2c1e; twin of specguard
/// `testaudit::collect_rs_files_inner`, rounds #7/#11): a directory that is
/// MISSING contributes no files (a `NotFound` — e.g. `root` doesn't exist, or a
/// subdir vanished mid-walk in a TOCTOU race), but a directory that EXISTS but
/// is UNREADABLE (PermissionDenied / IO), or a per-entry listing error, means
/// the result is INCOMPLETE — return `Err` instead of silently under-reporting.
/// The old `let Ok(entries) = read_dir else { continue }` + `entries.flatten()`
/// collapsed every such error into "no `.rs` files here", so the reverse-lookup
/// could report "no regression test" for a finding whose test lived in a
/// subtree it never managed to read.
fn rust_source_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules", ".condukt"];
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            // A per-entry error means the listing is INCOMPLETE; the old
            // `.flatten()` dropped it, silently under-reporting. Fail closed.
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !SKIP_DIRS.contains(&name) {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extract_ignored_fn_name_finds_matching_test() {
        let source = r#"
#[test]
#[ignore = "CA-overwatch-001: repros a stale-lease double-claim"]
fn stale_lease_allows_double_claim() {
    assert!(false);
}
"#;
        assert_eq!(
            extract_ignored_fn_name(source, "CA-overwatch-001: "),
            Some("stale_lease_allows_double_claim".to_string())
        );
    }

    #[test]
    fn extract_ignored_fn_name_returns_none_when_absent() {
        let source = r#"
#[test]
#[ignore = "CA-overwatch-002: some other finding"]
fn unrelated_test() {}
"#;
        assert_eq!(extract_ignored_fn_name(source, "CA-overwatch-001: "), None);
    }

    #[test]
    fn extract_ignored_fn_name_ignores_free_text_reasons() {
        // Pre-convention free-text `#[ignore]` reasons must not accidentally
        // match — the reason must start with exactly `"<finding_id>: "`.
        let source = r#"
#[test]
#[ignore = "re-review finding 1, see docs/x.md"]
fn legacy_free_text_test() {}
"#;
        assert_eq!(extract_ignored_fn_name(source, "1: "), None);
    }

    #[test]
    fn parse_fn_name_handles_pub_and_async() {
        assert_eq!(
            parse_fn_name("fn plain_test() {"),
            Some("plain_test".into())
        );
        assert_eq!(
            parse_fn_name("    pub fn pub_test() {"),
            Some("pub_test".into())
        );
        assert_eq!(
            parse_fn_name("async fn async_test() {"),
            Some("async_test".into())
        );
        assert_eq!(parse_fn_name("let fn_ptr = foo;"), None);
    }

    #[test]
    fn parse_crate_name_reads_package_name() {
        let toml = "[package]\nname = \"overwatch\"\nversion = \"0.1.30\"\n";
        assert_eq!(parse_crate_name(toml), Some("overwatch".to_string()));
    }

    #[test]
    fn parse_crate_name_none_when_no_package_table() {
        assert_eq!(parse_crate_name("[dependencies]\nfoo = \"1\"\n"), None);
    }

    #[test]
    fn classify_test_output_not_found_on_zero_tests() {
        assert_eq!(
            classify_test_output(
                true,
                "running 0 tests\n\ntest result: ok. 0 passed; 0 failed"
            ),
            TestFreshness::NotFound
        );
    }

    #[test]
    fn classify_test_output_passing_on_success() {
        assert_eq!(
            classify_test_output(
                true,
                "running 1 test\ntest foo ... ok\n\ntest result: ok. 1 passed; 0 failed"
            ),
            TestFreshness::Passing
        );
    }

    #[test]
    fn classify_test_output_failing_extracts_panic_message() {
        let stdout = "running 1 test\ntest foo ... FAILED\n\nthread 'foo' panicked at src/lib.rs:10:5:\nassertion failed: `(left == right)`\n\ntest result: FAILED. 0 passed; 1 failed";
        match classify_test_output(false, stdout) {
            TestFreshness::Failing { message } => {
                assert!(message.contains("panicked at"));
            }
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    /// find_ignored_test end-to-end against a real fixture directory tree
    /// (I/O path), proving the directory walk + crate-name resolution work
    /// together, not just the pure parsing helpers above.
    #[test]
    fn find_ignored_test_locates_fixture_across_directory_tree() {
        let dir = std::env::temp_dir().join(format!(
            "overwatch-test-freshness-fixture-{}",
            std::process::id()
        ));
        let src_dir = dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"fixture-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut f = std::fs::File::create(src_dir.join("lib.rs")).unwrap();
        writeln!(
            f,
            "#[test]\n#[ignore = \"CA-fixture-001: repros a thing\"]\nfn repro_thing() {{}}"
        )
        .unwrap();

        let found = find_ignored_test("CA-fixture-001", &dir);
        assert_eq!(
            found,
            IgnoredTestLookup::Found {
                crate_name: "fixture-crate".to_string(),
                test_path: "src/lib.rs".to_string(),
                fn_name: "repro_thing".to_string(),
            }
        );

        // A COMPLETE walk with no match is a legitimate `NotFound`, NOT
        // `ScanIncomplete` — this is the "genuinely no test" case the fix must
        // keep distinct from an unreadable subtree.
        let not_found = find_ignored_test("CA-fixture-999", &dir);
        assert_eq!(not_found, IgnoredTestLookup::NotFound);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rust_source_files_ok_and_empty_when_root_absent() {
        // An absent root is a legitimate "no sources" (NotFound), NOT an error:
        // the reverse-lookup over a project with no such tree yields NotFound,
        // not ScanIncomplete.
        let missing = std::env::temp_dir().join(format!(
            "overwatch-test-freshness-absent-{}-{}",
            std::process::id(),
            "nope"
        ));
        let got = rust_source_files(&missing).expect("absent root must be Ok(empty), not Err");
        assert!(got.is_empty(), "absent root must yield no files: {got:?}");
    }

    /// 50ad2c1e: a directory that EXISTS but is unreadable must fail closed
    /// (`Err`), not silently drop the subtree — otherwise the reverse-lookup
    /// reports "no regression test" for a finding whose test lives in the
    /// unreadable subtree (a false deprioritize signal). Twin of specguard
    /// `decision::list_files_fails_closed_on_unreadable_dir` (rounds #7/#11).
    /// Unix-only (chmod-based unreadability), skipped when running as root
    /// (root bypasses the permission bit, so the walk would succeed).
    #[cfg(unix)]
    #[test]
    fn rust_source_files_fails_closed_on_unreadable_subtree() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "overwatch-test-freshness-unreadable-{}",
            std::process::id()
        ));
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("hidden.rs"), "// would-be test file").unwrap();

        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        perms.set_mode(0o000); // unreadable/unsearchable
        std::fs::set_permissions(&locked, perms).unwrap();

        // Running as root defeats the 0o000 bit; skip rather than false-pass.
        let readable_as_root = std::fs::read_dir(&locked).is_ok();

        let got = rust_source_files(&root);

        // Restore perms so cleanup can remove the tree (before asserting).
        let mut restore = std::fs::metadata(&locked).unwrap().permissions();
        restore.set_mode(0o700);
        let _ = std::fs::set_permissions(&locked, restore);
        std::fs::remove_dir_all(&root).ok();

        if readable_as_root {
            eprintln!("skipping unreadable-subtree assertion: running as root");
            return;
        }
        assert!(
            got.is_err(),
            "an unreadable subtree must fail closed (Err), got {got:?}"
        );
    }

    /// End-to-end: `find_ignored_test` must surface `ScanIncomplete` (not
    /// `NotFound`) when the walk can't finish. This is the fail-open the round
    /// closes — `NotFound` renders as "該当テストなし" (deprioritize), so an
    /// incomplete search must be a *distinct* state.
    #[cfg(unix)]
    #[test]
    fn find_ignored_test_scan_incomplete_on_unreadable_subtree() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "overwatch-test-freshness-incomplete-{}",
            std::process::id()
        ));
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();

        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&locked, perms).unwrap();
        let readable_as_root = std::fs::read_dir(&locked).is_ok();

        let got = find_ignored_test("CA-whatever-001", &root);

        let mut restore = std::fs::metadata(&locked).unwrap().permissions();
        restore.set_mode(0o700);
        let _ = std::fs::set_permissions(&locked, restore);
        std::fs::remove_dir_all(&root).ok();

        if readable_as_root {
            eprintln!("skipping ScanIncomplete assertion: running as root");
            return;
        }
        assert!(
            matches!(got, IgnoredTestLookup::ScanIncomplete { .. }),
            "unreadable subtree must yield ScanIncomplete, not {got:?}"
        );
    }

    /// Fail-soft contract: a nonexistent crate name must yield
    /// `ExecutionError`, never a panic, when `cargo` can't resolve `-p`.
    #[test]
    fn run_ignored_test_execution_error_on_unknown_crate() {
        match run_ignored_test("this-crate-does-not-exist-xyz", "whatever") {
            TestFreshness::ExecutionError | TestFreshness::NotFound => {}
            other => panic!("expected ExecutionError or NotFound, got {other:?}"),
        }
    }

    /// Permanently-failing fixture, `#[ignore]`d under the finding-id
    /// convention, that exists ONLY so `run_ignored_test_reports_failing_*`
    /// below has a real test to invoke via `cargo test -- --ignored`. Do not
    /// "fix" this assertion — it is test infrastructure, not a real finding.
    #[test]
    #[ignore = "CA-test-freshness-fixture: intentionally-failing fixture for run_ignored_test's Failing{} path"]
    fn intentionally_failing_fixture_for_freshness_check() {
        assert_eq!(
            1, 2,
            "intentionally false — fixture for test_freshness::run_ignored_test"
        );
    }

    /// End-to-end proof that `run_ignored_test` returns `Failing{message}`
    /// when the underlying test genuinely fails (not just a classification
    /// unit test on captured stdout — this actually spawns `cargo test`).
    #[test]
    fn run_ignored_test_reports_failing_for_a_test_that_actually_fails() {
        match run_ignored_test(
            "overwatch",
            "intentionally_failing_fixture_for_freshness_check",
        ) {
            TestFreshness::Failing { message } => assert!(!message.is_empty()),
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    /// A hung child must be killed and reaped within the timeout, never
    /// blocking the caller past it. Mirrors
    /// `ctxrot::hooks::guard::run_with_timeout_kills_and_returns_none_on_timeout`.
    #[test]
    fn run_with_timeout_kills_and_returns_none_on_timeout() {
        let child = Command::new("sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh -c sleep");
        let start = std::time::Instant::now();
        let result = run_with_timeout(child, Duration::from_millis(200));
        assert!(result.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "run_with_timeout must not block past the timeout"
        );
    }

    /// A child that finishes well within the timeout returns its stdout and
    /// success status, exercising the fast-path classification is unaffected
    /// by the switch from `Command::output()` to `spawn()`+`wait_timeout()`.
    #[test]
    fn run_with_timeout_returns_stdout_on_fast_success() {
        let child = Command::new("sh")
            .args(["-c", "echo hello"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh -c echo");
        let (success, stdout) = run_with_timeout(child, Duration::from_secs(5))
            .expect("fast command must not time out");
        assert!(success);
        assert_eq!(String::from_utf8_lossy(&stdout).trim(), "hello");
    }
}
