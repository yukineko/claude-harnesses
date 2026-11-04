//! reports.jsonl shape and secret removal (CONTRACT.md §8); daily-task stanza (§10).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;

const TOKEN: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop0123456789";

#[test]
fn report_rows_have_target_rule_verdict_time() {
    let mut env = Env::new();
    let o = env.audit_one(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let rows = env.reports();
    for r in ["B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9"] {
        assert!(
            rows.iter().any(|x| x["target"] == "web1" && x["rule"] == r),
            "no report row for {r}: {rows:?}"
        );
    }
    for row in &rows {
        assert!(row["target"].is_string(), "{row}");
        assert!(row["rule"].is_string(), "{row}");
        let v = row["verdict"].as_str().unwrap_or("");
        assert!(
            ["clean", "not_applicable", "violation", "undetermined"].contains(&v),
            "{row}"
        );
        assert_eq!(row["time"], NOW, "{row}");
    }
    let b7 = rows.iter().find(|x| x["rule"] == "B7").unwrap();
    assert_eq!(b7["apt_index_time"], NOW - DAY, "{b7}");
}

#[test]
fn report_appends_across_runs() {
    let mut env = Env::new();
    env.audit_one(&clean_host());
    let n1 = env.reports().len();
    assert!(n1 >= 9, "first run rows: {n1}");
    let o = env.run(&["audit", "--json"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert_eq!(
        env.reports().len(),
        2 * n1,
        "reports.jsonl must be appended, not replaced"
    );
}

#[test]
fn begin_lines_never_appear() {
    let mut env = Env::new();
    let mut h = clean_host();
    h.insert(
        W10.into(),
        ok("APT::Periodic::Unattended-Upgrade \"1\";\n-----BEGIN OPENSSH PRIVATE KEY-----\nAPT::Periodic::Update-Package-Lists \"1\";\n"),
    );
    let o = env.audit_one(&h);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let f = finding(&o.findings(), "web1", "B6");
    assert!(
        evidence_text(&f).contains("APT::Periodic::Unattended-Upgrade"),
        "B6 evidence must include W10 APT::Periodic lines: {f}"
    );
    for (what, s) in [
        ("stdout", &o.stdout),
        ("stderr", &o.stderr),
        ("report", &env.reports_raw()),
    ] {
        assert!(
            !s.contains("-----BEGIN"),
            "-----BEGIN leaked into {what}: {s}"
        );
    }
}

#[test]
fn long_tokens_are_redacted_in_place() {
    let mut env = Env::new();
    let mut h = clean_host();
    h.insert(
        W10.into(),
        ok(&format!(
            "APT::Periodic::Unattended-Upgrade \"0\"; // key: {TOKEN}\n"
        )),
    );
    let o = env.audit_one(&h);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let f = finding(&o.findings(), "web1", "B6");
    assert_eq!(f["verdict"], "violation", "{f}");
    let ev = evidence_text(&f);
    assert!(
        ev.contains("APT::Periodic::Unattended-Upgrade \"0\"; // key: <redacted>"),
        "line kept, token replaced in place: {ev}"
    );
    let rows = env.reports();
    let b6 = rows.iter().find(|r| r["rule"] == "B6").unwrap();
    assert!(evidence_text(b6).contains("key: <redacted>"), "{b6}");
    for (what, s) in [("stdout", &o.stdout), ("report", &env.reports_raw())] {
        assert!(!s.contains(TOKEN), "token leaked into {what}");
    }
}

#[test]
fn evidence_entries_are_capped_at_512_bytes() {
    let mut env = Env::new();
    let mut h = clean_host();
    let long = format!(
        "APT::Periodic::Unattended-Upgrade \"0\"; // {}\n",
        "x ".repeat(800)
    );
    h.insert(W10.into(), ok(&long));
    let o = env.audit_one(&h);
    let f = finding(&o.findings(), "web1", "B6");
    let ev = f["evidence"].as_array().unwrap_or_else(|| panic!("{f}"));
    assert!(!ev.is_empty(), "{f}");
    for row in env.reports() {
        for e in row["evidence"].as_array().into_iter().flatten() {
            assert!(
                e.as_str().unwrap().len() <= 512,
                "evidence > 512 bytes: {row}"
            );
        }
    }
    for e in ev {
        assert!(e.as_str().unwrap().len() <= 512, "evidence > 512 bytes");
    }
}

#[test]
fn aws_credentials_are_never_recorded() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[sgr("sgr-0v4ssh", "tcp", 22, 22, Some("0.0.0.0/0"), None)]);
    let key = "AKIAIOSFODNN7EXAMPLE";
    let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    let session = "FwoGZXIvYXdzEXAMPLESESSIONTOKENvalue";
    let o = env.run_with(
        &["aws", "--json"],
        &[
            ("AWS_ACCESS_KEY_ID", key),
            ("AWS_SECRET_ACCESS_KEY", secret),
            ("AWS_SESSION_TOKEN", session),
        ],
    );
    assert_eq!(o.code, 1, "{}", o.dbg());
    let rows = env.reports();
    assert!(
        rows.iter()
            .any(|r| r["rule"] == "A1" && r["verdict"] == "violation"),
        "{rows:?}"
    );
    for (what, s) in [
        ("stdout", &o.stdout),
        ("stderr", &o.stderr),
        ("report", &env.reports_raw()),
    ] {
        for c in [key, secret, session] {
            assert!(!s.contains(c), "AWS credential leaked into {what}");
        }
    }
}

#[test]
fn daily_task_print_runs_audit_local_and_aws() {
    let env = Env::new();
    let o = env.run(&["daily-task", "--print"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let t: toml::Value = toml::from_str(&o.stdout)
        .unwrap_or_else(|e| panic!("stanza is not TOML ({e}): {}", o.stdout));
    let tasks = t["task"]
        .as_array()
        .unwrap_or_else(|| panic!("no [[task]]: {}", o.stdout));
    let cmds: Vec<String> = tasks
        .iter()
        .map(|x| {
            assert!(x["name"].is_str(), "task without name: {x}");
            x["command"].as_str().unwrap().to_string()
        })
        .collect();
    let all = cmds.join("\n");
    for need in ["hostaudit audit", "hostaudit local", "hostaudit aws"] {
        assert!(all.contains(need), "missing `{need}` in {cmds:?}");
    }
    assert!(
        !all.contains("apply"),
        "daily task must never apply: {cmds:?}"
    );
}
