//! Spec `sitecraft-capabilities` R12 — the completion-report condition in
//! SKILL.md (rulings D13, U2, U3).
//!
//! The SKILL.md bullet runs by default. The testbed bullets (D13 items 1–4
//! on `~/src/3d-pipeline-web` with the real Blender and Playwright Chromium)
//! are `d13_testbed_is_complete`, `#[ignore]`d because it needs this Mac's
//! testbed, real Blender and real state dir; run it with `-- --ignored`.
//! The `command` kind's fixture coverage lives in spec_r2 / spec_r4 / spec_r5.
//!
//! # Surface assumed
//!
//! * The SKILL.md completion report (Phase 11 section, and invariant 5 of
//!   「不変条件」) requires `sitecraft check capability` exit 0 for every
//!   relevant capability, in addition to the existing checks.
//! * Phase 11 says a probe's `feasible` alone is not completion (it mentions
//!   `probe` and `feasible`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use spec_common::*;

#[test]
fn phase_11_requires_check_capability_exit_0_for_relevant_capabilities() {
    let phase11 = section(&skill_text(), "### Phase 11");
    assert!(!phase11.is_empty(), "SKILL.md has no Phase 11 section");
    assert!(
        phase11.contains("sitecraft check capability"),
        "Phase 11 must require `sitecraft check capability`"
    );
    assert!(
        phase11.contains("relevant"),
        "Phase 11 must say which capabilities: the relevant ones"
    );
    assert!(phase11.contains("exit 0"), "Phase 11 must require exit 0");
}

#[test]
fn invariant_5_lists_check_capability() {
    let s = skill_text();
    let inv = section(&s, "## 不変条件");
    let five = inv
        .lines()
        .find(|l| l.starts_with("5."))
        .unwrap_or_else(|| panic!("invariant 5 not found in:\n{inv}"));
    assert!(
        five.contains("sitecraft check capability"),
        "invariant 5 (completion only after every check is exit 0) must include `sitecraft check capability`: {five}"
    );
}

/// D13 items 1–4 on the real testbed. Run by hand on this Mac:
/// `cargo test -p sitecraft --test spec_r12_skill_completion -- --ignored`.
/// Uses the caller's real `HOME` / state dir / trust store (D13 is about
/// the real environment), the real Blender at `/Applications/Blender.app`
/// and the testbed's own Playwright Chromium. It writes only what the
/// commands themselves write (capability results, check history, the
/// capability outputs D4 regenerates).
#[test]
#[ignore = "D13 testbed: needs ~/src/3d-pipeline-web, /Applications/Blender.app and Playwright Chromium (run by hand)"]
fn d13_testbed_is_complete() {
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    const BLENDER: &str = "/Applications/Blender.app/Contents/MacOS/Blender";
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
    let root = home.join("src/3d-pipeline-web");
    assert!(root.is_dir(), "testbed {} missing", root.display());
    let cmd = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_sitecraft"));
        c.current_dir(&root)
            .arg("--root")
            .arg(&root)
            .args(args)
            .env("CLAUDE_PLUGIN_ROOT", crate_dir())
            .env("SITECRAFT_BLENDER", BLENDER);
        c
    };
    let run = |args: &[&str]| run_deadline(cmd(args), None, Duration::from_secs(420));

    // 1. purpose recorded; decide check exit 0 with the W1 warning.
    let d: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("sitecraft/decisions.toml")).unwrap())
            .unwrap();
    assert!(
        d.get("purpose")
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .is_some_and(|t| !t.trim().is_empty()),
        "D13-1: [purpose] text must be recorded in the testbed"
    );
    let o = run(&["decide", "check"]);
    expect_code(&o, 0, "D13-1: decide check");
    assert!(
        o.stderr.contains("warning: W1:"),
        "D13-1: W1 warning\n{}",
        o.describe()
    );

    // 2. both builtin capabilities registered relevant; both probes feasible.
    for (name, kind) in [
        ("blender-headless", "builtin:blender-headless"),
        ("threejs-render", "builtin:threejs-render"),
    ] {
        let c = d
            .get("capabilities")
            .and_then(|cs| cs.get(name))
            .unwrap_or_else(|| panic!("D13-2: [capabilities.{name}] is not registered"));
        assert_eq!(
            c.get("relevance").and_then(|r| r.as_str()),
            Some("relevant"),
            "{name}: {c:?}"
        );
        assert_eq!(
            c.get("verify")
                .and_then(|v| v.get("kind"))
                .and_then(|k| k.as_str()),
            Some(kind),
            "{name}: {c:?}"
        );
        expect_code(
            &run(&["capability", "probe", name]),
            0,
            &format!("D13-2: probe {name}"),
        );
    }

    // 3. both real-machine checks clean.
    for name in ["blender-headless", "threejs-render"] {
        expect_code(
            &run(&["check", "capability", name]),
            0,
            &format!("D13-3: check capability {name}"),
        );
    }

    // 4. the Stop gate does not block in that state.
    let tmp = tempfile::tempdir().unwrap();
    let transcript = tmp.path().join("transcript.jsonl");
    write(
        &transcript,
        format!(
            "{}\n",
            serde_json::json!({"type": "user", "sessionId": "d13", "message": {"role": "user", "content": "report"}})
        ),
    );
    let session = format!("d13-{}", std::process::id());
    let payload = serde_json::json!({
        "session_id": session,
        "transcript_path": transcript.display().to_string(),
        "cwd": root.display().to_string(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let mut g = Command::new(env!("CARGO_BIN_EXE_sitecraft"));
    g.current_dir(&root)
        .arg("gate")
        .env("CLAUDE_PLUGIN_ROOT", crate_dir())
        .env("CLAUDE_CODE_SESSION_ID", &session);
    let o = run_deadline(g, Some(&payload), Duration::from_secs(600));
    assert_allow(&o, "D13-4: sitecraft gate on the testbed");
}

#[test]
fn phase_11_says_probe_feasible_is_not_completion() {
    let phase11 = section(&skill_text(), "### Phase 11");
    assert!(
        phase11.contains("probe") && phase11.contains("feasible"),
        "Phase 11 must say a probe's feasible alone is not completion"
    );
}
