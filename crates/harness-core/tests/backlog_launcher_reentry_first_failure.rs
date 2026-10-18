#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Missing-binary path of the Stop-gate launchers (`crates/<gate>/bin/<gate>`):
//! a gate's FIRST failure in a session must block even when the Stop payload
//! carries `stop_hook_active: true`.
//!
//! `stop_hook_active` is set whenever ANY Stop hook blocked the previous stop.
//! The launchers treat that flag as "this gate already surfaced its block once"
//! and allow the stop. So when another gate blocked first, this gate's missing
//! binary is never surfaced at all: the gate did not run and the stop is
//! allowed. CLAUDE.md section 1 permits only a SECOND CONSECUTIVE failure of the
//! SAME gate to fall through to allow; `harness_core::gate::run::run_guarded`
//! implements that with a per-gate, per-session marker.
//!
//! The test drives each real launcher script, copied into an empty temp dir so
//! no `<gate>-<os>-<arch>` sibling exists, with HOME and every other env var
//! isolated. The session id is fresh, so this gate has no prior failure in it.
//! A control run with `stop_hook_active: false` proves each launcher really
//! takes its missing-binary block path (anti-vacuity).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// (crate dir, Stop-hook subcommand as declared in that crate's hooks.json)
const STOP_GATES: &[(&str, &str)] = &[
    ("donegate", "gate"),
    ("reviewgate", "review"),
    ("propguard", "check"),
    ("tdd", "gate"),
    ("budgetguard", "gate"),
    ("autoflow", "stop"),
    ("ctxrot", "stop"),
];

fn launcher_src(gate: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(gate)
        .join("bin")
        .join(gate)
}

fn run_launcher(gate: &str, sub: &str, payload: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("mkdir bin");
    let home = dir.path().join("home");
    std::fs::create_dir(&home).expect("mkdir home");
    let launcher = bin.join(gate);
    std::fs::copy(launcher_src(gate), &launcher).expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .arg(sub)
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("TMPDIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn is_block(r: &Run) -> bool {
    r.code == 0 && r.stdout.contains(r#""decision":"block""#)
}

#[test]
fn first_missing_binary_failure_blocks_even_on_a_stop_hook_active_reentry() {
    let mut failures = Vec::new();
    for (i, (gate, sub)) in STOP_GATES.iter().enumerate() {
        assert!(
            launcher_src(gate).is_file(),
            "launcher missing: {}",
            launcher_src(gate).display()
        );

        // Control: a non-re-entry first stop takes the missing-binary block path.
        let control = run_launcher(
            gate,
            sub,
            &format!(r#"{{"session_id":"ctl-{gate}-{i}","stop_hook_active":false}}"#),
        );
        assert!(
            is_block(&control),
            "control failed, launcher did not take its missing-binary block path for {gate} {sub}: {control:?}"
        );

        // Defect probe: fresh session, this gate has never failed in it, but
        // another gate blocked the previous stop so stop_hook_active is true.
        let session = format!("fresh-{gate}-{i}-{}", std::process::id());
        let r = run_launcher(
            gate,
            sub,
            &format!(r#"{{"session_id":"{session}","stop_hook_active":true}}"#),
        );
        if !is_block(&r) {
            failures.push(format!(
                "{gate} {sub}: first failure of this gate in session {session} was allowed \
                 (code={}, stdout={:?}, stderr={:?})",
                r.code,
                r.stdout.trim(),
                r.stderr.trim()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a gate's FIRST missing-binary failure must block; only a second consecutive \
         failure of the same gate may allow:\n{}",
        failures.join("\n")
    );
}
