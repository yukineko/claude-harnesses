#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for the harness-core README prose. Each test drives
//! the observable the prose names; tests that touch `HOME` hold `HOME_LOCK` and
//! point it at a fresh tempdir (never the real home).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with `HOME` = a fresh tempdir and `LESSONS_STORE_DIR` unset, then
/// restore the previous environment.
fn with_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    let old_les = std::env::var_os("LESSONS_STORE_DIR");
    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("LESSONS_STORE_DIR");
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(tmp.path())));
    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    if let Some(v) = old_les {
        std::env::set_var("LESSONS_STORE_DIR", v);
    }
    match out {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e),
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Re-exec this test binary running only `test_name` with `CHILD_MODE` set, and
/// return (exit code, stdout, stderr). The child branch of the test performs the
/// process-exiting call.
fn reexec(test_name: &str) -> (Option<i32>, String, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
        .env("PROSE_CLAIMS_CHILD", test_name)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn is_child(test_name: &str) -> bool {
    std::env::var("PROSE_CLAIMS_CHILD").as_deref() == Ok(test_name)
}

// ---------------------------------------------------------------- prohibition

/// Body of `fn <name>() -> ! {...}` (brace-balanced) from `src`.
fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("fn {name} not found"));
    let open = start + src[start..].find('{').unwrap();
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in fn {name}");
}

#[test]
fn claim_prohib_run_guarded() {
    // (a) Each verdict-bearing Stop entry routes through run_guarded and not run_hook.
    // (crate, dispatch arm text, entry fn)
    let stops = [
        ("donegate", "Command::Gate =>", "gate_command"),
        ("reviewgate", "Command::Review =>", "review_command"),
        ("propguard", "Command::Check =>", "check_command"),
        ("tdd", "Command::Gate =>", "gate_command"),
        ("budgetguard", "Command::Gate =>", "gate_command"),
        ("autoflow", "Command::Stop =>", "stop_command"),
        ("ctxrot", "Command::Stop =>", "stop_command"),
    ];
    for (krate, arm, entry) in stops {
        let path = crate_dir().join("..").join(krate).join("src/main.rs");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        check_stop_entry(krate, &src, arm, entry);
    }
}

fn check_stop_entry(krate: &str, src: &str, arm: &str, entry: &str) {
    {
        let main_body = fn_body(src, "main");
        let arm_line = main_body
            .lines()
            .find(|l| l.contains(arm))
            .unwrap_or_else(|| panic!("{krate}: dispatch arm {arm} missing"));
        assert!(
            arm_line.contains(&format!("{entry}()")),
            "{krate}: arm {arm} must dispatch to {entry}(), got: {arm_line}"
        );
        assert!(
            !arm_line.contains("run_hook"),
            "{krate}: Stop arm uses run_hook"
        );
        let body = fn_body(src, entry);
        assert!(
            body.contains("harness_core::gate::run::run_guarded("),
            "{krate}::{entry} does not route through run_guarded"
        );
        assert!(
            !body.contains("run_hook"),
            "{krate}::{entry} (Stop path) goes through run_hook"
        );
    }
}

#[test]
fn claim_prohib_checker_rejects_run_hook_on_a_stop_path() {
    // Test-input mutation: the (a) checker must fail when a Stop entry uses run_hook.
    let path = crate_dir().join("../donegate/src/main.rs");
    let src = std::fs::read_to_string(path).unwrap();
    let mutated = src.replace(
        "harness_core::gate::run::run_guarded(",
        "harness_core::hook::run_hook(",
    );
    assert_ne!(src, mutated);
    let r = std::panic::catch_unwind(|| {
        check_stop_entry("donegate", &mutated, "Command::Gate =>", "gate_command")
    });
    assert!(
        r.is_err(),
        "checker accepted a Stop entry that uses run_hook"
    );
}

#[test]
fn claim_prohib_run_hook_cannot_carry_a_verdict() {
    // (b) run_hook: a panicking body yields exit 0 with NOTHING on stdout.
    let name = "claim_prohib_run_hook_cannot_carry_a_verdict";
    if is_child(name) {
        harness_core::hook::run_hook(|| panic!("boom"));
    }
    let (code, stdout, stderr) = reexec(name);
    assert_eq!(code, Some(0), "run_hook must exit 0; stderr={stderr}");
    assert!(
        !stdout.contains("decision"),
        "run_hook printed a decision: {stdout}"
    );
    assert!(
        !stdout.contains("block"),
        "run_hook printed a block: {stdout}"
    );
    assert!(
        stderr.contains("[harness hook panic] hook: boom"),
        "{stderr}"
    );
}

#[test]
fn claim_prohib_run_guarded_panic_carries_a_block_decision() {
    // Contrast: the same panic under run_guarded (hook mode, first stop) emits a block.
    let name = "claim_prohib_run_guarded_panic_carries_a_block_decision";
    if is_child(name) {
        harness_core::gate::run::run_guarded("g", false, false, || -> () { panic!("boom") });
        std::process::exit(99);
    }
    let (code, stdout, _) = reexec(name);
    assert_eq!(code, Some(0));
    // The libtest harness prefixes its own "test ... " text on the same line.
    let at = stdout
        .find("{\"decision\"")
        .unwrap_or_else(|| panic!("no JSON decision on stdout: {stdout}"));
    let line = stdout[at..].lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["decision"], "block");
}

// ---------------------------------------------------------------- packaging

#[test]
fn claim_no_plugin_artifacts() {
    assert_no_plugin_artifacts(&crate_dir());
}

#[test]
fn claim_no_plugin_artifacts_checker_rejects_a_hooks_dir() {
    // Test-input mutation: a crate dir that DOES ship hooks/ must be rejected.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(
        crate_dir().join("Cargo.toml"),
        tmp.path().join("Cargo.toml"),
    )
    .unwrap();
    std::fs::create_dir(tmp.path().join("hooks")).unwrap();
    let p = tmp.path().to_path_buf();
    assert!(std::panic::catch_unwind(move || assert_no_plugin_artifacts(&p)).is_err());
}

fn assert_no_plugin_artifacts(d: &Path) {
    assert!(!d.join(".claude-plugin").exists(), ".claude-plugin present");
    assert!(!d.join(".claude-plugin/plugin.json").exists());
    assert!(!d.join("hooks").exists(), "hooks/ present");
    assert!(!d.join("bin").exists(), "bin/ present");
    assert!(!d.join("src/main.rs").exists(), "src/main.rs present");
    assert!(!d.join("src/bin").exists(), "src/bin present");
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(d.join("Cargo.toml")).unwrap()).unwrap();
    assert!(manifest.get("bin").is_none(), "Cargo.toml declares [[bin]]");
    assert_eq!(
        manifest["package"].get("autobins"),
        None,
        "autobins override present"
    );
}

// ---------------------------------------------------------------- paths

#[test]
fn claim_hook_latency_path() {
    with_temp_home(|home| {
        let expected = harness_core::config::base_dir("harness")
            .join("state")
            .join("hook-latency.jsonl");
        assert_eq!(harness_core::hook_latency::ledger_path(), expected);
        assert_eq!(
            expected,
            home.join(".harness/state/hook-latency.jsonl"),
            "base_dir(\"harness\") is not $HOME/.harness"
        );
        harness_core::hook_latency::record("donegate", "s1", 7);
        let text = std::fs::read_to_string(&expected).expect("record wrote to ledger_path");
        assert!(
            text.contains("\"donegate\"") && text.contains("\"elapsed_ms\":7"),
            "{text}"
        );
    });
}

#[test]
fn claim_ledger_path() {
    with_temp_home(|home| {
        let dir = harness_core::ledger::default_state_dir();
        assert_eq!(dir, home.join(".budgetguard/state"));
        let mut l = harness_core::ledger::Ledger::default();
        l.record("s1", "2026-01-02", 1.5);
        l.save(&dir).unwrap();
        let file = home.join(".budgetguard/state/ledger.json");
        assert!(file.is_file(), "ledger.json not at {file:?}");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v.to_string().contains("2026-01-02"));
    });
}

#[test]
fn claim_lessons_path() {
    with_temp_home(|home| {
        assert_eq!(
            harness_core::lessons::store_path(),
            home.join(".lessons/lessons.jsonl")
        );
        harness_core::lessons::append(&harness_core::lessons::Lesson {
            id: "l1".into(),
            kind: harness_core::lessons::Kind::Convention,
            task_summary: "t".into(),
            lesson_text: "carry me".into(),
            source_run: "r".into(),
            ts: 1,
        });
        let text = std::fs::read_to_string(home.join(".lessons/lessons.jsonl"))
            .expect("append wrote to $HOME/.lessons/lessons.jsonl");
        assert!(text.contains("carry me"), "{text}");
    });
}

#[test]
fn claim_session_path() {
    let state = tempfile::tempdir().unwrap();
    let rec: harness_core::session::SessionRecord = serde_json::from_value(serde_json::json!({
        "session_id": "abc-123", "project": "p", "cwd": "/x"
    }))
    .unwrap();
    harness_core::session::upsert(state.path(), &rec);
    let file = state.path().join("sessions").join("abc-123.json");
    assert!(file.is_file(), "{file:?} missing");
    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(back["session_id"], "abc-123");
}

// ---------------------------------------------------------------- scorer / re-exports

#[test]
fn claim_scorer_formula() {
    use harness_core::{score, Candidate, Effort, Lens, Severity};
    let sevs = [Severity::High, Severity::Medium, Severity::Low];
    let effs = [Effort::Xs, Effort::S, Effort::M, Effort::L, Effort::Xl];
    let lenses = [Lens::L1, Lens::L2, Lens::L3, Lens::L4, Lens::L5];
    for s in sevs {
        for e in effs {
            for l in lenses {
                for gp in [-1.0, 0.0, 0.25, 0.8, 1.0, 5.0] {
                    let got = score(&Candidate {
                        severity: s,
                        effort: e,
                        lens: l,
                        goal_proximity: gp,
                    });
                    let want = s.weight() * f64::clamp(gp, 0.0, 1.0) / e.factor() * l.multiplier();
                    assert_eq!(got.to_bits(), want.to_bits(), "{s:?} {e:?} {l:?} {gp}");
                }
            }
        }
    }
    // Literal anchors (tables are part of the claim's inputs).
    let c = Candidate {
        severity: Severity::High,
        effort: Effort::S,
        lens: Lens::L2,
        goal_proximity: 0.8,
    };
    assert_eq!(score(&c), 3.0 * 0.8 / 2.0 * 1.5);
    assert_eq!(
        score(&Candidate {
            goal_proximity: 5.0,
            ..c
        }),
        3.0 * 1.0 / 2.0 * 1.5
    );
    assert_eq!(
        score(&Candidate {
            goal_proximity: -1.0,
            ..c
        }),
        0.0
    );
}

#[test]
fn claim_estimate_reexport() {
    let f: fn(
        &str,
        &[harness_core::pricing::PriceOverride],
    ) -> Option<harness_core::TranscriptCostEstimate> = harness_core::estimate_transcript_cost;
    let fixture = crate_dir().join("tests/fixtures/transcript.jsonl");
    let est = f(fixture.to_str().unwrap(), &[]).expect("fixture transcript yields an estimate");
    let direct =
        harness_core::estimate::estimate_transcript_cost(fixture.to_str().unwrap(), &[]).unwrap();
    assert_eq!(est.cost_usd, direct.cost_usd);
}

// ---------------------------------------------------------------- run_guarded / install

#[test]
fn claim_run_guarded_interactive_exit1() {
    let name = "claim_run_guarded_interactive_exit1";
    if is_child(name) {
        harness_core::gate::run::run_guarded("mygate", true, false, || -> () { panic!("boom") });
        std::process::exit(99);
    }
    let (code, stdout, stderr) = reexec(name);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(stderr.contains("mygate: internal error"), "{stderr}");
    assert!(
        !stdout.contains("\"decision\""),
        "interactive emitted decision: {stdout}"
    );
}

#[test]
fn claim_install_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    let original = "{\"hooks\": {\"Stop\": []}}\n";
    std::fs::write(&settings, original).unwrap();

    let baks = |dir: &Path| -> Vec<PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("settings.json.bak-")
            })
            .collect();
        v.sort();
        v
    };

    // backup() alone: sibling copy, timestamped name, identical content.
    harness_core::install::backup(&settings).unwrap();
    let b = baks(tmp.path());
    assert_eq!(b.len(), 1, "{b:?}");
    let stamp = b[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .strip_prefix("settings.json.bak-")
        .unwrap()
        .to_string();
    let bytes = stamp.as_bytes();
    assert!(
        stamp.len() == 15
            && bytes[8] == b'-'
            && stamp
                .chars()
                .enumerate()
                .all(|(i, c)| i == 8 || c.is_ascii_digit()),
        "not YYYYmmdd-HHMMSS: {stamp}"
    );
    assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), original);
    std::fs::remove_file(&b[0]).unwrap();

    // write_settings(): backup holds the OLD content, the file holds the NEW.
    harness_core::install::write_settings(&settings, &serde_json::json!({"new": true})).unwrap();
    let b = baks(tmp.path());
    assert_eq!(b.len(), 1, "write_settings made no backup: {b:?}");
    assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), original);
    assert!(std::fs::read_to_string(&settings)
        .unwrap()
        .contains("\"new\""));
}
