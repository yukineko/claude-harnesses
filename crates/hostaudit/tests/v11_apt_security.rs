//! v1.1 canon §4: S1, S2, S3, S5, S6, S7 plus tool-not-installed / sudo handling
//! and command-injection safety of the dynamic whitelist commands
//! (CONTRACT.md §12.2, §12.5).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;
use std::collections::BTreeMap;

const SEC_INST: &str = "Inst libssl3 [3.0.2-0ubuntu1.10] (3.0.2-0ubuntu1.12 Ubuntu:22.04/jammy-updates, Ubuntu:22.04/jammy-security [amd64])\n";
const UPD_INST: &str = "Inst vim [2:8.2-1] (2:8.2-2 Ubuntu:22.04/jammy-updates [amd64])\n";

fn audit(h: &BTreeMap<String, Resp>) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    let o = env.audit_v11(h);
    let fs = o.findings();
    (env, o, fs)
}

fn host(over: &[(&str, Resp)]) -> BTreeMap<String, Resp> {
    let mut h = clean_host();
    for (k, r) in over {
        set(&mut h, k, r.clone());
    }
    h
}

fn v(over: &[(&str, Resp)], rule: &str) -> Value {
    let (_, _, fs) = audit(&host(over));
    finding(&fs, "web1", rule)
}

fn fix_of(f: &Value) -> String {
    f["fix"].as_str().unwrap_or("").to_string()
}

fn reason_of(f: &Value) -> String {
    f["reason"].as_str().unwrap_or("").to_string()
}

// ---------------------------------------------------------------- S1

#[test]
fn s1_pending_security_update_is_violation() {
    let f = v(&[(W14, ok(SEC_INST))], "S1");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("libssl3"), "{f}");
    let fix = fix_of(&f);
    assert!(fix.contains("sudo apt update && sudo apt upgrade"), "{fix}");
    assert!(
        fix.contains("libssl3"),
        "fix must enumerate the packages: {fix}"
    );
}

#[test]
fn s1_debian_security_origin_counts() {
    let mut env = Env::new();
    let mut h = debian_host();
    set(
        &mut h,
        W14,
        ok("Inst libc6 [2.36-9] (2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])\n"),
    );
    env.eol_feed("debian", &[cyc("12", 300)]);
    let o = env.audit_one(&h);
    assert_eq!(
        verdict(&o.findings(), "web1", "S1"),
        "violation",
        "{}",
        o.dbg()
    );
}

#[test]
fn s1_non_security_updates_are_clean() {
    let f = v(&[(W14, ok(UPD_INST))], "S1");
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn s1_conf_lines_are_not_pending_installs() {
    let f = v(
        &[(
            W14,
            ok("Conf libssl3 (3.0.2-0ubuntu1.12 Ubuntu:22.04/jammy-security [amd64])\n"),
        )],
        "S1",
    );
    assert_eq!(f["verdict"], "clean", "only `Inst` lines are pending: {f}");
}

#[test]
fn s1_unusable_apt_simulation_is_undetermined() {
    for r in [ssh_down(), fail(100, "E: Could not get lock\n")] {
        let f = v(&[(W14, r.clone())], "S1");
        assert_eq!(f["verdict"], "undetermined", "{r:?} -> {f}");
        assert!(!reason_of(&f).is_empty(), "{f}");
    }
}

#[test]
fn s1_stale_index_makes_the_count_a_lower_bound() {
    let stale = [
        (W7A, absent()),
        (W7B, ok(&format!("{}\n", NOW - 9 * DAY))),
        (W14, ok(SEC_INST)),
    ];
    let (_, _, fs) = audit(&host(&stale));
    assert_eq!(verdict(&fs, "web1", "B7"), "violation", "fixture sanity");
    let f = finding(&fs, "web1", "S1");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        all_text(&f).contains("下限"),
        "must say the count is a lower bound: {f}"
    );
}

// ---------------------------------------------------------------- S2

fn pro(pkgs: &str) -> Resp {
    ok(&format!(
        r#"{{"_schema_version":"0.1","packages":[{pkgs}]}}"#
    ))
}

#[test]
fn s2_esm_only_fix_without_esm_is_violation() {
    let r = pro(
        r#"{"package":"libssl3","service_name":"esm-infra","status":"pending_attach"},{"package":"vim","service_name":"main","status":"released"}"#,
    );
    let f = v(&[(W15, r)], "S2");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("libssl3"), "{f}");
    assert!(
        !evidence_text(&f).contains("vim"),
        "released pkg is not a finding: {f}"
    );
    assert!(fix_of(&f).contains("pro attach"), "{f}");
}

#[test]
fn s2_no_esm_dependency_is_clean() {
    let r = pro(
        r#"{"package":"libssl3","service_name":"esm-infra","status":"upgrade_available"},{"package":"vim","service_name":"main","status":"released"}"#,
    );
    assert_eq!(v(&[(W15, r)], "S2")["verdict"], "clean");
}

#[test]
fn s2_unexpected_schema_is_undetermined() {
    for r in [ok(r#"{"unexpected":true}"#), ok("not json at all"), ok("")] {
        let f = v(&[(W15, r.clone())], "S2");
        assert_eq!(f["verdict"], "undetermined", "{r:?} -> {f}");
    }
}

#[test]
fn s2_tool_missing_is_undetermined_with_install_command() {
    let f = v(&[(W15, notfound("pro"))], "S2");
    assert_eq!(f["verdict"], "undetermined", "not NotApplicable: {f}");
    assert!(
        reason_of(&f).contains("sudo apt install ubuntu-pro-client"),
        "{f}"
    );
}

#[test]
fn s2_ssh_failure_is_undetermined_not_a_missing_tool() {
    let f = v(&[(W15, ssh_down())], "S2");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(
        !reason_of(&f).contains("apt install"),
        "an ssh failure must not be reported as a missing tool: {f}"
    );
}

#[test]
fn s2_is_not_applicable_on_debian() {
    let mut env = Env::new();
    env.eol_feed("debian", &[cyc("12", 300)]);
    let o = env.audit_one(&debian_host());
    assert_eq!(
        verdict(&o.findings(), "web1", "S2"),
        "not_applicable",
        "{}",
        o.dbg()
    );
}

// ---------------------------------------------------------------- S3

fn needrestart(svc: &[&str], ksta: u8) -> Resp {
    let mut s = String::from("NEEDRESTART-VER: 3.5\nNEEDRESTART-KCUR: 5.15.0-100-generic\n");
    for x in svc {
        s.push_str(&format!("NEEDRESTART-SVC: {x}\n"));
    }
    s.push_str(&format!("NEEDRESTART-KSTA: {ksta}\n"));
    ok(&s)
}

#[test]
fn s3_stale_library_service_is_violation() {
    let f = v(
        &[(W16, needrestart(&["cron.service", "ssh.service"], 1))],
        "S3",
    );
    assert_eq!(f["verdict"], "violation", "{f}");
    let ev = evidence_text(&f);
    assert!(
        ev.contains("cron.service") && ev.contains("ssh.service"),
        "{f}"
    );
    let fix = fix_of(&f);
    assert!(fix.contains("systemctl restart"), "{fix}");
    assert!(fix.contains("cron.service"), "services enumerated: {fix}");
}

#[test]
fn s3_unknown_kernel_state_is_undetermined() {
    let f = v(&[(W16, needrestart(&[], 0))], "S3");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

#[test]
fn s3_sudo_password_required_is_undetermined() {
    let f = v(&[(W16, sudo_fail())], "S3");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(!reason_of(&f).is_empty());
}

#[test]
fn s3_tool_missing_is_undetermined_with_install_command() {
    let f = v(
        &[(W16, fail(1, "sudo: needrestart: command not found\n"))],
        "S3",
    );
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(
        reason_of(&f).contains("sudo apt install needrestart"),
        "{f}"
    );
}

#[test]
fn s3_is_clean_when_nothing_is_stale_and_kernel_ok() {
    assert_eq!(v(&[(W16, needrestart(&[], 1))], "S3")["verdict"], "clean");
}

// ---------------------------------------------------------------- S5

#[test]
fn s5_foreign_repository_is_warning_with_listing() {
    let h = host(&[
        (W18_LS, ok("deadsnakes.sources\n")),
        (
            "LC_ALL=C cat /etc/apt/sources.list.d/deadsnakes.sources",
            ok("Types: deb\nURIs: https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu\nSuites: jammy\nComponents: main\n"),
        ),
    ]);
    let (_, o, fs) = audit(&h);
    let f = finding(&fs, "web1", "S5");
    assert_eq!(f["verdict"], "warning", "{f}");
    assert!(
        evidence_text(&f).contains("ppa.launchpadcontent.net"),
        "{f}"
    );
    assert!(
        fix_of(&f).contains("deadsnakes.sources")
            || evidence_text(&f).contains("deadsnakes.sources"),
        "the file to review/delete must be named: {f}"
    );
    assert_eq!(o.code, 0, "{}", o.dbg());
}

#[test]
fn s5_official_mirrors_only_is_clean() {
    let src = "# comment\n\
deb http://ap-northeast-1.ec2.archive.ubuntu.com/ubuntu jammy main\n\
deb http://jp.archive.ubuntu.com/ubuntu jammy-updates main\n\
deb-src http://archive.ubuntu.com/ubuntu jammy main\n\
deb http://security.ubuntu.com/ubuntu jammy-security main\n\
deb http://ports.ubuntu.com/ubuntu-ports jammy main\n\
deb https://esm.ubuntu.com/apps/ubuntu jammy-apps-security main\n\
deb http://deb.debian.org/debian bookworm main\n\
deb http://security.debian.org/debian-security bookworm-security main\n";
    assert_eq!(v(&[(W18_SRC, ok(src))], "S5")["verdict"], "clean");
}

#[test]
fn s5_lookalike_hosts_are_not_official() {
    for url in [
        "http://archive.ubuntu.com.evil.example/ubuntu",
        "http://evilarchive.ubuntu.com/ubuntu",
        "http://archive.ubuntu.com@evil.example/ubuntu",
        "http://evil.example/archive.ubuntu.com/ubuntu",
    ] {
        let f = v(&[(W18_SRC, ok(&format!("deb {url} jammy main\n")))], "S5");
        assert_eq!(
            f["verdict"], "warning",
            "{url} must not pass as official: {f}"
        );
    }
}

#[test]
fn s5_commented_out_foreign_line_is_ignored() {
    let src = "deb http://archive.ubuntu.com/ubuntu jammy main\n# deb https://evil.example/apt stable main\n";
    assert_eq!(v(&[(W18_SRC, ok(src))], "S5")["verdict"], "clean");
}

#[test]
fn s5_missing_sources_list_is_observed_as_none() {
    let h = host(&[
        (
            W18_SRC,
            fail(1, "cat: /etc/apt/sources.list: No such file or directory\n"),
        ),
        (W18_LS, ok("ubuntu.sources\n")),
        (
            "LC_ALL=C cat /etc/apt/sources.list.d/ubuntu.sources",
            ok("Types: deb\nURIs: http://archive.ubuntu.com/ubuntu\nSuites: jammy\n"),
        ),
    ]);
    let (_, _, fs) = audit(&h);
    assert_eq!(verdict(&fs, "web1", "S5"), "clean");
}

#[test]
fn s5_unreadable_sources_is_undetermined() {
    assert_eq!(v(&[(W18_SRC, ssh_down())], "S5")["verdict"], "undetermined");
    // listed file whose cat fails (no fake response -> ssh 255)
    let f = v(&[(W18_LS, ok("extra.list\n"))], "S5");
    assert_eq!(
        f["verdict"], "undetermined",
        "a listed file we could not read: {f}"
    );
}

#[test]
fn s5_only_safe_source_file_names_are_read() {
    let h = host(&[
        (
            W18_LS,
            ok("ok.list\nbad;name.list\n$(id).list\nfoo.list.save\nREADME\n"),
        ),
        (
            "LC_ALL=C cat /etc/apt/sources.list.d/ok.list",
            ok("deb http://archive.ubuntu.com/ubuntu jammy universe\n"),
        ),
    ]);
    let (env, _, fs) = audit(&h);
    assert_eq!(verdict(&fs, "web1", "S5"), "clean");
    let cmds = remote_cmds(&env);
    assert!(
        cmds.iter()
            .any(|c| c == "LC_ALL=C cat /etc/apt/sources.list.d/ok.list"),
        "the safe file must be read: {cmds:?}"
    );
    for bad in ["bad;name", "$(id)", ".save", "README"] {
        assert!(
            !cmds.iter().any(|c| c.contains(bad)),
            "unsafe/unrelated name {bad:?} reached ssh: {cmds:?}"
        );
    }
}

// ---------------------------------------------------------------- S6

#[test]
fn s6_dpkg_audit_output_is_violation() {
    let f = v(
        &[(
            W19,
            ok("The following packages are only half configured:\n libfoo  broken\n"),
        )],
        "S6",
    );
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(fix_of(&f).contains("sudo dpkg --configure -a"), "{f}");
    assert!(evidence_text(&f).contains("libfoo"), "{f}");
}

#[test]
fn s6_empty_audit_is_clean_and_failure_is_undetermined() {
    assert_eq!(v(&[(W19, ok(""))], "S6")["verdict"], "clean");
    assert_eq!(v(&[(W19, ssh_down())], "S6")["verdict"], "undetermined");
    assert_eq!(
        v(
            &[(
                W19,
                fail(1, "dpkg: error: dpkg status database is locked\n")
            )],
            "S6"
        )["verdict"],
        "undetermined",
        "a failed dpkg --audit is not an empty (clean) audit"
    );
}

// ---------------------------------------------------------------- S7

#[test]
fn s7_hold_on_a_package_with_pending_security_update_is_violation() {
    let f = v(
        &[
            (
                W14,
                ok("Inst curl [7.81.0-1] (7.81.0-2 Ubuntu:22.04/jammy-security [amd64])\n"),
            ),
            (W20, ok("curl\nvim\n")),
        ],
        "S7",
    );
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(fix_of(&f).contains("apt-mark unhold curl"), "{f}");
    assert!(
        !fix_of(&f).contains("unhold vim"),
        "unrelated hold untouched: {f}"
    );
}

#[test]
fn s7_hold_on_a_b5_affected_package_is_violation() {
    let mut env = Env::new();
    let mut h = clean_host();
    set(&mut h, W2, ok("tar\t1.34-1\ttar\t1.34-1\n"));
    set(&mut h, W20, ok("tar\n"));
    env.one_selected_host(&h);
    env.osv_feed(&serde_json::json!([osv_rec(
        "UBUNTU-CVE-2024-0031",
        &[],
        Some(("Ubuntu", "low")),
        "tar",
        Some("9.9")
    )]));
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "B5"), "violation");
    let f = finding(&fs, "web1", "S7");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("tar"), "{f}");
}

#[test]
fn s7_unrelated_hold_is_clean_and_listed_as_evidence() {
    let f = v(&[(W20, ok("vim\n"))], "S7");
    assert_eq!(f["verdict"], "clean", "{f}");
    assert!(
        evidence_text(&f).contains("vim"),
        "hold list is evidence: {f}"
    );
}

#[test]
fn s7_no_holds_clean_and_failure_undetermined() {
    assert_eq!(v(&[(W20, ok(""))], "S7")["verdict"], "clean");
    assert_eq!(v(&[(W20, ssh_down())], "S7")["verdict"], "undetermined");
}

// ---------------------------------------------------------------- whitelist / injection

#[test]
fn every_issued_command_is_whitelisted_lc_all_and_sudo_only_where_declared() {
    let (env, o, _) = audit(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let cmds = remote_cmds(&env);
    for c in &cmds {
        assert!(is_whitelisted(c), "non-whitelisted: {c:?}");
        assert!(
            c.starts_with("LC_ALL=C ") || c.starts_with("sudo -n LC_ALL=C "),
            "LC_ALL=C prefix: {c:?}"
        );
        assert_eq!(
            c.starts_with("sudo"),
            is_sudo_cmd(c),
            "sudo only for [sudo]: {c:?}"
        );
    }
    // Liveness: the static v1.1 commands are really issued (not a vacuous pass).
    for need in [
        W12, W14, W15, W16, W17U, W18_SRC, W18_LS, W19, W20, W23, W24, W25,
    ] {
        assert!(cmds.iter().any(|c| c == need), "never issued: {need}");
    }
    for (_, c) in W21_CMDS {
        assert!(cmds.iter().any(|x| *x == w21(c)), "W21 never issued: {c}");
    }
    assert!(
        cmds.iter()
            .any(|c| c == "sudo -n LC_ALL=C readlink /proc/812/exe"),
        "W12b: {cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c == "LC_ALL=C dpkg -S /usr/sbin/sshd"),
        "W13: {cmds:?}"
    );
    // A Linux host never triggers the Windows probe (W1 succeeded).
    assert!(
        !cmds.iter().any(|c| c == W0 || c.starts_with("powershell")),
        "{cmds:?}"
    );
}

#[test]
fn hostile_listener_data_never_reaches_a_remote_command() {
    let w12 = "LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"a\",pid=$(id),fd=3))\n\
LISTEN 0 128 0.0.0.0:23 0.0.0.0:* users:((\"b\",pid=7;reboot,fd=3))\n\
LISTEN 0 128 0.0.0.0:101 0.0.0.0:* users:((\"c\",pid=101,fd=3))\n\
LISTEN 0 128 0.0.0.0:102 0.0.0.0:* users:((\"c\",pid=102,fd=3))\n\
LISTEN 0 128 0.0.0.0:103 0.0.0.0:* users:((\"c\",pid=103,fd=3))\n\
LISTEN 0 128 0.0.0.0:104 0.0.0.0:* users:((\"c\",pid=104,fd=3))\n\
LISTEN 0 128 0.0.0.0:105 0.0.0.0:* users:((\"c\",pid=105,fd=3))\n\
LISTEN 0 128 0.0.0.0:106 0.0.0.0:* users:((\"c\",pid=106,fd=3))\n\
LISTEN 0 128 0.0.0.0:107 0.0.0.0:* users:((\"c\",pid=107,fd=3))\n\
LISTEN 0 128 0.0.0.0:108 0.0.0.0:* users:((\"c\",pid=108,fd=3))\n";
    let bad = [
        (101, "/tmp/x;touch /tmp/pwned/node"),
        (102, "/tmp/a b/node"),
        (103, "/tmp/'q'/node"),
        (104, "relative/path/node"),
        (105, "/tmp/a|b/node"),
        (106, "/tmp/a&b/node"),
        (107, "/tmp/a$HOME`id`/node"),
    ];
    let mut over: Vec<(String, Resp)> = vec![(W12.to_string(), ok(w12))];
    for (pid, path) in bad {
        over.push((
            format!("sudo -n LC_ALL=C readlink /proc/{pid}/exe"),
            ok(&format!("{path}\n")),
        ));
    }
    over.push((
        "sudo -n LC_ALL=C readlink /proc/108/exe".into(),
        ok("/opt/good/bin/node\n"),
    ));
    over.push((
        "LC_ALL=C dpkg -S /opt/good/bin/node".into(),
        fail(
            1,
            "dpkg-query: no path found matching pattern /opt/good/bin/node\n",
        ),
    ));
    let mut h = clean_host();
    for (k, r) in over {
        h.insert(k, r);
    }
    let (env, _, _) = audit(&h);
    let cmds = remote_cmds(&env);
    let readlink = regex_free_readlink(&cmds);
    assert!(
        readlink.contains(&"sudo -n LC_ALL=C readlink /proc/108/exe".to_string()),
        "{cmds:?}"
    );
    for c in &cmds {
        if c.starts_with("sudo -n LC_ALL=C readlink /proc/") {
            let pid = c
                .trim_start_matches("sudo -n LC_ALL=C readlink /proc/")
                .trim_end_matches("/exe");
            assert!(
                !pid.is_empty() && pid.chars().all(|x| x.is_ascii_digit()),
                "non-numeric pid reached readlink: {c:?}"
            );
        }
        for evil in [
            "pwned",
            "$(",
            "7;reboot",
            "a b/",
            "'q'",
            "a|b",
            "a&b",
            "`id`",
            "relative/",
        ] {
            assert!(
                !c.contains(evil),
                "hostile data {evil:?} in remote command {c:?}"
            );
        }
    }
    // Liveness: the good path was used all the way to dpkg -S and W22.
    assert!(
        cmds.iter()
            .any(|c| c == "LC_ALL=C dpkg -S /opt/good/bin/node"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter()
            .any(|c| c == "LC_ALL=C /opt/good/bin/node --version"),
        "W22 for a non-dpkg process whose basename is a monitored product: {cmds:?}"
    );
}

fn regex_free_readlink(cmds: &[String]) -> Vec<String> {
    cmds.iter()
        .filter(|c| c.starts_with("sudo -n LC_ALL=C readlink /proc/"))
        .cloned()
        .collect()
}

#[test]
fn w22_is_not_run_for_unmonitored_or_dpkg_owned_executables() {
    let mut h = clean_host();
    set(
        &mut h,
        W12,
        ok("LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=812,fd=3))\nLISTEN 0 128 0.0.0.0:9 0.0.0.0:* users:((\"foo\",pid=900,fd=3))\n"),
    );
    set(
        &mut h,
        "sudo -n LC_ALL=C readlink /proc/900/exe",
        ok("/usr/local/bin/foo\n"),
    );
    set(
        &mut h,
        "LC_ALL=C dpkg -S /usr/local/bin/foo",
        fail(
            1,
            "dpkg-query: no path found matching pattern /usr/local/bin/foo\n",
        ),
    );
    let (env, _, _) = audit(&h);
    let cmds = remote_cmds(&env);
    // Liveness: the listener was really examined (W12b ran for pid 900).
    assert!(
        cmds.iter()
            .any(|c| c == "sudo -n LC_ALL=C readlink /proc/900/exe"),
        "W12b never issued: {cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.starts_with("LC_ALL=C /usr/local/bin/foo")
                || c.starts_with("LC_ALL=C /usr/sbin/sshd")),
        "executables are only run for monitored products that dpkg does not own: {cmds:?}"
    );
}

// ---------------------------------------------------------------- canon amendment 4322e061

#[test]
fn s7_hold_with_undetermined_s1_or_b5_is_undetermined() {
    // S1 (W14) unobservable, B5 clean: a hold exists, no violation can be shown -> Undetermined.
    let (_, _, fs) = audit(&host(&[(W20, ok("vim\n")), (W14, ssh_down())]));
    assert_eq!(verdict(&fs, "web1", "S1"), "undetermined", "fixture");
    let f = finding(&fs, "web1", "S7");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(!reason_of(&f).is_empty(), "{f}");
    // B5 undetermined (advisory feed unobtainable), S1 observed clean.
    let mut env = Env::new();
    env.one_selected_host(&host(&[(W20, ok("vim\n"))]));
    env.osv_broken();
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "B5"), "undetermined", "fixture");
    assert_eq!(verdict(&fs, "web1", "S7"), "undetermined", "{}", o.dbg());
}

#[test]
fn s7_violation_still_wins_over_an_undetermined_side() {
    // S1 shows curl pending while B5 is undetermined: the observed violation stands.
    let mut env = Env::new();
    env.one_selected_host(&host(&[
        (
            W14,
            ok("Inst curl [7.81.0-1] (7.81.0-2 Ubuntu:22.04/jammy-security [amd64])\n"),
        ),
        (W20, ok("curl\n")),
    ]));
    env.osv_broken();
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    assert_eq!(
        verdict(&o.findings(), "web1", "S7"),
        "violation",
        "{}",
        o.dbg()
    );
}

#[test]
fn s8_item_of_an_unsafe_executable_path_is_undetermined_and_never_sent() {
    let w12 = "LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:((\"node\",pid=900,fd=19))\n";
    let h = host(&[
        (W12, ok(w12)),
        (
            "sudo -n LC_ALL=C readlink /proc/900/exe",
            ok("/opt/no de/bin/node\n"),
        ),
    ]);
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    env.eol_feed("nodejs", &[cyc("25", 400)]);
    let o = env.audit_one(&h);
    let fs = o.findings();
    let f = finding(&fs, "web1", "S8");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(!reason_of(&f).is_empty(), "{f}");
    let cmds = remote_cmds(&env);
    assert!(
        cmds.iter()
            .any(|c| c == "sudo -n LC_ALL=C readlink /proc/900/exe"),
        "liveness: {cmds:?}"
    );
    assert!(
        !cmds.iter().any(|c| c.contains("no de")),
        "unsafe path reached ssh: {cmds:?}"
    );
    assert_eq!(o.code, 1, "{}", o.dbg());
}
