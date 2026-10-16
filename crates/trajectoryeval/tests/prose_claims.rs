//! README claim reproductions for trajectoryeval. Each test drives the real
//! built binary against a fresh temp dir and asserts the exact outcome the
//! README states.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let out = Command::new(env!("CARGO_BIN_EXE_trajectoryeval"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn unique_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("trajectoryeval-pc-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(dir: &Path, name: &str, content: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p.to_str().unwrap().to_string()
}

fn tier(args: &[&str]) -> Out {
    let mut a = vec!["tier"];
    a.extend_from_slice(args);
    run(&a)
}

fn check(expected: &str, actual: &str, extra: &[&str]) -> Out {
    let dir = unique_dir();
    let e = write(&dir, "e.json", expected);
    let a = write(&dir, "a.json", actual);
    let mut args = vec!["check", "--expected", &e, "--actual", &a];
    args.extend_from_slice(extra);
    run(&args)
}

#[test]
fn claim_core_allowlist_exact_match() {
    let dir = unique_dir();
    let cfg = write(&dir, "c.json", r#"{"core":["checkout"]}"#);
    let snap = write(&dir, "s.json", r#"{"a":1}"#);
    let base = write(&dir, "b.json", r#"{"a":1}"#);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "checkout",
        "--baseline",
        &base,
        "--snapshot",
        &snap,
    ]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("tier: core"), "{}", r.stdout);
    for f in ["checkout2", "Checkout", "check"] {
        let r = tier(&["--config", &cfg, "--flow", f]);
        assert_eq!(r.code, 0, "{f}: {}{}", r.stdout, r.stderr);
        assert!(r.stdout.contains("tier: non-core"), "{f}: {}", r.stdout);
    }
}

#[test]
fn claim_diff_strategy_default_structured() {
    let dir = unique_dir();
    let cfg = write(&dir, "c.json", r#"{"core":["f"]}"#);
    let base = write(&dir, "b.json", r#"{"a":1,"b":2}"#);
    let reordered = write(&dir, "s1.json", r#"{"b":2,"a":1}"#);
    let changed = write(&dir, "s2.json", r#"{"a":1,"b":3}"#);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &base,
        "--snapshot",
        &reordered,
    ]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &base,
        "--snapshot",
        &changed,
    ]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("MISMATCH at /b"), "{}", r.stdout);
}

#[test]
fn claim_threshold_permille_default_zero() {
    let dir = unique_dir();
    let cfg = write(
        &dir,
        "c.json",
        r#"{"core":["f"],"diff_strategy":"fuzzy_hash"}"#,
    );
    let mut zeros = vec![0u32; 1000];
    let base = write(&dir, "b.json", &serde_json::to_string(&zeros).unwrap());
    let same = write(&dir, "s1.json", &serde_json::to_string(&zeros).unwrap());
    zeros[999] = 1;
    // 1 differing leaf of 1000 -> distance 1 permille: must escalate at default 0.
    let one = write(&dir, "s2.json", &serde_json::to_string(&zeros).unwrap());
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &base,
        "--snapshot",
        &same,
    ]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &base,
        "--snapshot",
        &one,
        "--json",
    ]);
    assert_eq!(r.code, 3, "{}{}", r.stdout, r.stderr);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["diff"]["distance_permille"], serde_json::json!(1));
}

fn fuzzy_distance(snapshot: &str) -> i64 {
    let dir = unique_dir();
    let cfg = write(
        &dir,
        "c.json",
        r#"{"core":["f"],"diff_strategy":"fuzzy_hash"}"#,
    );
    let base = write(&dir, "b.json", r#"{"a":1,"b":2,"c":3}"#);
    let snap = write(&dir, "s.json", snapshot);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &base,
        "--snapshot",
        &snap,
        "--json",
    ]);
    assert_eq!(r.code, 3, "{}{}", r.stdout, r.stderr);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    v["diff"]["distance_permille"].as_i64().expect("distance")
}

#[test]
fn claim_fuzzy_distance_rounding() {
    // 1 of 3 leaves: 333.33 -> 333 ; 2 of 3: 666.67 -> 667 (round, not floor).
    assert_eq!(fuzzy_distance(r#"{"a":1,"b":2,"c":9}"#), 333);
    assert_eq!(fuzzy_distance(r#"{"a":1,"b":9,"c":9}"#), 667);
}

#[test]
fn claim_sampling_decision_reported() {
    let dir = unique_dir();
    let cfg2 = write(&dir, "c2.json", r#"{"core":[],"sample_one_in":2}"#);
    let cfg1 = write(&dir, "c1.json", r#"{"core":[],"sample_one_in":1}"#);
    let (mut sampled, mut skipped) = (false, false);
    for i in 0..64u32 {
        let idx = i.to_string();
        let r = tier(&[
            "--config",
            &cfg2,
            "--flow",
            "nc",
            "--seed",
            "7",
            "--run-index",
            &idx,
        ]);
        assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
        let s = r.stdout.contains("present (sampled this run)");
        let n = r.stdout.contains("present (not sampled this run)");
        assert!(s ^ n, "exactly one decision line expected: {}", r.stdout);
        sampled |= s;
        skipped |= n;
    }
    assert!(sampled && skipped, "both outcomes must occur over 64 runs");
    for i in 0..8u32 {
        let idx = i.to_string();
        let r = tier(&["--config", &cfg1, "--flow", "nc", "--run-index", &idx]);
        assert_eq!(r.code, 0);
        assert!(
            r.stdout.contains("present (sampled this run)"),
            "{}",
            r.stdout
        );
    }
}

#[test]
fn claim_tier_exit_two_harness_error() {
    let dir = unique_dir();
    let missing = dir.join("nope.json");
    let missing = missing.to_str().unwrap();
    let r = tier(&["--config", missing, "--flow", "f"]);
    assert_eq!(r.code, 2, "(a) {}", r.stderr);
    assert!(r.stderr.contains("cannot read tier config"), "{}", r.stderr);

    let bad = write(&dir, "bad.json", "{not json");
    let r = tier(&["--config", &bad, "--flow", "f"]);
    assert_eq!(r.code, 2, "(b) {}", r.stderr);
    assert!(
        r.stderr.contains("invalid tier config JSON"),
        "{}",
        r.stderr
    );

    let cfg = write(&dir, "c.json", r#"{"core":["f"]}"#);
    let good = write(&dir, "g.json", r#"{"a":1}"#);
    let invalid = write(&dir, "i.json", "[[");
    let r = tier(&["--config", &cfg, "--flow", "f", "--baseline", &good]);
    assert_eq!(r.code, 2, "(c) only baseline: {}", r.stderr);
    assert!(
        r.stderr.contains("requires --baseline and --snapshot"),
        "{}",
        r.stderr
    );
    let r = tier(&["--config", &cfg, "--flow", "f", "--snapshot", &good]);
    assert_eq!(r.code, 2, "(c) only snapshot: {}", r.stderr);
    let r = tier(&["--config", &cfg, "--flow", "f"]);
    assert_eq!(r.code, 2, "(c) neither: {}", r.stderr);

    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        missing,
        "--snapshot",
        &good,
    ]);
    assert_eq!(r.code, 2, "(d) unreadable baseline: {}", r.stderr);
    assert!(r.stderr.contains("cannot read baseline"), "{}", r.stderr);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &good,
        "--snapshot",
        missing,
    ]);
    assert_eq!(r.code, 2, "(d) unreadable snapshot: {}", r.stderr);
    assert!(r.stderr.contains("cannot read snapshot"), "{}", r.stderr);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &invalid,
        "--snapshot",
        &good,
    ]);
    assert_eq!(r.code, 2, "(d) invalid baseline: {}", r.stderr);
    assert!(r.stderr.contains("invalid baseline JSON"), "{}", r.stderr);
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "f",
        "--baseline",
        &good,
        "--snapshot",
        &invalid,
    ]);
    assert_eq!(r.code, 2, "(d) invalid snapshot: {}", r.stderr);
    assert!(r.stderr.contains("invalid snapshot JSON"), "{}", r.stderr);
}

#[test]
fn claim_check_exit_two_unparseable() {
    let good_spec = r#"{"mode":"strict","steps":[{"tool":"Read"}]}"#;
    let r = check("{nope", r#"["Read"]"#, &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(
        r.stderr.contains("invalid expected spec JSON"),
        "{}",
        r.stderr
    );
    let r = check(good_spec, "{}", &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(
        r.stderr.contains("invalid actual trajectory JSON"),
        "{}",
        r.stderr
    );
    let r = check(good_spec, "{nope", &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    // unreadable
    let dir = unique_dir();
    let a = write(&dir, "a.json", r#"["Read"]"#);
    let missing = dir.join("m.json");
    let r = run(&[
        "check",
        "--expected",
        missing.to_str().unwrap(),
        "--actual",
        &a,
    ]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    // nonexistent --actual
    let e = write(&dir, "e.json", good_spec);
    let r = run(&[
        "check",
        "--expected",
        &e,
        "--actual",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(
        r.stderr.contains("cannot read actual trajectory"),
        "{}",
        r.stderr
    );
}

#[test]
fn claim_check_json_flag() {
    let spec = r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#;
    let r = check(spec, r#"["Read"]"#, &["--json"]);
    assert_eq!(r.code, 1);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).expect("stdout is JSON");
    let o = v.as_object().expect("object");
    for k in ["pass", "missing", "unexpected", "out_of_order"] {
        assert!(o.contains_key(k), "missing key {k} in {}", r.stdout);
    }
    assert_eq!(o["pass"], serde_json::json!(false));
    assert_eq!(o["missing"], serde_json::json!(["Edit"]));
    let r = check(spec, r#"["Read","Edit"]"#, &[]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("trajectory matched (pass)"),
        "{}",
        r.stdout
    );
    assert!(serde_json::from_str::<serde_json::Value>(r.stdout.trim()).is_err());
}

#[test]
fn claim_mode_required() {
    let r = check(r#"{"steps":[{"tool":"Read"}]}"#, r#"["Read"]"#, &[]);
    assert_eq!(r.code, 2, "missing mode: {}", r.stderr);
    let r = check(
        r#"{"mode":"bogus","steps":[{"tool":"Read"}]}"#,
        r#"["Read"]"#,
        &[],
    );
    assert_eq!(r.code, 2, "unknown mode: {}", r.stderr);
    for m in ["strict", "unordered", "subsequence"] {
        let spec = format!(r#"{{"mode":"{m}","steps":[{{"tool":"Read"}}]}}"#);
        let r = check(&spec, r#"["Read"]"#, &[]);
        assert_eq!(r.code, 0, "{m}: {}{}", r.stdout, r.stderr);
    }
}

#[test]
fn claim_steps_default_empty() {
    let r = check(r#"{"mode":"strict"}"#, r#"["Read"]"#, &["--json"]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["unexpected"], serde_json::json!(["Read"]));
    assert_eq!(v["missing"], serde_json::json!([]));
}

#[test]
fn claim_extract_unreadable_exits_two() {
    let dir = unique_dir();
    let p = dir.join("no-such.jsonl");
    let r = run(&["extract", "--transcript", p.to_str().unwrap()]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("cannot read transcript"), "{}", r.stderr);
    assert!(r.stdout.is_empty(), "{}", r.stdout);
}

#[test]
fn claim_not_a_lifecycle_hook() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("hooks").exists(), "hooks/ dir present");
    assert!(!root.join("hooks.json").exists());
    let raw = std::fs::read_to_string(root.join(".claude-plugin/plugin.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(v.as_object().unwrap().get("hooks").is_none());
}

#[test]
fn claim_example_fuzzy_threshold_250() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tier-config-fuzzy.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    assert_eq!(v["diff_strategy"], serde_json::json!("fuzzy_hash"));
    assert_eq!(v["threshold_permille"], serde_json::json!(250));
}

#[test]
fn claim_strict_optional_out_of_slot_fails() {
    let spec = r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit","optional":true},{"tool":"Grep"}]}"#;
    let r = check(spec, r#"["Read","Grep","Edit"]"#, &["--json"]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    // control: optional in its slot passes, absent passes
    assert_eq!(check(spec, r#"["Read","Edit","Grep"]"#, &[]).code, 0);
    assert_eq!(check(spec, r#"["Read","Grep"]"#, &[]).code, 0);
}

#[test]
fn claim_check_exit_one_unexpected_and_out_of_order() {
    let spec = r#"{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}"#;
    let r = check(spec, r#"["Read","Edit","Bash"]"#, &[]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("unexpected:  Bash"), "{}", r.stdout);
    let r = check(spec, r#"["Edit","Read"]"#, &[]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("out of order"), "{}", r.stdout);
}

#[test]
fn claim_tier_json_report_core_and_non_core() {
    let dir = unique_dir();
    let cfg = write(&dir, "c.json", r#"{"core":["c"]}"#);
    let snap = write(&dir, "s.json", r#"{"a":1}"#);
    let r = tier(&["--config", &cfg, "--flow", "n", "--json"]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["tier"], serde_json::json!("noncore"));
    assert_eq!(v["verdict"], serde_json::json!("pass"));
    let r = tier(&[
        "--config",
        &cfg,
        "--flow",
        "c",
        "--baseline",
        &snap,
        "--snapshot",
        &snap,
        "--json",
    ]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim()).unwrap();
    assert_eq!(v["tier"], serde_json::json!("core"));
    assert_eq!(v["diff"]["kind"], serde_json::json!("match"));
    let r = tier(&["--config", &cfg, "--flow", "n"]);
    assert!(r.stdout.contains("tier: non-core"), "{}", r.stdout);
}

#[test]
fn claim_sampling_keyed_by_seed_and_flow() {
    let dir = unique_dir();
    let cfg = write(&dir, "c.json", r#"{"core":[],"sample_one_in":2}"#);
    let dec = |flow: &str, seed: u32, idx: u32| -> (bool, String) {
        let (s, i) = (seed.to_string(), idx.to_string());
        let r = tier(&[
            "--config",
            &cfg,
            "--flow",
            flow,
            "--seed",
            &s,
            "--run-index",
            &i,
        ]);
        assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
        let sampled = r.stdout.contains("present (sampled this run)");
        assert!(sampled ^ r.stdout.contains("present (not sampled this run)"));
        (sampled, r.stdout)
    };
    let seed_varies = (0..32).any(|i| {
        let d: Vec<bool> = (0..8).map(|s| dec("flow", s, i).0).collect();
        d.iter().any(|x| *x) && d.iter().any(|x| !*x)
    });
    assert!(seed_varies, "seed alone must change the decision");
    let flows: Vec<String> = (0..8).map(|i| format!("flow{i}")).collect();
    let flow_varies = (0..32).any(|i| {
        let d: Vec<bool> = flows.iter().map(|f| dec(f, 3, i).0).collect();
        d.iter().any(|x| *x) && d.iter().any(|x| !*x)
    });
    assert!(flow_varies, "flow id alone must change the decision");
    assert_eq!(dec("flow", 5, 9).1, dec("flow", 5, 9).1);
}
