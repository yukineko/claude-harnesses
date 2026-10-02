// Reproduction tests for backlog items audited in shard s10-small-b.
// Each test is RED while its backlog item is an open defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn home() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("gauge-s10-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gauge"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("GAUGE_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let o = child.wait_with_output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
        String::from_utf8_lossy(&o.stderr).trim().to_string(),
    )
}

/// Records session `sid` (1M opus input tokens, one assistant turn dated
/// 2026-09-24) through the real `record` hook.
fn record_session(home: &Path, sid: &str) {
    let tp = home.join(format!("{sid}.jsonl"));
    std::fs::write(
        &tp,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n",
    )
    .unwrap();
    let payload = format!(
        "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{sid}\",\"cwd\":\"{}\",\"transcript_path\":\"{}\"}}",
        home.display(),
        tp.display()
    );
    let (rc, _, _) = run(home, &["record"], &payload);
    assert_eq!(rc, 0);
    assert!(
        home.join(".gauge/store/sessions")
            .join(format!("{sid}.json"))
            .exists(),
        "apparatus: record hook did not write the session record"
    );
}

fn cost_of(home: &Path, sid: &str) -> f64 {
    let (rc, out, err) = run(home, &["session", "--json", "--session", sid], "");
    assert_eq!(rc, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    v["cost_usd"]
        .as_f64()
        .unwrap_or_else(|| panic!("no cost_usd: {out}"))
}

/// 00880b3d (P3+P4): a malformed pricing override must never become a
/// $0.00 override that beats the built-in table. Controls pin the two
/// legitimate cases (no override -> built-in; complete override -> override).
#[test]
#[ignore = "backlog 00880b3d: open defect, remove ignore when fixed"]
fn backlog_00880b3d_malformed_pricing_override_is_not_a_zero_dollar_override() {
    // Control 1: no override.
    let h1 = home();
    record_session(&h1, "s1");
    let builtin = cost_of(&h1, "s1");
    assert!(
        builtin > 0.0,
        "apparatus: built-in price must be > 0, got {builtin}"
    );

    // Control 2: complete override applies.
    let h2 = home();
    record_session(&h2, "s1");
    std::fs::write(
        h2.join("gauge.toml"),
        "[[pricing]]\npattern = \"opus\"\ninput = 15.0\noutput = 75.0\n",
    )
    .unwrap();
    let full = cost_of(&h2, "s1");
    assert!(
        (full - 15.0).abs() < 1e-9,
        "apparatus: complete override should give 15.0, got {full}"
    );

    // Probe A: one-character typo in the field name.
    let h3 = home();
    record_session(&h3, "s1");
    std::fs::write(
        h3.join("gauge.toml"),
        "[[pricing]]\npattern = \"opus\"\ninputt = 15.0\noutput = 75.0\n",
    )
    .unwrap();
    let (rc, out, _) = run(&h3, &["session", "--json", "--session", "s1"], "");
    let typo_cost = serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v["cost_usd"].as_f64());
    // Probe B: rate fields omitted entirely.
    let h4 = home();
    record_session(&h4, "s1");
    std::fs::write(h4.join("gauge.toml"), "[[pricing]]\npattern = \"opus\"\n").unwrap();
    let (rc4, out4, _) = run(&h4, &["session", "--json", "--session", "s1"], "");
    let omitted_cost = serde_json::from_str::<serde_json::Value>(&out4)
        .ok()
        .and_then(|v| v["cost_usd"].as_f64());
    eprintln!("typo: rc={rc} cost={typo_cost:?}; omitted: rc={rc4} cost={omitted_cost:?}");
    assert!(
        rc != 0 || typo_cost.is_some_and(|c| c > 0.0),
        "typo'd pricing field produced cost {typo_cost:?} with rc={rc} (silent $0 override)"
    );
    assert!(
        rc4 != 0 || omitted_cost.is_some_and(|c| c > 0.0),
        "pricing row with no rates produced cost {omitted_cost:?} with rc={rc4} (silent $0 override)"
    );
}

/// a2d3d195 (P6): "no such session" and "a record exists but cannot be read"
/// must be distinguishable in `session --json`.
#[test]
#[ignore = "backlog a2d3d195: open defect, remove ignore when fixed"]
fn backlog_a2d3d195_session_json_absent_vs_undecodable_record_differ() {
    let h = home();
    record_session(&h, "s1");
    // Control a: a healthy record yields an object.
    let (rc_a, out_a, _) = run(&h, &["session", "--json", "--session", "s1"], "");
    assert_eq!(rc_a, 0);
    assert!(out_a.starts_with('{'), "control a: {out_a}");
    // b: no such session.
    let (rc_b, out_b, _) = run(&h, &["session", "--json", "--session", "nope"], "");
    // c: record exists but is undecodable (required field `cwd` removed).
    let rec_path = h.join(".gauge/store/sessions/s1.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("cwd");
    std::fs::write(&rec_path, serde_json::to_string(&v).unwrap()).unwrap();
    let (rc_c, out_c, _) = run(&h, &["session", "--json", "--session", "s1"], "");
    eprintln!("b: rc={rc_b} out={out_b:?}\nc: rc={rc_c} out={out_c:?}");
    assert!(
        rc_b != rc_c || out_b != out_c,
        "absent session and undecodable record are indistinguishable: rc={rc_b} out={out_b:?}"
    );
}

/// a2d3d195 (P7): `subagents --json` for a session with no transcript at all
/// vs a session whose transcript exists and simply has no sub-agents.
#[test]
#[ignore = "backlog a2d3d195: open defect, remove ignore when fixed"]
fn backlog_a2d3d195_subagents_json_no_transcript_vs_no_subagents_differ() {
    let h = home();
    // No ~/.claude/projects at all.
    let (rc_a, out_a, _) = run(&h, &["subagents", "--json", "--session", "ghost"], "");
    // Transcript exists, zero sub-agents.
    let proj = h.join(".claude/projects/p1");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("real.jsonl"),
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-24T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n",
    )
    .unwrap();
    let (rc_b, out_b, _) = run(&h, &["subagents", "--json", "--session", "real"], "");
    eprintln!("no transcript: rc={rc_a} out={out_a:?}\nno subagents: rc={rc_b} out={out_b:?}");
    assert!(
        rc_a != rc_b || out_a != out_b,
        "'could not find/read the transcript' and 'no sub-agents' are byte-identical: rc={rc_a} out={out_a:?}"
    );
}

/// eb26e14e (P9): `report --since` must not silently drop a record whose
/// timestamp cannot be read (last_ts missing).
#[test]
#[ignore = "backlog eb26e14e: open defect, remove ignore when fixed"]
fn backlog_eb26e14e_report_since_does_not_silently_drop_record_without_last_ts() {
    let h = home();
    record_session(&h, "s1");
    // Control: with last_ts present, --since 2026-01-01 includes the session.
    let (_, ctl, _) = run(&h, &["report", "--since", "2026-01-01"], "");
    assert!(
        ctl.contains("s1") || ctl.contains("opus"),
        "apparatus control: {ctl}"
    );
    let rec_path = h.join(".gauge/store/sessions/s1.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("last_ts");
    std::fs::write(&rec_path, serde_json::to_string(&v).unwrap()).unwrap();
    let (rc, out, err) = run(&h, &["report", "--since", "2026-01-01"], "");
    eprintln!("rc={rc}\nout={out}\nerr={err}");
    let mentions_session = out.contains("opus") || out.contains("s1");
    let discloses = out.to_lowercase().contains("unknown")
        || err.to_lowercase().contains("unknown")
        || err.to_lowercase().contains("timestamp")
        || out.to_lowercase().contains("timestamp");
    assert!(
        mentions_session || discloses || rc != 0,
        "record without last_ts vanished from --since totals with no diagnostic"
    );
}

/// c444cd8e (P8): `Path::exists()` maps "could not stat" (EACCES) to false, so a
/// config file that exists but whose directory cannot be traversed is reported
/// as "no config file". (P10, current_dir() failure -> ".", could NOT be
/// observed here: see the audit note on that item.)
#[cfg(unix)]
#[test]
#[ignore = "backlog c444cd8e: open defect, remove ignore when fixed"]
fn backlog_c444cd8e_status_does_not_claim_no_config_when_home_config_is_untraversable() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    let proj = h.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let gdir = h.join(".gauge");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::write(
        gdir.join("config.toml"),
        "[[pricing]]\npattern = \"opus\"\ninput = 15.0\noutput = 75.0\n",
    )
    .unwrap();
    // Control: readable -> status names the config file.
    let (_, ctl, _) = run(&proj, &["status"], "");
    let mut ctl_cmd = Command::new(env!("CARGO_BIN_EXE_gauge"));
    ctl_cmd.arg("status").current_dir(&proj).env("HOME", &h);
    let ctl_out = String::from_utf8_lossy(&ctl_cmd.output().unwrap().stdout).into_owned();
    assert!(
        ctl_out.contains("config.toml"),
        "apparatus control: {ctl_out} / {ctl}"
    );

    std::fs::set_permissions(&gdir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::metadata(gdir.join("config.toml")).is_err();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gauge"));
    cmd.arg("status").current_dir(&proj).env("HOME", &h);
    let out = String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned();
    std::fs::set_permissions(&gdir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(denied, "apparatus: mode 000 did not deny this uid (root?)");
    eprintln!("{out}");
    assert!(
        !out.contains("no config file"),
        "status says 'no config file' although {}/config.toml exists but cannot be stat'ed:\n{out}",
        gdir.display()
    );
}
