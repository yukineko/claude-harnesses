//! v1.1 canon §7: Windows detection (W1 non-zero + W0) and WN1..WN5 over OpenSSH +
//! fixed read-only PowerShell (CONTRACT.md §12.8).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn fix_of(f: &Value) -> String {
    f["fix"].as_str().unwrap_or("").to_string()
}

fn server_cycle(eol_days: i64, ext: Value) -> Value {
    json!({"cycle":"2022","latest":"10.0.20348","eol":ymd(NOW + eol_days*DAY),"extendedSupport":ext})
}

fn run(h: &BTreeMap<String, Resp>, feed: Option<Value>) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    if let Some(c) = feed {
        env.eol_feed("windows-server", &[c]);
    }
    let o = env.audit_one(h);
    let fs = o.findings();
    (env, o, fs)
}

fn supported() -> Option<Value> {
    Some(server_cycle(1500, json!(ymd(NOW + 3000 * DAY))))
}

fn win(over: &[(&str, Resp)]) -> BTreeMap<String, Resp> {
    let mut h = win_host();
    for (k, r) in over {
        h.insert((*k).to_string(), r.clone());
    }
    h
}

fn wn(over: &[(&str, Resp)], rule: &str) -> Value {
    let (_, _, fs) = run(&win(over), supported());
    finding(&fs, "web1", rule)
}

fn hotfix(days_ago: i64) -> Resp {
    ok(&format!(
        r#"{{"HotFixID":"KB5034439","InstalledOn":"{}T00:00:00"}}"#,
        ymd(NOW - days_ago * DAY)
    ))
}

// ---------------------------------------------------------------- detection

#[test]
fn windows_host_gets_wn_rules_and_linux_rules_are_not_applicable() {
    let (env, o, fs) = run(&win(&[]), supported());
    assert_eq!(o.code, 0, "{}", o.dbg());
    for r in ["WN1", "WN2", "WN3", "WN4", "WN5"] {
        assert_eq!(verdict(&fs, "web1", r), "clean", "{r}");
    }
    for r in [
        "B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9", "S1", "S2", "S3", "S5", "S6", "S7",
        "S8", "S9", "K1", "K2", "K3", "K4", "K5",
    ] {
        assert_eq!(
            verdict(&fs, "web1", r),
            "not_applicable",
            "{r} is a Linux rule: NotApplicable on Windows, not Undetermined"
        );
    }
    // Only the probe + the fixed PowerShell strings were run on the Windows host.
    let cmds = remote_cmds(&env);
    for c in &cmds {
        assert!(
            *c == W1 || *c == W0 || WN_CMDS.contains(&c.as_str()),
            "unexpected command on a Windows host: {c:?}"
        );
    }
    assert!(
        cmds.iter().any(|c| c == W0),
        "W0 must be issued after W1 failed: {cmds:?}"
    );
    for w in WN_CMDS {
        assert!(cmds.iter().any(|c| c == w), "never issued: {w}");
    }
}

#[test]
fn undeterminable_os_makes_every_rule_undetermined() {
    // W1 fails and W0 does not say Microsoft Windows.
    let mut h = BTreeMap::new();
    h.insert(
        W1.to_string(),
        fail(1, "cat: /etc/os-release: No such file or directory\n"),
    );
    h.insert(W0.to_string(), fail(127, "cmd: command not found\n"));
    let (_, o, fs) = run(&h, None);
    assert_eq!(o.code, 1, "{}", o.dbg());
    for r in all_rules() {
        let f = finding(&fs, "web1", r);
        assert_eq!(f["verdict"], "undetermined", "{r}: {f}");
        assert!(
            f["reason"].as_str().is_some_and(|s| !s.is_empty()),
            "{r}: {f}"
        );
    }
}

// ---------------------------------------------------------------- WN1

#[test]
fn wn1_supported_clean_extended_warning_expired_violation() {
    let f = wn(&[], "WN1");
    assert_eq!(f["verdict"], "clean", "{f}");

    let (_, o, fs) = run(
        &win(&[]),
        Some(server_cycle(-100, json!(ymd(NOW + 400 * DAY)))),
    );
    let f = finding(&fs, "web1", "WN1");
    assert_eq!(f["verdict"], "warning", "extended (ESU) period only: {f}");
    assert!(!fix_of(&f).is_empty(), "{f}");
    assert_eq!(o.code, 0, "{}", o.dbg());

    for ext in [json!(false), json!(ymd(NOW - 10 * DAY))] {
        let (_, o, fs) = run(&win(&[]), Some(server_cycle(-100, ext.clone())));
        let f = finding(&fs, "web1", "WN1");
        assert_eq!(f["verdict"], "violation", "ext={ext}: {f}");
        assert!(!fix_of(&f).is_empty());
        assert_eq!(o.code, 1);
    }
}

#[test]
fn wn1_undetermined_without_feed_match_or_parseable_output() {
    let (_, _, fs) = run(&win(&[]), None);
    assert_eq!(
        verdict(&fs, "web1", "WN1"),
        "undetermined",
        "no endoflife feed"
    );
    // feed has a cycle, but none matches this build
    let other = json!({"cycle":"2016","latest":"10.0.14393","eol":ymd(NOW-1000*DAY),"extendedSupport":false});
    let (_, _, fs) = run(&win(&[]), Some(other));
    assert_eq!(
        verdict(&fs, "web1", "WN1"),
        "undetermined",
        "no cycle for build 20348"
    );
    let f = wn(&[(WN1, ok("not json"))], "WN1");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    let f = wn(&[(WN1, ssh_down())], "WN1");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

// ---------------------------------------------------------------- WN2

#[test]
fn wn2_threshold_is_45_days() {
    for (days, want) in [(44, "clean"), (46, "violation")] {
        let f = wn(&[(WN2, hotfix(days))], "WN2");
        assert_eq!(f["verdict"], want, "{days} days ago: {f}");
    }
    let f = wn(&[(WN2, hotfix(60))], "WN2");
    assert!(fix_of(&f).contains("Windows Update"), "{f}");
}

#[test]
fn wn2_accepts_the_powershell5_date_form() {
    let ms = (NOW - 60 * DAY) * 1000;
    let r = ok(&format!(
        r#"{{"HotFixID":"KB1","InstalledOn":"\/Date({ms})\/"}}"#
    ));
    let f = wn(&[(WN2, r)], "WN2");
    assert_eq!(f["verdict"], "violation", "{f}");
}

#[test]
fn wn2_no_hotfix_or_unreadable_date_is_undetermined() {
    for r in [
        ok(""),
        ok("null"),
        ok(r#"{"HotFixID":"KB1","InstalledOn":null}"#),
        ok(r#"{"HotFixID":"KB1","InstalledOn":"yesterday-ish"}"#),
        ssh_down(),
    ] {
        let f = wn(&[(WN2, r.clone())], "WN2");
        assert_eq!(f["verdict"], "undetermined", "{r:?} -> {f}");
    }
}

// ---------------------------------------------------------------- WN3

#[test]
fn wn3_pending_reboot_ignored_for_over_seven_days_is_violation() {
    let f = wn(&[(WN3, ok("True\r\n")), (WN2, hotfix(10))], "WN3");
    assert_eq!(f["verdict"], "violation", "{f}");
    let f = wn(&[(WN3, ok("True\r\n")), (WN2, hotfix(3))], "WN3");
    assert_eq!(f["verdict"], "clean", "{f}");
    let f = wn(&[(WN3, ok("False\r\n")), (WN2, hotfix(30))], "WN3");
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn wn3_true_without_a_usable_wn2_date_is_undetermined() {
    let f = wn(&[(WN3, ok("True\r\n")), (WN2, ok(""))], "WN3");
    assert_eq!(
        f["verdict"], "undetermined",
        "cannot age the pending reboot: {f}"
    );
    let f = wn(&[(WN3, ok("maybe\r\n"))], "WN3");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    let f = wn(&[(WN3, ssh_down())], "WN3");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

// ---------------------------------------------------------------- WN4

#[test]
fn wn4_disabled_firewall_profile_is_violation_naming_the_profile() {
    let r = ok(
        r#"[{"Name":"Domain","Enabled":1},{"Name":"Private","Enabled":1},{"Name":"Public","Enabled":0}]"#,
    );
    let f = wn(&[(WN4, r)], "WN4");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("Public"), "{f}");
    assert!(
        fix_of(&f).contains("Set-NetFirewallProfile -Profile Public -Enabled True"),
        "{f}"
    );
    let f = wn(&[], "WN4");
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn wn4_empty_or_garbled_profile_list_is_undetermined() {
    for r in [ok("[]"), ok(""), ok("not json"), ssh_down()] {
        let f = wn(&[(WN4, r.clone())], "WN4");
        assert_eq!(
            f["verdict"], "undetermined",
            "{r:?}: an empty set is not a pass: {f}"
        );
    }
}

// ---------------------------------------------------------------- WN5

#[test]
fn wn5_rdp_without_nla_is_violation() {
    let f = wn(&[(WN5, ok(r#"{"UserAuthentication":0}"#))], "WN5");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(fix_of(&f).contains("ネットワークレベル認証"), "{f}");
    let f = wn(&[], "WN5");
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn wn5_missing_key_or_garbage_is_undetermined() {
    for r in [ok(r#"{"PSPath":"x"}"#), ok("garbage"), ok(""), ssh_down()] {
        let f = wn(&[(WN5, r.clone())], "WN5");
        assert_eq!(f["verdict"], "undetermined", "{r:?}: {f}");
    }
}
