//! ssh whitelist + baseline B1–B4, B6–B9 (CONTRACT.md §4.3–4.5). B5 lives in osv_freshness.rs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use std::collections::BTreeMap;

fn with(over: &[(&str, Resp)]) -> BTreeMap<String, Resp> {
    let mut m = clean_host();
    for (k, r) in over {
        m.insert((*k).to_string(), r.clone());
    }
    m
}

/// Audit one host with overrides; return (exit code, findings).
fn audit(over: &[(&str, Resp)]) -> (Out, Vec<serde_json::Value>) {
    let mut env = Env::new();
    let o = env.audit_one(&with(over));
    let fs = o.findings();
    (o, fs)
}

fn v(over: &[(&str, Resp)], rule: &str) -> String {
    let (_, fs) = audit(over);
    verdict(&fs, "web1", rule)
}

// ---------- whitelist / argv ----------

#[test]
fn only_whitelisted_commands_reach_ssh_with_batchmode_and_timeout() {
    let mut env = Env::new();
    let o = env.audit_one(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let log = env.ssh_log();
    assert!(!log.is_empty(), "no ssh invocation logged");
    let mut seen = std::collections::BTreeSet::new();
    for argv in &log {
        let cmd = argv.last().unwrap().as_str();
        assert!(
            is_whitelisted(cmd),
            "non-whitelisted remote command: {cmd:?}"
        );
        assert!(cmd.starts_with("LC_ALL=C ") || cmd.starts_with("sudo -n LC_ALL=C "));
        assert_eq!(
            cmd.starts_with("sudo"),
            is_sudo_cmd(cmd),
            "sudo only for [sudo] commands: {cmd:?}"
        );
        let pair = |k: &str| argv.windows(2).any(|w| w[0] == "-o" && w[1] == k);
        assert!(pair("BatchMode=yes"), "BatchMode=yes missing: {argv:?}");
        assert!(
            pair("ConnectTimeout=10"),
            "ConnectTimeout=10 missing: {argv:?}"
        );
        seen.insert(cmd.to_string());
    }
    for w in WHITELIST {
        assert!(seen.contains(w), "whitelist command never issued: {w}");
    }
}

#[test]
fn connect_timeout_is_clamped_from_config() {
    for (cfg, want) in [("0", "ConnectTimeout=1"), ("999", "ConnectTimeout=60")] {
        let mut env = Env::new();
        write(
            &env.hostaudit_dir().join("config.toml"),
            &format!("ssh_connect_timeout_secs = {cfg}\n"),
        );
        let o = env.audit_one(&clean_host());
        assert_eq!(o.code, 0, "{}", o.dbg());
        let log = env.ssh_log();
        assert!(!log.is_empty());
        for argv in &log {
            assert!(
                argv.windows(2).any(|w| w[0] == "-o" && w[1] == want),
                "cfg {cfg}: want {want} in {argv:?}"
            );
        }
    }
}

#[test]
fn every_rule_reported_once_and_clean_host_exits_zero() {
    let (o, fs) = audit(&[]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    for r in ["B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8"] {
        assert_eq!(verdict(&fs, "web1", r), "clean", "{r}");
    }
    assert_eq!(verdict(&fs, "web1", "B9"), "not_applicable");
}

// ---------- undetermined ----------

#[test]
fn ssh_connection_failure_is_undetermined_never_clean() {
    let all_down: Vec<(&str, Resp)> = WHITELIST.iter().map(|w| (*w, ssh_down())).collect();
    let (o, fs) = audit(&all_down);
    assert_eq!(o.code, 1, "{}", o.dbg());
    for r in ["B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9"] {
        let f = finding(&fs, "web1", r);
        assert_eq!(f["verdict"], "undetermined", "{r}: {f}");
        assert!(
            f["reason"].as_str().is_some_and(|s| !s.is_empty()),
            "{r}: undetermined needs a reason: {f}"
        );
    }
}

#[test]
fn sudo_failure_is_undetermined() {
    let (o, fs) = audit(&[(W4, sudo_fail())]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(verdict(&fs, "web1", "B1"), "undetermined");
    assert_eq!(verdict(&fs, "web1", "B2"), "undetermined");
    assert_eq!(verdict(&fs, "web1", "B4"), "clean");
}

#[test]
fn unexpected_nonzero_is_undetermined() {
    let (o, fs) = audit(&[(W3, fail(2, "ss: unexpected failure\n"))]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(verdict(&fs, "web1", "B4"), "undetermined");
    assert_eq!(
        verdict(&fs, "web1", "B9"),
        "undetermined",
        "W3 failed: 443 unknown, not N/A"
    );
}

// ---------- B1 / B2 ----------

#[test]
fn b1_permitrootlogin_yes_is_violation() {
    let (o, fs) = audit(&[(W4, ok("permitrootlogin yes\npasswordauthentication no\n"))]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let f = finding(&fs, "web1", "B1");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("permitrootlogin yes"), "{f}");
    assert!(f["fix"].as_str().unwrap_or("").contains("sshd -t"), "{f}");
}

#[test]
fn b1_permitrootlogin_no_is_clean() {
    assert_eq!(
        v(
            &[(W4, ok("permitrootlogin no\npasswordauthentication no\n"))],
            "B1"
        ),
        "clean"
    );
}

#[test]
fn b1_missing_key_is_undetermined() {
    assert_eq!(
        v(&[(W4, ok("passwordauthentication no\n"))], "B1"),
        "undetermined"
    );
}

#[test]
fn b2_passwordauthentication_yes_is_violation() {
    let (_, fs) = audit(&[(W4, ok("permitrootlogin no\npasswordauthentication yes\n"))]);
    let f = finding(&fs, "web1", "B2");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        f["fix"]
            .as_str()
            .unwrap_or("")
            .contains("PasswordAuthentication no"),
        "{f}"
    );
}

#[test]
fn b2_passwordauthentication_no_is_clean() {
    assert_eq!(v(&[], "B2"), "clean");
}

// ---------- B3 ----------

const NFT_DROP: &str = "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy drop;\n\t}\n}\n";

#[test]
fn b3_no_ufw_and_no_nft_drop_is_violation() {
    let (_, fs) = audit(&[(W5, ok("Status: inactive\n")), (W6, ok(""))]);
    let f = finding(&fs, "web1", "B3");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        f["fix"].as_str().unwrap_or("").contains("ufw enable"),
        "{f}"
    );
}

#[test]
fn b3_ufw_active_is_clean() {
    assert_eq!(v(&[], "B3"), "clean");
}

#[test]
fn b3_nft_input_policy_drop_is_clean() {
    assert_eq!(
        v(&[(W5, ok("Status: inactive\n")), (W6, ok(NFT_DROP))], "B3"),
        "clean"
    );
}

#[test]
fn b3_one_side_undetermined_is_undetermined() {
    assert_eq!(
        v(&[(W5, sudo_fail()), (W6, ok(NFT_DROP))], "B3"),
        "undetermined",
        "ufw unknown + nft drop"
    );
    assert_eq!(
        v(&[(W6, sudo_fail())], "B3"),
        "undetermined",
        "ufw active + nft unknown"
    );
}

// ---------- B4 ----------

#[test]
fn b4_db_ports_on_wildcard_addresses_are_violation() {
    for line in [
        "LISTEN 0 4096 0.0.0.0:3306 0.0.0.0:*",
        "LISTEN 0 4096 [::]:5432 [::]:*",
        "LISTEN 0 511 *:6379 *:*",
        "LISTEN 0 4096 0.0.0.0:27017 0.0.0.0:*",
    ] {
        let (_, fs) = audit(&[(
            W3,
            ok(&format!("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\n{line}\n")),
        )]);
        let f = finding(&fs, "web1", "B4");
        assert_eq!(f["verdict"], "violation", "{line}: {f}");
        assert!(
            evidence_text(&f).contains(line.split_whitespace().nth(3).unwrap()),
            "{f}"
        );
        assert!(f["fix"].as_str().unwrap_or("").contains("127.0.0.1"), "{f}");
    }
}

#[test]
fn b4_db_port_on_loopback_is_clean() {
    assert_eq!(v(&[], "B4"), "clean");
}

// ---------- B6 ----------

#[test]
fn b6_disabled_unit_is_observed_violation() {
    for state in ["disabled", "masked", "not-found"] {
        let r = Resp {
            out: format!("{state}\n"),
            err: String::new(),
            code: 1,
        };
        let (_, fs) = audit(&[(W9, r)]);
        let f = finding(&fs, "web1", "B6");
        assert_eq!(f["verdict"], "violation", "W9 {state}: {f}");
        assert!(
            f["fix"].as_str().unwrap_or("").contains("dpkg-reconfigure"),
            "{f}"
        );
    }
}

#[test]
fn b6_missing_20auto_upgrades_is_observed_violation() {
    let r = fail(
        1,
        "cat: /etc/apt/apt.conf.d/20auto-upgrades: No such file or directory\n",
    );
    assert_eq!(v(&[(W10, r)], "B6"), "violation");
}

#[test]
fn b6_unattended_upgrade_zero_is_violation() {
    let r = ok(
        "APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"0\";\n",
    );
    assert_eq!(v(&[(W10, r)], "B6"), "violation");
}

#[test]
fn b6_disabled_and_missing_file_is_violation_not_undetermined() {
    let w9 = Resp {
        out: "disabled\n".into(),
        err: String::new(),
        code: 1,
    };
    let w10 = fail(
        1,
        "cat: /etc/apt/apt.conf.d/20auto-upgrades: No such file or directory\n",
    );
    assert_eq!(v(&[(W9, w9), (W10, w10)], "B6"), "violation");
}

#[test]
fn b6_clean_and_unknown_unit_state_is_undetermined() {
    assert_eq!(v(&[], "B6"), "clean");
    assert_eq!(v(&[(W9, ssh_down())], "B6"), "undetermined");
}

// ---------- B7 ----------

#[test]
fn b7_stale_apt_index_is_violation() {
    let (_, fs) = audit(&[(W7A, absent()), (W7B, ok(&format!("{}\n", NOW - 8 * DAY)))]);
    let f = finding(&fs, "web1", "B7");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        f["fix"].as_str().unwrap_or("").contains("apt update"),
        "{f}"
    );
    assert_eq!(f["apt_index_time"], NOW - 8 * DAY, "{f}");
}

#[test]
fn b7_uses_max_of_present_stamps() {
    let (_, fs) = audit(&[
        (W7A, ok(&format!("{}\n", NOW - 30 * DAY))),
        (W7B, ok(&format!("{}\n", NOW - DAY))),
    ]);
    let f = finding(&fs, "web1", "B7");
    assert_eq!(f["verdict"], "clean", "{f}");
    assert_eq!(f["apt_index_time"], NOW - DAY, "{f}");
}

#[test]
fn b7_both_stamps_absent_is_undetermined() {
    assert_eq!(v(&[(W7A, absent()), (W7B, absent())], "B7"), "undetermined");
}

// ---------- B8 ----------

#[test]
fn b8_old_reboot_required_is_violation() {
    let (_, fs) = audit(&[(W8, ok(&format!("{}\n", NOW - 8 * DAY)))]);
    let f = finding(&fs, "web1", "B8");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(f["fix"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
}

#[test]
fn b8_recent_or_absent_reboot_required_is_clean() {
    assert_eq!(v(&[(W8, ok(&format!("{}\n", NOW - DAY)))], "B8"), "clean");
    assert_eq!(v(&[], "B8"), "clean");
}

// ---------- B9 ----------

fn with_443() -> (&'static str, Resp) {
    (
        W3,
        ok("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\nLISTEN 0 511 0.0.0.0:443 0.0.0.0:*\n"),
    )
}

#[test]
fn b9_no_443_is_not_applicable_distinct_from_clean() {
    let mut env = Env::new();
    env.tls_probe("notAfter=Jun  1 00:00:00 2027 GMT\n", 0);
    let o = env.audit_one(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert_eq!(verdict(&o.findings(), "web1", "B9"), "not_applicable");
    assert!(env.tls_log().is_empty(), "probe must not run without 443");
}

#[test]
fn b9_cert_expiring_within_14_days_is_violation() {
    let mut env = Env::new();
    env.tls_probe("notAfter=Jan 20 00:00:00 2027 GMT\n", 0);
    let o = env.audit_one(&with(&[with_443()]));
    let f = finding(&o.findings(), "web1", "B9");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        f["fix"].as_str().unwrap_or("").contains("certbot renew"),
        "{f}"
    );
    let log = env.tls_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        log[0],
        vec!["10.0.0.1".to_string(), "443".to_string()],
        "probe argv"
    );
}

#[test]
fn b9_cert_far_from_expiry_is_clean() {
    let mut env = Env::new();
    env.tls_probe("notAfter=Jun  1 00:00:00 2027 GMT\n", 0);
    let o = env.audit_one(&with(&[with_443()]));
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert_eq!(verdict(&o.findings(), "web1", "B9"), "clean");
}

#[test]
fn b9_probe_failure_is_undetermined() {
    let mut env = Env::new();
    env.tls_probe("", 1);
    let o = env.audit_one(&with(&[with_443()]));
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(verdict(&o.findings(), "web1", "B9"), "undetermined");
}

// ---------- fixes ----------

#[test]
fn every_violation_has_nonempty_fix() {
    let mut env = Env::new();
    env.tls_probe("notAfter=Jan 20 00:00:00 2027 GMT\n", 0);
    let bad = with(&[
        (W4, ok("permitrootlogin yes\npasswordauthentication yes\n")),
        (W5, ok("Status: inactive\n")),
        (W6, ok("")),
        (
            W3,
            ok("LISTEN 0 4096 0.0.0.0:3306 0.0.0.0:*\nLISTEN 0 511 0.0.0.0:443 0.0.0.0:*\n"),
        ),
        (
            W9,
            Resp {
                out: "disabled\n".into(),
                err: String::new(),
                code: 1,
            },
        ),
        (W7A, absent()),
        (W7B, ok(&format!("{}\n", NOW - 9 * DAY))),
        (W8, ok(&format!("{}\n", NOW - 9 * DAY))),
    ]);
    let o = env.audit_one(&bad);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let fs = o.findings();
    let viol: Vec<_> = fs.iter().filter(|f| f["verdict"] == "violation").collect();
    assert!(viol.len() >= 8, "expected B1-B4,B6-B9 violations: {fs:?}");
    for f in viol {
        assert!(
            f["fix"].as_str().is_some_and(|s| !s.trim().is_empty()),
            "violation without fix: {f}"
        );
    }
}
