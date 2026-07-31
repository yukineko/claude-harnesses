// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests for the `taintguard tally` subcommand — the CLI face of
//! `observe::tally`, which reports the observe-only ledger totals for the
//! project in the PROCESS CURRENT DIRECTORY.
//!
//! The property under test is the one `observe::tally` already guarantees at
//! the library level (`Err` is not `Ok((0, 0))`) but which a CLI can trivially
//! throw away: a `main` that does `let (ok, bad) = tally(&cwd).unwrap_or((0,
//! 0));` compiles, exits 0, and prints `suppressed: 0` — turning "I could not
//! read the tally" into "nothing was suppressed". That is the
//! cannot-determine→clean collapse this crate exists to prevent (CLAUDE.md §3)
//! applied to its own statistic, and only a test that drives the real binary
//! and reads its EXIT CODE can see it.
//!
//! Conventions follow `tests/provenance_gate.rs`: the process environment is
//! fully cleared and only the variables the scenario needs are set (so a real
//! `~/.taintguard` or a stray `TAINTGUARD_STATE_DIR` cannot contaminate the
//! result), and every test gets its own project root + its own
//! `TAINTGUARD_STATE_DIR` so parallel `cargo test` never collides.
//!
//! Unlike `mark`/`gate`/`clear`, `tally` is an operator-facing report, not a
//! hook: it takes its project from the process cwd (`Command::current_dir`),
//! not from a stdin payload, and it is allowed — required — to exit non-zero.

use std::path::{Path, PathBuf};
use std::process::Command;

use taintguard::observe;

/// The state-dir env var (the same name `state::state_base` reads). Spelled
/// out here rather than imported because the crate keeps its constant private.
const STATE_DIR_ENV: &str = "TAINTGUARD_STATE_DIR";

/// Guards the *test process's* `TAINTGUARD_STATE_DIR` while we ask the library
/// where the ledger lives. `std::env::set_var` is process-global and `cargo
/// test` runs these tests in parallel threads, so without this two fixtures
/// would compute each other's ledger path. The child processes are unaffected
/// (their env is set per-`Command`); this lock only covers the in-process
/// `observe::ledger_path` call below.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh project root + isolated state dir, so parallel runs never collide.
struct Fixture {
    _project_root: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    /// Canonicalized so the path the test derives matches the one the child
    /// derives from its (already-canonical) process cwd.
    cwd: PathBuf,
    state_dir: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let project_root = tempfile::Builder::new()
        .prefix(&format!("taintguard-tally-{name}-project-"))
        .tempdir()
        .expect("tempdir");
    let state_dir = tempfile::Builder::new()
        .prefix(&format!("taintguard-tally-{name}-state-"))
        .tempdir()
        .expect("tempdir");
    let cwd = project_root
        .path()
        .canonicalize()
        .expect("canonicalize project root");
    let state_dir_path = state_dir
        .path()
        .canonicalize()
        .expect("canonicalize state dir");
    Fixture {
        _project_root: project_root,
        _state_dir: state_dir,
        cwd,
        state_dir: state_dir_path,
    }
}

impl Fixture {
    /// Where this fixture's observe-only ledger lives, asked of the library
    /// (`observe::ledger_path`) rather than hardcoded, so the layout stays
    /// owned by `state::project_state_dir`.
    fn ledger(&self) -> PathBuf {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(STATE_DIR_ENV, &self.state_dir);
        let path = observe::ledger_path(&self.cwd);
        drop(guard);
        path
    }

    /// Write `lines` verbatim as the ledger body (creating the project-scoped
    /// state dir first).
    fn plant_ledger(&self, lines: &[String]) -> PathBuf {
        let path = self.ledger();
        std::fs::create_dir_all(path.parent().expect("ledger has a parent dir"))
            .expect("mk ledger dir");
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(&path, body).expect("plant ledger");
        path
    }

    /// Run `taintguard tally [args]` with the process cwd pointed at this
    /// fixture's project root and a fully-controlled environment.
    /// Returns (exit_code, stdout, stderr).
    fn tally(&self, args: &[&str]) -> (i32, String, String) {
        let bin = env!("CARGO_BIN_EXE_taintguard");
        let mut cmd = Command::new(bin);
        cmd.arg("tally");
        for a in args {
            cmd.arg(a);
        }
        cmd.env_clear();
        cmd.env(STATE_DIR_ENV, &self.state_dir);
        cmd.current_dir(&self.cwd);
        let out = cmd.output().expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// One valid ledger line, built through `observe::Record` so the schema is
/// exactly what `observe::tally` parses (no hand-rolled JSON to drift).
fn valid_line(tool: &str, session: &str) -> String {
    let record = observe::Record::now(tool, &["web".to_string()], "tainted", session);
    serde_json::to_string(&record).expect("serialize record")
}

/// The number that follows `label` in `haystack` (whitespace-tolerant), or
/// `None` if the label is absent or is not followed by digits. Used instead of
/// substring matching on `"suppressed: 2"` so a flattened count (`3`) or a
/// dropped one (`0`) is read as a *wrong number* rather than a missing label.
fn number_after(haystack: &str, label: &str) -> Option<u64> {
    let idx = haystack.find(label)?;
    let rest = &haystack[idx + label.len()..];
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// True when `s` contains no ASCII digit at all — the strongest available form
/// of "printed no count".
fn has_no_digits(s: &str) -> bool {
    !s.chars().any(|c| c.is_ascii_digit())
}

/// Make `path` unreadable and report whether the fault actually took effect.
///
/// `chmod 000` is a no-op against a privileged (root / `CAP_DAC_OVERRIDE`)
/// process, and a fault-injection test whose fault did not inject proves
/// nothing. So this probes the result by trying to read the file back: `false`
/// means the file is STILL readable, i.e. the scenario could not be created in
/// this environment.
#[cfg(unix)]
fn make_unreadable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000 the ledger");
    std::fs::read_to_string(path).is_err()
}

#[cfg(unix)]
fn make_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
        .expect("restore ledger permissions for tempdir cleanup");
}

// ---------------------------------------------------------------------------
// (a) unreadable ledger — non-zero exit, and NO count printed
// ---------------------------------------------------------------------------

/// KILLS: `let (ok, bad) = observe::tally(&cwd).unwrap_or((0, 0));` — and every
/// variant of it (`.unwrap_or_default()`, `if let Ok(..) = tally { .. }` with a
/// zero fallback, `eprintln!` the error then print the totals anyway). Each of
/// those exits 0 and prints `suppressed: 0`, reporting "I could not read the
/// tally" as "nothing was suppressed". Also kills an implementation that
/// reports the error on stderr but still exits 0 (the exit-code assertion), and
/// one that exits non-zero but prints a zero tally first (the no-digits
/// assertion).
#[cfg(unix)]
#[test]
fn unreadable_ledger_exits_non_zero_and_prints_no_count() {
    let f = fixture("unreadable");
    let ledger = f.plant_ledger(&[valid_line("Bash", "s1")]);

    if !make_unreadable(&ledger) {
        // CANNOT-DETERMINE RESOLVES TO THE RESTRICTED SIDE (CLAUDE.md §3). The
        // fault could not be injected (privileged process — likely root), so
        // this test observed NOTHING about the unreadable path. This used to
        // `return`, which cargo reports as a PASS and whose stderr note cargo
        // HIDES on green — rendering "I could not verify" indistinguishable from
        // "I verified it", which is the exact collapse this file exists to pin.
        // Failing is the honest answer.
        make_readable(&ledger);
        panic!(
            "CANNOT VERIFY unreadable_ledger_exits_non_zero_and_prints_no_count: chmod 000 on {} \
             did NOT make it unreadable (privileged process — likely root). This test asserted \
             NOTHING, so it FAILS rather than passing vacuously. Re-run unprivileged.",
            ledger.display()
        );
    }

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (code, stdout, stderr) = f.tally(&[]);
        assert_ne!(
            code, 0,
            "an unreadable ledger must exit NON-ZERO; got exit {code} with stdout {stdout:?} / \
             stderr {stderr:?}"
        );
        assert!(
            !stdout.contains("suppressed"),
            "a failure must not print a tally at all; stdout was {stdout:?}"
        );
        assert!(
            !stdout.contains("corrupt"),
            "a failure must not print a corrupt count either; stdout was {stdout:?}"
        );
        assert!(
            has_no_digits(&stdout),
            "a failure must print NO count — not even zero; stdout was {stdout:?}"
        );
        assert!(
            stderr.contains("could not read"),
            "the propagated observe::tally error must reach stderr; stderr was {stderr:?}"
        );
        assert!(
            stderr.contains("NOT a tally of zero"),
            "stderr must say the failure is NOT a tally of zero; stderr was {stderr:?}"
        );
    }));

    // Restore unconditionally so the TempDir can be cleaned up on drop.
    make_readable(&ledger);

    // Control: once readable again the same ledger tallies its one line, so the
    // failure above came from the permission fault and not from the ledger
    // being absent, empty or malformed.
    let (code, stdout, stderr) = f.tally(&[]);
    assert_eq!(
        code, 0,
        "control: a readable ledger must exit 0; stderr was {stderr:?}"
    );
    assert_eq!(
        number_after(&stdout, "suppressed:"),
        Some(1),
        "control: the restored ledger holds exactly one valid record; stdout was {stdout:?}"
    );

    outcome.unwrap();
}

// ---------------------------------------------------------------------------
// (b) absent ledger — a real zero, distinguishable from (a)
// ---------------------------------------------------------------------------

/// KILLS: an implementation that reports a read failure as a zero tally — i.e.
/// one for which the ABSENT case and the UNREADABLE case produce the same
/// (exit code, stdout). The `differs` control at the end of this test is what
/// makes that concrete: if `tally` printed `suppressed: 0` / exit 0 on a read
/// error, the two invocations would be byte-identical and this test fails.
/// Also kills the opposite over-correction: an implementation that treats an
/// absent ledger as an error (nothing suppressed yet is a real zero, not a
/// fault), and one that prints the failure disclaimer on the success path.
#[test]
fn absent_ledger_prints_zero_exits_zero_and_says_nothing_observed_yet() {
    let f = fixture("absent");
    assert!(
        !f.ledger().exists(),
        "precondition: no ledger has been planted"
    );

    let (code, stdout, stderr) = f.tally(&[]);
    assert_eq!(
        code, 0,
        "an absent ledger is a real zero, not a fault; stderr was {stderr:?}"
    );
    assert_eq!(
        number_after(&stdout, "suppressed:"),
        Some(0),
        "absent ledger must report suppressed 0; stdout was {stdout:?}"
    );
    assert_eq!(
        number_after(&stdout, "corrupt:"),
        Some(0),
        "absent ledger must report corrupt 0; stdout was {stdout:?}"
    );
    assert!(
        stdout.contains("nothing observed yet"),
        "a 0/0 tally must say `nothing observed yet` so an empty measurement is not mistaken \
         for a missing feature; stdout was {stdout:?}"
    );
    assert!(
        !stdout.contains("could not read") && !stdout.contains("NOT a tally of zero"),
        "the success path must not carry the failure disclaimer; stdout was {stdout:?}"
    );

    // Distinguishability control (the whole point of (b) existing next to (a)):
    // plant an UNREADABLE ledger in the same fixture and require the output to
    // differ from the absent-ledger output above.
    #[cfg(unix)]
    {
        let ledger = f.plant_ledger(&[valid_line("Bash", "s1")]);
        if make_unreadable(&ledger) {
            let (bad_code, bad_stdout, _bad_stderr) = f.tally(&[]);
            make_readable(&ledger);
            assert_ne!(
                (code, stdout.as_str()),
                (bad_code, bad_stdout.as_str()),
                "an unreadable ledger must NOT produce the same report as an absent one — \
                 `could not read the tally` and `nothing was suppressed` have to be \
                 distinguishable downstream"
            );
        } else {
            make_readable(&ledger);
            eprintln!(
                "NOTE absent_ledger_prints_zero_exits_zero_and_says_nothing_observed_yet: the \
                 distinguishability control was SKIPPED (chmod 000 not honoured — privileged \
                 process). The absent-ledger assertions above still ran."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (c) corrupt lines are counted separately, not folded in and not dropped
// ---------------------------------------------------------------------------

/// KILLS three flattening implementations at once: one that adds the
/// unparseable line into the ok count (prints `suppressed: 3`) — caught by
/// `suppressed == 2`; one that silently ignores unparseable lines (prints
/// `corrupt: 0`, or no corrupt line at all) — caught by `corrupt == 1` being
/// present and non-zero; and one that prints a single combined total, dropping
/// the split entirely — caught by requiring BOTH labels to yield a number.
/// Also kills an implementation that emits `nothing observed yet` whenever one
/// of the two counts is zero rather than only when both are.
#[test]
fn corrupt_lines_are_counted_separately_from_ok_lines() {
    let f = fixture("mixed");
    f.plant_ledger(&[
        valid_line("Bash", "s1"),
        valid_line("Write", "s2"),
        "{ not json".to_string(),
    ]);

    let (code, stdout, stderr) = f.tally(&[]);
    assert_eq!(code, 0, "a readable ledger exits 0; stderr was {stderr:?}");

    let ok = number_after(&stdout, "suppressed:");
    let bad = number_after(&stdout, "corrupt:");
    assert_eq!(
        ok,
        Some(2),
        "2 valid records must be reported as 2 — not 3 (corrupt line folded in) and not 0; \
         stdout was {stdout:?}"
    );
    assert_eq!(
        bad,
        Some(1),
        "the 1 unparseable line must be reported as corruption, not silently dropped; \
         stdout was {stdout:?}"
    );
    assert_ne!(
        ok, bad,
        "the two counts are distinct numbers here (2 and 1); reporting the same value for \
         both would mean one label is echoing the other; stdout was {stdout:?}"
    );
    assert!(
        !stdout.contains("nothing observed yet"),
        "`nothing observed yet` is only for ok==0 && corrupt==0; stdout was {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// (d) --json success shape
// ---------------------------------------------------------------------------

/// KILLS: an implementation that stringifies the counts (`"suppressed": "2"`)
/// — a JSON consumer doing `jq '.suppressed > 0'` silently misreads a string;
/// one that leaks extra keys (an `ok`/`error`/`status` field the contract does
/// not have, so a consumer cannot rely on the shape); one that omits the
/// absolute ledger path; and one that prints the human text alongside the JSON
/// (which would fail to parse as a single object).
#[test]
fn json_success_has_exactly_the_three_contract_keys_with_numeric_counts() {
    let f = fixture("json-ok");
    f.plant_ledger(&[
        valid_line("Bash", "s1"),
        valid_line("Write", "s1"),
        "{ not json".to_string(),
    ]);
    let ledger = f.ledger();

    let (code, stdout, stderr) = f.tally(&["--json"]);
    assert_eq!(code, 0, "--json success exits 0; stderr was {stderr:?}");

    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("--json must print a single parseable JSON object on stdout; got {stdout:?}: {e}")
    });
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("--json output must be a JSON object; got {value}"));

    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["corrupt", "ledger", "suppressed"],
        "--json must carry EXACTLY the three contract keys; got {value}"
    );

    assert!(
        obj["suppressed"].is_u64(),
        "`suppressed` must be a JSON number, not a string; got {}",
        obj["suppressed"]
    );
    assert!(
        obj["corrupt"].is_u64(),
        "`corrupt` must be a JSON number, not a string; got {}",
        obj["corrupt"]
    );
    assert_eq!(obj["suppressed"].as_u64(), Some(2), "got {value}");
    assert_eq!(obj["corrupt"].as_u64(), Some(1), "got {value}");
    assert_eq!(
        obj["ledger"].as_str(),
        Some(ledger.to_string_lossy().as_ref()),
        "`ledger` must be the absolute ledger path; got {value}"
    );
    assert!(
        Path::new(obj["ledger"].as_str().unwrap_or("")).is_absolute(),
        "`ledger` must be ABSOLUTE so the operator can find the file; got {value}"
    );
}

// ---------------------------------------------------------------------------
// (e) --json failure shape
// ---------------------------------------------------------------------------

/// KILLS: an implementation whose `--json` failure path emits
/// `{"suppressed": 0, "corrupt": 0, ...}` — a machine consumer reading
/// `.suppressed` would record a clean measurement for a ledger it never read.
/// Also kills one that exits 0 on the JSON failure path, and one that prints
/// a bare non-JSON message when `--json` was requested (a consumer piping
/// stderr into a parser gets nothing it can branch on).
#[cfg(unix)]
#[test]
fn json_failure_reports_an_error_object_with_no_suppressed_key() {
    let f = fixture("json-err");
    let ledger = f.plant_ledger(&[valid_line("Bash", "s1")]);

    if !make_unreadable(&ledger) {
        // Cannot-determine resolves to the restricted side (CLAUDE.md §3) — see
        // the longer note in `unreadable_ledger_exits_non_zero_and_prints_no_count`.
        make_readable(&ledger);
        panic!(
            "CANNOT VERIFY json_failure_reports_an_error_object_with_no_suppressed_key: chmod 000 \
             on {} did NOT make it unreadable (privileged process — likely root). This test \
             asserted NOTHING, so it FAILS rather than passing vacuously. Re-run unprivileged.",
            ledger.display()
        );
    }

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (code, stdout, stderr) = f.tally(&["--json"]);
        assert_ne!(
            code, 0,
            "--json on an unreadable ledger must exit NON-ZERO; got stdout {stdout:?} / \
             stderr {stderr:?}"
        );
        assert!(
            has_no_digits(&stdout),
            "the --json failure path must print no tally on stdout; got {stdout:?}"
        );

        let value: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
            panic!("--json failure must print a JSON object on stderr; got {stderr:?}: {e}")
        });
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("--json failure output must be a JSON object; got {value}"));
        assert!(
            obj.contains_key("error"),
            "--json failure must carry an `error` key; got {value}"
        );
        assert!(
            obj["error"].as_str().is_some_and(|s| !s.trim().is_empty()),
            "`error` must be a non-empty message, not null/empty; got {value}"
        );
        assert!(
            !obj.contains_key("suppressed"),
            "a failure must NOT carry a `suppressed` key — `could not read` must be \
             unrepresentable as a count; got {value}"
        );
        assert!(
            !obj.contains_key("corrupt"),
            "a failure must NOT carry a `corrupt` count either; got {value}"
        );
    }));

    make_readable(&ledger);
    outcome.unwrap();
}
