//! Single-instance harness: setup -> candidate-patch generation -> gold-test
//! results -> resolution scoring.
//!
//! The harness wires four stages for one [`Instance`]:
//!   1. **setup** — read `repo` / `base_commit` / `problem_statement` /
//!      `test_patch` off the instance,
//!   2. **generate** — ask an injected [`PatchGenerator`] for a candidate patch
//!      (the LLM / harness-under-test lives behind this seam),
//!   3. **run tests** — obtain a per-test pass/fail map from an injected
//!      [`TestResultSource`], and
//!   4. **score** — delegate to [`crate::scorer::is_resolved`] (never
//!      reimplemented here) to produce a [`Verdict`].
//!
//! Design invariant — two disjoint paths:
//!   * [`run_instance`] is **pure / mockable**: given a generator and a
//!     test-result source it opens no network and shells out to nothing. Every
//!     test drives this path with canned mocks, so `cargo test -p benchkit` is
//!     hermetic.
//!   * [`run_instance_real`] is the **explicitly-gated** real path: it builds a
//!     [`RealExecSource`] that shells out (git clone / git apply / pytest,
//!     matching the `download` house pattern) and is reached *only* via
//!     `benchkit run-instance --real`. No test constructs it.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::model::Instance;
use crate::scorer;

/// Produces a candidate patch for an [`Instance`] — the injectable seam behind
/// which the LLM / harness-under-test lives.
///
/// Implementations must be side-effect-free from the harness's point of view:
/// the harness treats the returned string as an opaque unified diff and hands
/// it to the [`TestResultSource`]. Tests supply a `MockPatchGenerator` that
/// returns a canned string; the real path supplies whatever generator wraps the
/// model under evaluation.
pub trait PatchGenerator {
    /// Generate a candidate patch (unified diff) for `instance`.
    fn generate(&self, instance: &Instance) -> Result<String>;
}

/// Yields the per-test pass/fail map for a candidate patch on an instance.
///
/// This is the second injectable seam. In tests it returns a **canned** map so
/// no tools run; on the real path [`RealExecSource`] shells out to apply the
/// patch and run pytest for the instance's `FAIL_TO_PASS` / `PASS_TO_PASS`
/// sets. Modelling it as a trait is what keeps [`run_instance`] pure.
pub trait TestResultSource {
    /// Return a map of test-id -> `true` (pass) / `false` (fail) for
    /// `candidate_patch` applied to `instance`.
    fn results(&self, instance: &Instance, candidate_patch: &str)
        -> Result<BTreeMap<String, bool>>;
}

/// The graded outcome of running one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The instance this verdict is for.
    pub instance_id: String,
    /// Whether the candidate **resolves** the instance (per [`scorer::is_resolved`]).
    pub resolved: bool,
    /// The raw per-test result map the verdict was scored from (kept for
    /// reporting / debugging).
    pub results: BTreeMap<String, bool>,
}

/// Run one instance through the pure harness pipeline.
///
/// `generator` produces the candidate patch; `source` turns that patch into a
/// per-test result map; the map is scored by [`scorer::is_resolved`] against the
/// instance's two named test sets. **Pure / mockable**: this function opens no
/// network and shells out to nothing — all external effects live behind the two
/// injected traits, so canned mocks make it fully hermetic.
pub fn run_instance<G, S>(instance: &Instance, generator: &G, source: &S) -> Result<Verdict>
where
    G: PatchGenerator,
    S: TestResultSource,
{
    // 1. generate the candidate (the injected LLM / harness seam).
    let candidate_patch = generator
        .generate(instance)
        .with_context(|| format!("generating candidate patch for {}", instance.instance_id))?;

    // 2. obtain per-test results for that candidate (injected source).
    let results = source
        .results(instance, &candidate_patch)
        .with_context(|| format!("collecting test results for {}", instance.instance_id))?;

    // 3. score — reuse the canonical resolver, never reimplement it here.
    let resolved = scorer::is_resolved(&results, &instance.fail_to_pass, &instance.pass_to_pass);

    Ok(Verdict {
        instance_id: instance.instance_id.clone(),
        resolved,
        results,
    })
}

/// Run one instance through the **real** exec path (git clone + git apply +
/// pytest). This is the explicitly-gated entrypoint reached only via
/// `benchkit run-instance --real`; no test constructs the [`RealExecSource`] it
/// wires, so `cargo test` never touches git / pytest / the network.
///
/// It is deliberately a thin composition: the real path is just
/// [`run_instance`] with a [`RealExecSource`] plugged into the test-result seam,
/// so scoring is shared with the mock path.
pub fn run_instance_real<G>(instance: &Instance, generator: &G) -> Result<Verdict>
where
    G: PatchGenerator,
{
    let source = RealExecSource::new();
    run_instance(instance, generator, &source)
}

/// The real test-result source: shells out to git / pytest to actually execute
/// the candidate against the instance's gold tests.
///
/// This is the *only* [`TestResultSource`] that performs external effects, and
/// it is constructed *only* by [`run_instance_real`] (the `--real` path). It
/// follows the `download` house pattern of shelling out via
/// [`std::process::Command`] rather than linking heavyweight libraries.
///
/// The stage boundaries (clone → checkout → apply test_patch → apply candidate
/// → run pytest → parse) are laid out and it shells out; pytest's terminal
/// output is parsed by the pure [`parse_pytest_output`] helper into the same
/// per-test boolean map the mock path returns, so scoring is shared.
#[derive(Debug, Default)]
pub struct RealExecSource {
    _private: (),
}

impl RealExecSource {
    /// Construct the real source. Kept as a distinct constructor so the gate is
    /// obvious in a grep: nothing but the `--real` path calls this.
    pub fn new() -> Self {
        RealExecSource { _private: () }
    }
}

impl TestResultSource for RealExecSource {
    fn results(
        &self,
        instance: &Instance,
        candidate_patch: &str,
    ) -> Result<BTreeMap<String, bool>> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        // Structured real path (gated — never reached from tests). Each step
        // shells out via std::process::Command per the house pattern.
        let workdir = std::env::temp_dir().join(format!("benchkit-real-{}", instance.instance_id));

        // 1. clone the repo and check out the pinned base commit.
        let repo_url = format!("https://github.com/{}.git", instance.repo);
        let clone = Command::new("git")
            .args(["clone", "--quiet", &repo_url])
            .arg(&workdir)
            .status()
            .context("spawning git clone for the real exec path (is git installed?)")?;
        if !clone.success() {
            anyhow::bail!(
                "git clone of {repo_url} failed for {} (exit {:?})",
                instance.instance_id,
                clone.code()
            );
        }

        let checkout = Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(["checkout", "--quiet", &instance.base_commit])
            .status()
            .context("spawning git checkout of base_commit")?;
        if !checkout.success() {
            anyhow::bail!(
                "git checkout of base_commit {} failed for {} (exit {:?})",
                instance.base_commit,
                instance.instance_id,
                checkout.code()
            );
        }

        // 2. apply the gold test_patch, then the candidate patch, via
        //    `git apply` (each diff is fed on stdin). Empty diffs are skipped.
        let diffs: [(&str, &str); 2] = [
            ("test_patch", instance.test_patch.as_str()),
            ("candidate", candidate_patch),
        ];
        for (label, diff) in diffs {
            if diff.trim().is_empty() {
                continue;
            }
            let mut child = Command::new("git")
                .arg("-C")
                .arg(&workdir)
                .arg("apply")
                .stdin(Stdio::piped())
                .spawn()
                .with_context(|| format!("spawning git apply for the {label} patch"))?;
            child
                .stdin
                .take()
                .context("git apply stdin unavailable")?
                .write_all(diff.as_bytes())
                .with_context(|| format!("writing the {label} diff to git apply"))?;
            let status = child
                .wait()
                .with_context(|| format!("waiting on git apply for the {label} patch"))?;
            if !status.success() {
                anyhow::bail!(
                    "git apply of the {label} patch failed for {} (exit {:?})",
                    instance.instance_id,
                    status.code()
                );
            }
        }

        // 3. run pytest over the union of the two named test sets, then parse
        //    its terminal output into the per-test boolean map. `-rA` prints a
        //    PASSED/FAILED/ERROR line per test in the short summary; `--tb=no
        //    -q` keeps the output compact. Missing tests stay fail-closed via
        //    the scorer (they simply never appear in the parsed map).
        let mut targets: Vec<&str> = Vec::new();
        targets.extend(instance.fail_to_pass.iter().map(String::as_str));
        targets.extend(instance.pass_to_pass.iter().map(String::as_str));

        let mut cmd = Command::new("python");
        cmd.arg("-m")
            .arg("pytest")
            .arg("-rA")
            .arg("--tb=no")
            .arg("-q")
            .current_dir(&workdir);
        for t in &targets {
            cmd.arg(t);
        }
        let output = cmd
            .output()
            .context("spawning pytest for the real exec path (is pytest installed?)")?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push('\n');
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        Ok(parse_pytest_output(&combined))
    }
}

/// Parse pytest terminal output into a per-test `id -> pass?` map.
///
/// Pure: no I/O, no network — a function of the captured output string only, so
/// it is unit-tested directly against fixture strings (no pytest runs in tests).
///
/// Recognizes the short-summary lines pytest prints with `-rA`:
///   * `PASSED <nodeid>`  → `true`
///   * `FAILED <nodeid>`  → `false`
///   * `ERROR <nodeid>`   → `false`  (collection / setup errors are failures)
///
/// A node id may carry a trailing ` - <reason>` on FAILED/ERROR lines; the
/// reason is stripped so the key matches the plain `FAIL_TO_PASS` id. A test
/// seen more than once resolves to `false` if *any* occurrence failed
/// (fail-closed). Lines that are not status markers (progress dots, the summary
/// counts line, collection-error banners, "no tests ran") are ignored — an
/// all-error or no-tests run therefore yields no `true` entries, so the scorer
/// treats every target as unresolved.
pub fn parse_pytest_output(output: &str) -> BTreeMap<String, bool> {
    let mut map: BTreeMap<String, bool> = BTreeMap::new();

    for raw in output.lines() {
        let line = raw.trim();
        let (passed, rest) = if let Some(r) = line.strip_prefix("PASSED ") {
            (true, r)
        } else if let Some(r) = line.strip_prefix("FAILED ") {
            (false, r)
        } else if let Some(r) = line.strip_prefix("ERROR ") {
            (false, r)
        } else {
            continue;
        };

        // The node id is the first whitespace-delimited token; FAILED/ERROR
        // lines append ` - <reason>` which we drop.
        let node = rest
            .split_whitespace()
            .next()
            .map(|n| n.split(" - ").next().unwrap_or(n))
            .unwrap_or("")
            .trim();
        if node.is_empty() {
            continue;
        }

        map.entry(node.to_string())
            .and_modify(|v| *v = *v && passed)
            .or_insert(passed);
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Canned patch generator — the test-double for the LLM seam. Records that
    /// it was asked, and returns a fixed patch string so we can assert the
    /// harness plumbs that exact output into the result source.
    struct MockPatchGenerator {
        patch: String,
    }

    impl PatchGenerator for MockPatchGenerator {
        fn generate(&self, _instance: &Instance) -> Result<String> {
            Ok(self.patch.clone())
        }
    }

    /// Canned result source. Asserts (by construction) that the harness handed
    /// it the generator's output, then returns a pre-baked pass/fail map — so
    /// the test never runs git or pytest.
    struct MockTestResultSource {
        expect_patch: String,
        canned: BTreeMap<String, bool>,
    }

    impl TestResultSource for MockTestResultSource {
        fn results(
            &self,
            _instance: &Instance,
            candidate_patch: &str,
        ) -> Result<BTreeMap<String, bool>> {
            // Prove the generator output is plumbed through unchanged.
            assert_eq!(
                candidate_patch, self.expect_patch,
                "run_instance must pass the generator's patch to the result source"
            );
            Ok(self.canned.clone())
        }
    }

    /// The canned-run fixture: an instance shape plus two result maps.
    #[derive(serde::Deserialize)]
    struct MockRun {
        instance_id: String,
        candidate_patch: String,
        fail_to_pass: Vec<String>,
        pass_to_pass: Vec<String>,
        resolved_results: BTreeMap<String, bool>,
        unresolved_results: BTreeMap<String, bool>,
    }

    fn load_mock_run() -> MockRun {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("mock_run.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).expect("mock_run.json should parse")
    }

    fn instance_from(mock: &MockRun) -> Instance {
        Instance {
            instance_id: mock.instance_id.clone(),
            repo: "example/repo".into(),
            base_commit: "0000000000000000000000000000000000000000".into(),
            patch: String::new(),
            test_patch: String::new(),
            problem_statement: "canned".into(),
            hints_text: String::new(),
            created_at: String::new(),
            version: String::new(),
            fail_to_pass: mock.fail_to_pass.clone(),
            pass_to_pass: mock.pass_to_pass.clone(),
            environment_setup_commit: String::new(),
        }
    }

    #[test]
    fn harness_resolves_when_all_gold_tests_pass() {
        let mock = load_mock_run();
        let instance = instance_from(&mock);
        let generator = MockPatchGenerator {
            patch: mock.candidate_patch.clone(),
        };
        let source = MockTestResultSource {
            expect_patch: mock.candidate_patch.clone(),
            canned: mock.resolved_results.clone(),
        };

        let verdict = run_instance(&instance, &generator, &source).expect("mock run is infallible");

        assert_eq!(verdict.instance_id, mock.instance_id);
        assert!(
            verdict.resolved,
            "all FAIL_TO_PASS + PASS_TO_PASS pass -> resolved"
        );
        assert_eq!(verdict.results, mock.resolved_results);
    }

    #[test]
    fn harness_unresolved_when_a_gold_test_fails() {
        let mock = load_mock_run();
        let instance = instance_from(&mock);
        let generator = MockPatchGenerator {
            patch: mock.candidate_patch.clone(),
        };
        let source = MockTestResultSource {
            expect_patch: mock.candidate_patch.clone(),
            canned: mock.unresolved_results.clone(),
        };

        let verdict = run_instance(&instance, &generator, &source).expect("mock run is infallible");

        assert_eq!(verdict.instance_id, mock.instance_id);
        assert!(
            !verdict.resolved,
            "a failing gold test -> not resolved (fail-closed via scorer)"
        );
        assert_eq!(verdict.results, mock.unresolved_results);
    }

    #[test]
    fn generator_output_is_plumbed_into_the_scorer() {
        // A generator whose patch differs from what the source expects would
        // trip the assertion inside MockTestResultSource — here they match, so
        // this test confirms the plumbing end to end for the resolved path.
        let mock = load_mock_run();
        let instance = instance_from(&mock);
        let generator = MockPatchGenerator {
            patch: mock.candidate_patch.clone(),
        };
        let source = MockTestResultSource {
            expect_patch: mock.candidate_patch.clone(),
            canned: mock.resolved_results.clone(),
        };

        let verdict = run_instance(&instance, &generator, &source).unwrap();
        // The verdict's result map is exactly what the (patch-driven) source
        // returned, proving generator -> source -> scorer wiring.
        assert_eq!(verdict.results, mock.resolved_results);
        assert!(verdict.resolved);
    }

    // ---- parse_pytest_output: the pure pytest-output parser ----------------
    //
    // These drive the parser against captured sample pytest output strings so
    // no pytest runs in the test suite — the real exec path stays gated.

    fn parsed(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect::<BTreeMap<String, bool>>()
    }

    #[test]
    fn parse_all_pass() {
        // `pytest -rA -q` short summary when every selected test passes.
        let out = "\
....                                                                     [100%]
==================== PASSES ====================
PASSED tests/test_core.py::test_alpha
PASSED tests/test_core.py::test_beta
PASSED tests/test_core.py::test_gamma
PASSED tests/test_core.py::test_delta
4 passed in 0.12s
";
        let got = parse_pytest_output(out);
        assert_eq!(
            got,
            parsed(&[
                ("tests/test_core.py::test_alpha", true),
                ("tests/test_core.py::test_beta", true),
                ("tests/test_core.py::test_gamma", true),
                ("tests/test_core.py::test_delta", true),
            ])
        );
    }

    #[test]
    fn parse_some_fail() {
        // Mixed run: FAILED lines carry a trailing ` - <reason>` that must be
        // stripped so the key equals the plain node id.
        let out = "\
.F.F                                                                     [100%]
==================== short test summary info ====================
PASSED tests/test_math.py::test_add
FAILED tests/test_math.py::test_sub - AssertionError: 1 != 2
PASSED tests/test_math.py::test_mul
FAILED tests/test_math.py::test_div - ZeroDivisionError: division by zero
2 failed, 2 passed in 0.20s
";
        let got = parse_pytest_output(out);
        assert_eq!(
            got,
            parsed(&[
                ("tests/test_math.py::test_add", true),
                ("tests/test_math.py::test_sub", false),
                ("tests/test_math.py::test_mul", true),
                ("tests/test_math.py::test_div", false),
            ])
        );
    }

    #[test]
    fn parse_collection_error() {
        // A collection/import error emits ERROR lines (no PASSED). Every entry
        // is a failure, so the scorer credits nothing.
        let out = "\
==================== ERRORS ====================
ERROR tests/test_broken.py - ModuleNotFoundError: No module named 'widget'
!!!!!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!!!!!
1 error in 0.05s
";
        let got = parse_pytest_output(out);
        assert_eq!(got, parsed(&[("tests/test_broken.py", false)]));
        assert!(
            !got.values().any(|&v| v),
            "no test may be credited on a collection error"
        );
    }

    #[test]
    fn parse_no_tests_ran() {
        // Selector matched nothing: pytest prints "no tests ran" and no
        // PASSED/FAILED markers — the parsed map is empty, so the scorer treats
        // every target as unresolved (fail-closed).
        let out = "\
============================ no tests ran in 0.01s =============================
ERROR: not found: tests/test_core.py::test_absent
(no match in any of [<Module test_core.py>])
";
        let got = parse_pytest_output(out);
        assert!(
            got.is_empty(),
            "no status markers -> empty map, got {got:?}"
        );
    }

    #[test]
    fn parse_ignores_noise_and_dedupes_fail_closed() {
        // Progress dots, banners and summary counts are ignored. A node id seen
        // both PASSED and FAILED resolves to false (any failure wins).
        let out = "\
collected 3 items
tests/test_x.py ..F                                                      [100%]
PASSED tests/test_x.py::test_flaky
FAILED tests/test_x.py::test_flaky - AssertionError
PASSED tests/test_x.py::test_stable
1 failed, 2 passed in 0.03s
";
        let got = parse_pytest_output(out);
        assert_eq!(
            got,
            parsed(&[
                ("tests/test_x.py::test_flaky", false),
                ("tests/test_x.py::test_stable", true),
            ])
        );
    }

    #[test]
    fn parse_empty_output_is_empty_map() {
        assert!(parse_pytest_output("").is_empty());
        assert!(parse_pytest_output("\n\n   \n").is_empty());
    }
}
