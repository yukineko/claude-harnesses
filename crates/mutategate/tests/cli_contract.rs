#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for the statements in crates/mutategate/README*.md.
//! Written independently of the implementation (CLAUDE.md 2(a)).
//!
//! Binary claims drive the real `mutategate` binary with a throwaway HOME and
//! cwd. Script claims drive the REAL scripts/mutation-gate.sh, copied into a
//! temp fake repo root, with a stub `cargo` first on PATH that records its
//! argv + cwd. The real cargo-mutants / a real cargo build are never run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

// ───────────────────────── binary helpers ─────────────────────────

fn outcomes_json(caught: usize, missed: usize, unviable: usize, unknown: usize) -> String {
    let mut v = Vec::new();
    let mut push = |n: usize, s: &str| {
        for _ in 0..n {
            v.push(format!(
                r#"{{ "scenario": {{ "Mutant": {{}} }}, "summary": "{s}" }}"#
            ));
        }
    };
    push(caught, "CaughtMutant");
    push(missed, "MissedMutant");
    push(unviable, "Unviable");
    push(unknown, "SomeFutureState");
    format!(r#"{{ "outcomes": [ {} ] }}"#, v.join(","))
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_bin(cwd: &Path, args: &[&str]) -> Run {
    let home = tempfile::tempdir().unwrap();
    let out: Output = Command::new(env!("CARGO_BIN_EXE_mutategate"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home.path())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn sandbox_with(path: &str, json: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, json).unwrap();
    d
}

// ───────────────────────── not_a_plugin ─────────────────────────

#[test]
fn claim_not_a_plugin_a() {
    let plugin_json = repo_root().join("crates/mutategate/.claude-plugin/plugin.json");
    assert!(!plugin_json.exists(), "{} exists", plugin_json.display());
    // Control: the crate dir itself exists, so the absence is not a wrong path.
    assert!(repo_root().join("crates/mutategate/Cargo.toml").is_file());
}

#[test]
fn claim_not_a_plugin_b() {
    let text =
        std::fs::read_to_string(repo_root().join(".claude-plugin/marketplace.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let plugins = v["plugins"].as_array().expect("plugins array");
    assert!(plugins.len() > 10, "marketplace parsed but looks empty");
    // Control: a real plugin entry is findable the same way.
    assert!(plugins.iter().any(|p| p["name"] == "blastguard"));
    for p in plugins {
        assert_ne!(p["name"], "mutategate");
        // Non-vacuous: every entry must expose a string source path (an
        // unreadable shape is a failure, not a pass), and nothing in the whole
        // serialized source may mention mutategate.
        let path = p["source"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("entry has no string source.path: {p}"));
        assert!(
            !path.ends_with("mutategate"),
            "entry sources mutategate: {p}"
        );
        let src = p["source"].to_string();
        assert!(
            !src.contains("mutategate"),
            "entry source mentions mutategate: {p}"
        );
    }
}

// ───────────────────── unknown_warning_in_reason ─────────────────────

#[test]
fn claim_unknown_warning_in_reason_a() {
    // 9 caught + 1 unknown = 90% -> PASS; the reason is printed on stdout.
    let d = sandbox_with("o.json", &outcomes_json(9, 0, 0, 1));
    let r = run_bin(d.path(), &["--outcomes", "o.json"]);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains("PASS:")
            && r.stdout
                .contains("[warning: 1 mutant(s) had an unrecognised summary state"),
        "stdout: {}",
        r.stdout
    );
}

#[test]
fn claim_unknown_warning_in_reason_b() {
    // 1 caught + 1 unknown = 50% -> FAIL; the reason is printed on stderr.
    let d = sandbox_with("o.json", &outcomes_json(1, 0, 0, 1));
    let r = run_bin(d.path(), &["--outcomes", "o.json"]);
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("FAIL:")
            && r.stderr
                .contains("[warning: 1 mutant(s) had an unrecognised summary state"),
        "stderr: {}",
        r.stderr
    );
}

#[test]
fn claim_unknown_warning_in_reason_c_no_unknown_no_warning() {
    for (c, m) in [(9, 0), (1, 1)] {
        let d = sandbox_with("o.json", &outcomes_json(c, m, 0, 0));
        let r = run_bin(d.path(), &["--outcomes", "o.json"]);
        let all = format!("{}{}", r.stdout, r.stderr);
        assert!(all.contains("kill-rate"), "ran: {all}");
        assert!(!all.contains("warning"), "spurious warning: {all}");
    }
}

// ───────────────────────── defaults ─────────────────────────

#[test]
fn claim_defaults_a_exactly_080_passes_with_default_path() {
    let d = sandbox_with("mutants.out/outcomes.json", &outcomes_json(4, 1, 0, 0));
    let r = run_bin(d.path(), &[]);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("threshold: 80.0%"), "{}", r.stdout);
}

#[test]
fn claim_defaults_b_075_fails_with_default_path() {
    let d = sandbox_with("mutants.out/outcomes.json", &outcomes_json(3, 1, 0, 0));
    let r = run_bin(d.path(), &[]);
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("FAIL"), "{}", r.stderr);
}

#[test]
fn claim_defaults_c_default_path_is_the_one_read() {
    // Outcomes only at a DIFFERENT path: the no-flag run must not find it.
    let d = sandbox_with("outcomes.json", &outcomes_json(4, 1, 0, 0));
    let r = run_bin(d.path(), &[]);
    assert_eq!(r.code, Some(2), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("mutants.out/outcomes.json"),
        "{}",
        r.stderr
    );
}

// ───────────────────────── exit_codes ─────────────────────────

#[test]
fn claim_exit_codes_a_pass_is_0() {
    let d = sandbox_with("o.json", &outcomes_json(4, 1, 0, 0));
    assert_eq!(run_bin(d.path(), &["--outcomes", "o.json"]).code, Some(0));
}

#[test]
fn claim_exit_codes_b_below_threshold_is_1() {
    let d = sandbox_with("o.json", &outcomes_json(1, 1, 0, 0));
    assert_eq!(run_bin(d.path(), &["--outcomes", "o.json"]).code, Some(1));
}

#[test]
fn claim_exit_codes_c_no_viable_mutants_is_1() {
    let d = sandbox_with("o.json", &outcomes_json(0, 0, 3, 0));
    let r = run_bin(d.path(), &["--outcomes", "o.json"]);
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("no viable mutants"), "{}", r.stderr);
}

#[test]
fn claim_exit_codes_d_missing_file_is_2() {
    let d = tempfile::tempdir().unwrap();
    let r = run_bin(d.path(), &["--outcomes", "absent.json"]);
    assert_eq!(r.code, Some(2), "{}{}", r.stdout, r.stderr);
}

#[test]
fn claim_exit_codes_e_malformed_json_is_2() {
    let d = sandbox_with("o.json", "{ not json");
    let r = run_bin(d.path(), &["--outcomes", "o.json"]);
    assert_eq!(r.code, Some(2), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("failed to parse"), "{}", r.stderr);
}

#[test]
fn claim_exit_codes_f_unknown_flag_is_2() {
    let d = sandbox_with("o.json", &outcomes_json(4, 1, 0, 0));
    let r = run_bin(d.path(), &["--outcomes", "o.json", "--no-such-flag"]);
    assert_eq!(r.code, Some(2), "{}{}", r.stdout, r.stderr);
}

// ───────────────────────── script harness ─────────────────────────

const STUB: &str = r#"#!/usr/bin/env bash
if [ "${1:-}" = "mutants" ] && [ "${2:-}" = "--version" ]; then
  echo "cargo-mutants 99.99.99 (stub)"; exit 0
fi
n=$(ls "$STUB_CALLS" | wc -l | tr -d ' ')
f="$STUB_CALLS/call-$n"
{ echo "$(pwd -P)"; printf '%s\n' "$@"; } > "$f"
if [ "${1:-}" = "mutants" ]; then
  out=""
  prev=""
  for a in "$@"; do
    if [ "$prev" = "--output" ]; then out="$a"; fi
    prev="$a"
  done
  if [ -n "$out" ]; then
    mkdir -p "$out/mutants.out"
    printf '%s' "$STUB_OUTCOMES" > "$out/mutants.out/outcomes.json"
  fi
  exit 0
fi
if [ "${1:-}" = "run" ]; then
  # Forward everything after `--` to the REAL mutategate binary.
  shift
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do shift; done
  shift
  exec "$STUB_MUTATEGATE" "$@"
fi
exit 0
"#;

struct Call {
    cwd: PathBuf,
    args: Vec<String>,
}

struct GateRun {
    root: tempfile::TempDir,
    code: Option<i32>,
    stdout: String,
    stderr: String,
    mutants: Call,
    scorer: Option<Call>,
}

fn read_call(p: &Path) -> Call {
    let text = std::fs::read_to_string(p).unwrap();
    let mut lines = text.lines();
    let cwd = PathBuf::from(lines.next().unwrap());
    Call {
        cwd,
        args: lines.map(str::to_string).collect(),
    }
}

/// Run the real mutation-gate.sh in a temp fake repo root.
fn run_gate(env: &[(&str, &str)], outcomes: &str) -> GateRun {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().canonicalize().unwrap();
    std::fs::create_dir_all(rp.join("scripts")).unwrap();
    std::fs::copy(
        repo_root().join("scripts/mutation-gate.sh"),
        rp.join("scripts/mutation-gate.sh"),
    )
    .unwrap();
    let bin = rp.join("stubbin");
    let calls = rp.join("calls");
    let home = rp.join("home");
    for d in [&bin, &calls, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, STUB).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cmd = Command::new("bash");
    cmd.arg(rp.join("scripts/mutation-gate.sh"))
        .current_dir(&rp)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("STUB_CALLS", &calls)
        .env("STUB_OUTCOMES", outcomes)
        .env("STUB_MUTATEGATE", env!("CARGO_BIN_EXE_mutategate"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&calls)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort_by_key(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("call-")
            .parse::<u32>()
            .unwrap()
    });
    let all: Vec<Call> = files.iter().map(|p| read_call(p)).collect();
    let mut mutants = None;
    let mut scorer = None;
    for c in all {
        match c.args.first().map(String::as_str) {
            Some("mutants") => mutants = Some(c),
            Some("run") => scorer = Some(c),
            _ => {}
        }
    }
    GateRun {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        mutants: mutants.unwrap_or_else(|| {
            panic!(
                "cargo mutants was never invoked; stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }),
        scorer,
        root,
    }
}

fn pass_outcomes() -> String {
    outcomes_json(4, 1, 0, 0)
}

/// Value following `flag` in `args`, if present.
fn flag_val<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_pair(args: &[String], flag: &str, val: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == val)
}

// ───────────────────── pilot defaults (script) ─────────────────────

#[test]
fn claim_pilot_default_harness_core() {
    let g = run_gate(&[], &pass_outcomes());
    assert_eq!(flag_val(&g.mutants.args, "--package"), Some("harness-core"));
    assert!(
        !g.mutants.args.iter().any(|a| a == "--file"),
        "default pilot must not be narrowed: {:?}",
        g.mutants.args
    );
    assert_eq!(g.code, Some(0), "{}{}", g.stdout, g.stderr);
}

#[test]
fn claim_pilot_specguard_default_a_file() {
    let g = run_gate(&[("PILOT", "specguard")], &pass_outcomes());
    assert_eq!(flag_val(&g.mutants.args, "--package"), Some("specguard"));
    assert!(
        has_pair(
            &g.mutants.args,
            "--file",
            "crates/specguard/src/similarity.rs"
        ),
        "{:?}",
        g.mutants.args
    );
}

#[test]
fn claim_pilot_specguard_default_b_skip() {
    let g = run_gate(&[("PILOT", "specguard")], &pass_outcomes());
    // Exact form observed: cargo-mutants gets these three args, in order, after
    // the --file pair.
    let a = &g.mutants.args;
    let want = [
        "--cargo-test-arg=--",
        "--cargo-test-arg=--skip",
        "--cargo-test-arg=ack_blocks_when_no_new_commits_since_raised",
    ];
    assert!(
        a.windows(3).any(|w| w == want),
        "skip args missing/misordered: {a:?}"
    );
}

#[test]
fn claim_pilot_condukt_default() {
    let g = run_gate(&[("PILOT", "condukt")], &pass_outcomes());
    assert_eq!(flag_val(&g.mutants.args, "--package"), Some("condukt"));
    assert!(
        has_pair(&g.mutants.args, "--file", "crates/condukt/src/circuit.rs"),
        "{:?}",
        g.mutants.args
    );
}

#[test]
fn claim_pilot_blastguard_default() {
    let g = run_gate(&[("PILOT", "blastguard")], &pass_outcomes());
    assert_eq!(flag_val(&g.mutants.args, "--package"), Some("blastguard"));
    assert!(
        has_pair(
            &g.mutants.args,
            "--file",
            "crates/blastguard/src/classify.rs"
        ),
        "{:?}",
        g.mutants.args
    );
}

// ───────────────────── mutants_extra_overrides ─────────────────────

#[test]
fn claim_mutants_extra_overrides_a_arbitrary_pilot() {
    let g = run_gate(&[("PILOT", "difflog")], &pass_outcomes());
    assert_eq!(flag_val(&g.mutants.args, "--package"), Some("difflog"));
    assert!(!g.mutants.args.iter().any(|a| a == "--file"));
}

#[test]
fn claim_mutants_extra_overrides_b_path_passed_verbatim_cwd_is_repo_root() {
    let g = run_gate(
        &[("MUTANTS_EXTRA", "--file crates/harness-core/src/hash.rs")],
        &pass_outcomes(),
    );
    assert!(
        has_pair(&g.mutants.args, "--file", "crates/harness-core/src/hash.rs"),
        "{:?}",
        g.mutants.args
    );
    // The path is relative; cargo-mutants runs with cwd = the repo root
    // (the dir above scripts/), so it resolves against the repo root.
    let root = g.root.path().canonicalize().unwrap();
    assert_eq!(g.mutants.cwd, root);
}

#[test]
fn claim_mutants_extra_overrides_b2_cwd_independent_of_caller_cwd() {
    // Same run, but the caller's cwd is NOT the repo root.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().canonicalize().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(rp.join("scripts")).unwrap();
    std::fs::copy(
        repo_root().join("scripts/mutation-gate.sh"),
        rp.join("scripts/mutation-gate.sh"),
    )
    .unwrap();
    let bin = rp.join("stubbin");
    let calls = rp.join("calls");
    for d in [&bin, &calls, &rp.join("home")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(bin.join("cargo"), STUB).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let out = Command::new("bash")
        .arg(rp.join("scripts/mutation-gate.sh"))
        .current_dir(other.path())
        .env_clear()
        .env("HOME", rp.join("home"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("STUB_CALLS", &calls)
        .env("STUB_OUTCOMES", pass_outcomes())
        .env("STUB_MUTATEGATE", env!("CARGO_BIN_EXE_mutategate"))
        .env("MUTANTS_EXTRA", "--file crates/harness-core/src/hash.rs")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let call = read_call(&calls.join("call-0"));
    assert_eq!(call.cwd, rp, "cargo-mutants must run from the repo root");
}

#[test]
fn claim_mutants_extra_overrides_c_explicit_extra_replaces_default() {
    let g = run_gate(
        &[("PILOT", "specguard"), ("MUTANTS_EXTRA", "--file x.rs")],
        &pass_outcomes(),
    );
    let a = &g.mutants.args;
    assert!(has_pair(a, "--file", "x.rs"), "{a:?}");
    assert!(
        !a.iter().any(|s| s.contains("similarity.rs")),
        "default scope leaked: {a:?}"
    );
    assert!(
        !a.iter()
            .any(|s| s.contains("--skip") || s.contains("ack_blocks")),
        "default --skip leaked: {a:?}"
    );
    assert_eq!(a.iter().filter(|s| *s == "--file").count(), 1, "{a:?}");
}

// ─────────────── min_kill_rate_env_and_survivors_dir ───────────────

#[test]
fn claim_min_kill_rate_env_a_passed_to_mutategate() {
    let g = run_gate(&[("MIN_KILL_RATE", "0.70")], &outcomes_json(7, 3, 0, 0));
    let s = g.scorer.as_ref().expect("scorer not invoked");
    assert_eq!(
        flag_val(&s.args, "--min-kill-rate"),
        Some("0.70"),
        "{:?}",
        s.args
    );
    // Observed verdict: 70% meets a 0.70 floor.
    assert_eq!(g.code, Some(0), "{}{}", g.stdout, g.stderr);
    assert!(g.stdout.contains("threshold: 70.0%"), "{}", g.stdout);
}

#[test]
fn claim_min_kill_rate_env_a2_default_is_080_and_fails_same_data() {
    let g = run_gate(&[], &outcomes_json(7, 3, 0, 0));
    let s = g.scorer.as_ref().expect("scorer not invoked");
    assert_eq!(flag_val(&s.args, "--min-kill-rate"), Some("0.80"));
    assert_eq!(g.code, Some(1), "{}{}", g.stdout, g.stderr);
}

#[test]
fn claim_min_kill_rate_env_b_output_dir_per_pilot() {
    let g = run_gate(&[("PILOT", "difflog")], &pass_outcomes());
    assert_eq!(
        flag_val(&g.mutants.args, "--output"),
        Some("target/mutants-difflog"),
        "{:?}",
        g.mutants.args
    );
    // The scorer reads the outcomes under that dir's mutants.out/ subdirectory,
    // i.e. cargo-mutants' results (and missed.txt beside outcomes.json) live at
    // target/mutants-<pilot>/mutants.out/.
    let s = g.scorer.as_ref().expect("scorer not invoked");
    assert_eq!(
        flag_val(&s.args, "--outcomes"),
        Some("target/mutants-difflog/mutants.out/outcomes.json")
    );
}

// ───────────────────────── inert_no_trigger ─────────────────────────

#[test]
fn claim_inert_no_trigger() {
    let root = repo_root();
    let hooks = root.join(".githooks");
    let rd = std::fs::read_dir(&hooks)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", hooks.display()));
    let mut scanned = 0;
    let mut precommit = String::new();
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry unreadable: {e}"));
        let path = entry.path();
        let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if !meta.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("unreadable hook {}: {e}", path.display()));
        assert!(
            !text.contains("mutation-gate"),
            "{} references mutation-gate",
            path.display()
        );
        if path.file_name().unwrap() == "pre-commit" {
            precommit = text;
        }
        scanned += 1;
    }
    assert!(scanned > 0, ".githooks has no regular files");
    // Control: the scan sees real hook content.
    assert!(
        precommit.contains("#!/") && precommit.len() > 1000,
        "pre-commit not read as real hook content"
    );
    assert!(
        !root.join(".github/workflows").exists(),
        ".github/workflows exists"
    );
}
