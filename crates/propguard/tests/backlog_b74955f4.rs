// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reproduction for backlog b74955f4.
//!
//! Inject mode (the default) blocks with `satisfied = 0, findings = None`:
//! NOTHING was evaluated. Yet the Block's `properties` were appended to the
//! overwatch fleet `violations.jsonl` as `propguard:<id>` for every derived
//! property. A never-evaluated property is not a violation; only a
//! checked-and-FAILED one (subprocess mode, `PROP <id>: FAIL`) is.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const CRITERIA: &str = "idempotent; never panic; stable output schema";

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("propguard-b74955f4-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn hook_payload(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

fn run_in(dir: &Path, args: &[&str], payload: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_propguard");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("PROPGUARD_STATE_DIR", dir.join(".propguard-state"))
        .env_remove("PROPGUARD_CRITERIA")
        .env_remove("PROPGUARD_DISABLE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn git_init(dir: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(dir).args(&args).output();
    }
}

fn read_violations_jsonl(home: &Path) -> Vec<serde_json::Value> {
    let base = home.join(".overwatch");
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("overwatch").join("violations.jsonl");
        if let Ok(txt) = std::fs::read_to_string(&path) {
            for line in txt.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    found.push(v);
                }
            }
        }
    }
    found
}

fn propguard_signatures(violations: &[serde_json::Value]) -> Vec<String> {
    violations
        .iter()
        .filter(|v| v.get("source").and_then(|s| s.as_str()) == Some("propguard"))
        .filter_map(|v| {
            v.get("signature")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .collect()
}

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\": \"block\"") || stdout.contains("\"decision\":\"block\"")
}

/// RED: inject mode blocks, but nothing was evaluated, so ZERO propguard
/// violations may be recorded to the fleet store.
#[test]
fn inject_block_records_no_propguard_violations() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    let payload = hook_payload("s-b74955f4-inject", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[("PROPGUARD_CRITERIA", CRITERIA)],
    );
    assert_eq!(code, 0, "hook always exits 0 toward Claude");
    // Non-vacuity: the inject block path must actually have been exercised.
    assert!(is_block(&stdout), "inject mode must still block: {stdout}");

    let sigs = propguard_signatures(&read_violations_jsonl(&home));
    assert!(
        sigs.is_empty(),
        "inject-mode block evaluated NOTHING, yet recorded propguard violations: {sigs:?}"
    );
}

/// Control (must pass): subprocess mode with a checker that FAILs exactly one
/// property and PASSes the rest, below threshold -> block, and exactly the
/// failing id is recorded. Proves the violations reader observes real
/// propguard events, so the emptiness assertion above is not vacuous.
#[test]
fn subprocess_block_records_only_the_failed_property() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    // 4 derived properties for CRITERIA; threshold 4 -> one FAIL blocks.
    std::fs::create_dir_all(home.join(".propguard")).unwrap();
    std::fs::write(
        home.join(".propguard").join("config.toml"),
        "mode = \"subprocess\"\nthreshold = 4\n\
         checker_cmd = \"echo 'PROP error-path: PASS'; echo 'PROP output-schema: PASS'; \
         echo 'PROP determinism: PASS'; echo 'PROP idempotence: FAIL - not idempotent'\"\n",
    )
    .unwrap();
    let payload = hook_payload("s-b74955f4-sub", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[("PROPGUARD_CRITERIA", CRITERIA)],
    );
    assert_eq!(code, 0);
    assert!(
        is_block(&stdout),
        "subprocess below-threshold must block: {stdout}"
    );

    let sigs = propguard_signatures(&read_violations_jsonl(&home));
    assert_eq!(
        sigs,
        vec!["propguard:idempotence".to_string()],
        "exactly the checked-and-FAILED property must be recorded"
    );
}
