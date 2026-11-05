// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! hypothesis f5f9522a: condukt must forward the decomposition task's
//! `suggested_model` to `fugu-router record --suggested-model <m>` when it is
//! known, and OMIT the flag when it is not (never `--suggested-model ""`).
//!
//! `spawn_fugu_record` builds its argv inline inside a `Command` (not a pure
//! function), so the argv is observed through a FAKE `fugu-router` placed
//! first on PATH (the mechanism `fugu_router_record_e2e.rs` uses for `gauge`;
//! `harness_core::plugin_bin::resolve` finds it by PATH). The fake answers
//! `fingerprint` and dumps every other invocation's argv, one arg per line,
//! to a file; `--- ` separates invocations.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-suggested-model-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

/// Fake fugu-router: `fingerprint` -> prints `fp`; anything else -> appends
/// its argv (one per line, then `---`) to `dump`, exit 0.
fn write_fake_fugu(bin_dir: &Path, dump: &Path) {
    let path = bin_dir.join("fugu-router");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"fingerprint\" ]; then echo fp; exit 0; fi\n\
         if [ \"$1\" = \"--version\" ]; then echo fugu-router-fake; exit 0; fi\n\
         for a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n\
         echo '---' >> '{}'\n\
         exit 0\n",
        dump.display(),
        dump.display()
    );
    std::fs::write(&path, script).expect("write fake fugu-router");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

/// Runs init -> set running -> set verified -> record-run for a one-task
/// decomposition whose task carries `suggested_model` (or not). Returns the
/// recorded argv invocations (each a Vec of args).
fn run_and_capture(tag: &str, suggested: Option<&str>) -> Vec<Vec<String>> {
    let dir = unique_dir(&format!("{tag}-cwd"));
    let home = unique_dir(&format!("{tag}-home"));
    let fake_bin = unique_dir(&format!("{tag}-bin"));
    let dump = fake_bin.join("argv.dump");
    write_fake_fugu(&fake_bin, &dump);

    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin.clone()];
    paths.extend(std::env::split_paths(&existing));
    let combined = std::env::join_paths(paths).expect("join PATH");
    let bin = env!("CARGO_BIN_EXE_condukt");
    let run = |args: &[&str]| -> (i32, String, String) {
        let out = harness_core::git_env::scrub(&mut Command::new(bin))
            .args(args)
            .current_dir(&dir)
            .env("HOME", &home)
            .env("PATH", &combined)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("condukt spawns");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let mut task = serde_json::json!({"id": "t1", "title": "demo task", "touched_files": []});
    if let Some(m) = suggested {
        task["suggested_model"] = serde_json::json!(m);
    }
    let decomp = serde_json::json!({"goal": "demo goal", "tasks": [task]}).to_string();
    let dec_path = dir.join("decomposition.json");
    std::fs::write(&dec_path, decomp).expect("write decomposition");

    let (code, stdout, stderr) = run(&["state", "init", "--file", dec_path.to_str().unwrap()]);
    assert_eq!(code, 0, "init failed: {stdout} {stderr}");
    let rid = stdout
        .lines()
        .chain(stderr.lines())
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("run- id")
        .to_string();
    let (code, o, e) = run(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "running",
    ]);
    assert_eq!(code, 0, "set running failed: {o} {e}");
    // The executed model (sonnet) deliberately differs from the suggestion
    // (opus) so the flag cannot be confused with `--model`.
    let (code, o, e) = run(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "verified", "--model", "sonnet",
        "--cost", "0",
    ]);
    assert_eq!(code, 0, "set verified failed: {o} {e}");
    let (code, o, e) = run(&["state", "record-run", "--all"]);
    assert_eq!(code, 0, "record-run failed: {o} {e}");

    let raw = std::fs::read_to_string(&dump)
        .unwrap_or_else(|err| panic!("fake fugu-router never got `record` (stderr={e}): {err}"));
    let mut calls: Vec<Vec<String>> = vec![vec![]];
    for l in raw.lines() {
        if l == "---" {
            calls.push(vec![]);
        } else {
            calls.last_mut().unwrap().push(l.to_string());
        }
    }
    calls.retain(|c| !c.is_empty());
    assert!(
        calls
            .iter()
            .all(|c| c.first().map(String::as_str) == Some("record")),
        "unexpected non-record call: {calls:?}"
    );
    calls
}

/// (e) RED today: the flag is never passed.
#[test]
fn record_run_forwards_suggested_model_when_known() {
    let calls = run_and_capture("known", Some("opus"));
    let worker = calls
        .iter()
        .find(|c| c.windows(2).any(|w| w[0] == "--role" && w[1] == "worker"))
        .unwrap_or_else(|| panic!("no worker record call: {calls:?}"));
    assert!(
        worker
            .windows(2)
            .any(|w| w[0] == "--suggested-model" && w[1] == "opus"),
        "worker argv must carry `--suggested-model opus`: {worker:?}"
    );
    assert!(
        worker
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "sonnet"),
        "executed --model stays sonnet: {worker:?}"
    );
}

/// (f) Guard (passes today, must keep passing): unknown -> flag omitted, and
/// never an empty value.
#[test]
fn record_run_omits_suggested_model_flag_when_unknown() {
    let calls = run_and_capture("unknown", None);
    assert!(!calls.is_empty(), "record must have been called");
    for c in &calls {
        assert!(
            !c.iter().any(|a| a == "--suggested-model"),
            "unknown suggested_model must omit the flag: {c:?}"
        );
    }
}
