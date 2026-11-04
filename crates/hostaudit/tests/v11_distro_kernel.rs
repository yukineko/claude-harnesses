//! v1.1 canon §5: K1 (release support), K2 (running kernel CVEs), K3 (reboot
//! pending), K4 (kernel security update pending), K5 (livepatch); K6 is parked.
//! Non-Debian-family Linux: K1 only, the rest Undetermined (CONTRACT.md §12.6).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn fix_of(f: &Value) -> String {
    f["fix"].as_str().unwrap_or("").to_string()
}
fn reason_of(f: &Value) -> String {
    f["reason"].as_str().unwrap_or("").to_string()
}

fn ubuntu_cycle(eol_days: i64, ext: Value) -> Value {
    json!({"cycle":"22.04","eol":ymd(NOW + eol_days*DAY),"extendedSupport":ext})
}

/// Audit `h` with the given ubuntu feed (None = feed unavailable).
fn run(h: &BTreeMap<String, Resp>, feed: Option<Value>) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    if let Some(c) = feed {
        env.eol_feed("ubuntu", &[c]);
    }
    let o = env.audit_one(h);
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

fn supported() -> Option<Value> {
    Some(ubuntu_cycle(200, json!(ymd(NOW + 2000 * DAY))))
}

// ---------------------------------------------------------------- K1

#[test]
fn k1_supported_release_is_clean() {
    let (_, _, fs) = run(&clean_host(), supported());
    assert_eq!(verdict(&fs, "web1", "K1"), "clean");
}

#[test]
fn k1_standard_and_extended_support_both_over_is_violation() {
    for ext in [json!(ymd(NOW - 10 * DAY)), json!(false)] {
        let (_, o, fs) = run(&clean_host(), Some(ubuntu_cycle(-100, ext.clone())));
        let f = finding(&fs, "web1", "K1");
        assert_eq!(f["verdict"], "violation", "ext={ext}: {f}");
        assert!(fix_of(&f).contains("do-release-upgrade"), "{f}");
        assert_eq!(o.code, 1, "{}", o.dbg());
    }
}

#[test]
fn k1_extended_support_only_is_warning_and_names_the_contract() {
    let (_, o, fs) = run(
        &clean_host(),
        Some(ubuntu_cycle(-100, json!(ymd(NOW + 500 * DAY)))),
    );
    let f = finding(&fs, "web1", "K1");
    assert_eq!(f["verdict"], "warning", "{f}");
    assert!(all_text(&f).contains("延長サポート"), "{f}");
    assert!(
        all_text(&f).contains("ESM") || all_text(&f).contains("ELTS"),
        "{f}"
    );
    assert!(!fix_of(&f).is_empty(), "{f}");
    assert_eq!(o.code, 0, "warning alone exits 0: {}", o.dbg());
}

#[test]
fn k1_falls_back_to_distro_info_when_endoflife_is_unavailable() {
    // feed missing -> W17 lists jammy -> clean
    let (_, _, fs) = run(&clean_host(), None);
    assert_eq!(verdict(&fs, "web1", "K1"), "clean");
    // W17 does not list the current codename -> violation
    let h = host(&[(W17U, ok("focal\nnoble\n"))]);
    let (_, o, fs) = run(&h, None);
    let f = finding(&fs, "web1", "K1");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert_eq!(o.code, 1, "{}", o.dbg());
}

#[test]
fn k1_unobtainable_support_data_is_undetermined() {
    // endoflife unavailable AND the tool is not installed
    let h = host(&[(W17U, notfound("ubuntu-distro-info"))]);
    let (_, _, fs) = run(&h, None);
    let f = finding(&fs, "web1", "K1");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(reason_of(&f).contains("distro-info"), "install hint: {f}");
    // endoflife unavailable AND ssh failure
    let h = host(&[(W17U, ssh_down())]);
    let (_, _, fs) = run(&h, None);
    assert_eq!(verdict(&fs, "web1", "K1"), "undetermined");
}

#[test]
fn k1_debian_fallback_uses_debian_distro_info() {
    let mut env = Env::new(); // no debian feed
    let o = env.audit_one(&debian_host());
    assert_eq!(verdict(&o.findings(), "web1", "K1"), "clean", "{}", o.dbg());
    assert!(
        remote_cmds(&env).iter().any(|c| c == W17D),
        "{:?}",
        remote_cmds(&env)
    );
    let mut h = debian_host();
    set(&mut h, W17D, ok("bullseye\ntrixie\n"));
    let mut env = Env::new();
    let o = env.audit_one(&h);
    assert_eq!(
        verdict(&o.findings(), "web1", "K1"),
        "violation",
        "{}",
        o.dbg()
    );
}

#[test]
fn k1_amazon_linux_uses_its_endoflife_slug() {
    let mut env = Env::new();
    env.eol_feed(
        "amazon-linux",
        &[json!({"cycle":"2023","eol":ymd(NOW-5*DAY),"extendedSupport":false})],
    );
    let o = env.audit_one(&amzn_host());
    let f = finding(&o.findings(), "web1", "K1");
    assert_eq!(f["verdict"], "violation", "{f}");
}

#[test]
fn non_debian_family_gets_k1_only_and_undetermined_elsewhere() {
    let mut env = Env::new();
    env.eol_feed(
        "amazon-linux",
        &[json!({"cycle":"2023","eol":ymd(NOW+900*DAY),"extendedSupport":false})],
    );
    let o = env.audit_one(&amzn_host());
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "K1"), "clean");
    for r in [
        "S1", "S2", "S3", "S5", "S6", "S7", "S8", "S9", "K2", "K3", "K4", "K5",
    ] {
        let f = finding(&fs, "web1", r);
        assert_eq!(f["verdict"], "undetermined", "{r}: {f}");
        assert!(reason_of(&f).contains("未実装"), "{r}: {f}");
    }
    assert_eq!(o.code, 1, "{}", o.dbg());
}

// ---------------------------------------------------------------- K2

const TWO_KERNELS: &str = "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n\
linux-image-5.15.0-90-generic\t5.15.0-90.100\tlinux\t5.15.0-90.100\n";

fn kernel_run(feed: Vec<Value>, kev: &[&str], over: &[(&str, Resp)]) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    let h = host(over);
    env.one_selected_host(&h);
    env.osv_feed(&Value::Array(feed));
    env.kev_feed(kev);
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    let fs = o.findings();
    (env, o, fs)
}

fn linux_rec(id: &str, sev: &str, fixed: &str) -> Value {
    osv_rec(id, &[], Some(("Ubuntu", sev)), "linux", Some(fixed))
}

#[test]
fn k2_running_kernel_with_open_cve_is_violation_with_evaluation() {
    let (_, o, fs) = kernel_run(
        vec![linux_rec("UBUNTU-CVE-2024-7001", "high", "5.15.0-105.115")],
        &["CVE-2024-7001"],
        &[],
    );
    let f = finding(&fs, "web1", "K2");
    assert_eq!(f["verdict"], "violation", "{f}");
    let c = &f["cves"]
        .as_array()
        .unwrap_or_else(|| panic!("K2 lacks cves[]: {f}"))[0];
    assert_eq!(c["cve"], "CVE-2024-7001", "{c}");
    assert_eq!(c["severity"], "high", "{c}");
    assert_eq!(c["kev"], "yes", "{c}");
    assert_eq!(c["priority"], "P0", "{c}");
    assert_eq!(c["fixed"], "5.15.0-105.115", "{c}");
    assert!(fix_of(&f).contains("再起動"), "{f}");
    assert_eq!(o.code, 1);
}

#[test]
fn k2_clean_when_the_running_kernel_is_not_affected() {
    let (_, _, fs) = kernel_run(
        vec![linux_rec("UBUNTU-CVE-2024-7002", "high", "5.15.0-95.105")],
        &[],
        &[],
    );
    assert_eq!(verdict(&fs, "web1", "K2"), "clean");
}

#[test]
fn k2_judges_the_running_kernel_not_every_installed_one() {
    // 5.15.0-90 (installed, not running) is affected, the running -100 is not.
    let (_, _, fs) = kernel_run(
        vec![linux_rec("UBUNTU-CVE-2024-7003", "high", "5.15.0-95.105")],
        &[],
        &[(W2, ok(TWO_KERNELS))],
    );
    assert_eq!(
        verdict(&fs, "web1", "K2"),
        "clean",
        "running kernel is fixed"
    );
    assert_eq!(
        verdict(&fs, "web1", "B5"),
        "violation",
        "B5 still covers every installed package"
    );
}

#[test]
fn k2_undetermined_when_running_kernel_or_data_is_unknown() {
    let feed = vec![linux_rec("UBUNTU-CVE-2024-7001", "high", "5.15.0-105.115")];
    // running kernel's package is not in dpkg output
    let (_, _, fs) = kernel_run(feed.clone(), &[], &[(W24, ok("5.15.0-77-generic\n"))]);
    assert_eq!(verdict(&fs, "web1", "K2"), "undetermined");
    // uname failed
    let (_, _, fs) = kernel_run(feed, &[], &[(W24, ssh_down())]);
    assert_eq!(verdict(&fs, "web1", "K2"), "undetermined");
    // advisory feed unobtainable
    let mut env = Env::new();
    env.one_selected_host(&clean_host());
    env.osv_broken();
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    assert_eq!(verdict(&o.findings(), "web1", "K2"), "undetermined");
}

// ---------------------------------------------------------------- K3

#[test]
fn k3_newer_installed_kernel_of_same_flavour_is_reboot_pending() {
    let dpkg = "linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n\
linux-image-5.15.0-105-generic\t5.15.0-105.115\tlinux\t5.15.0-105.115\n";
    let (_, _, fs) = kernel_run(
        vec![linux_rec("UBUNTU-CVE-2024-7001", "high", "5.15.0-105.115")],
        &[],
        &[(W2, ok(dpkg))],
    );
    let f = finding(&fs, "web1", "K3");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("5.15.0-105"), "{f}");
    assert!(
        all_text(&f).contains("K2"),
        "must note that K2 vulnerabilities remain: {f}"
    );
    assert!(fix_of(&f).contains("再起動"), "{f}");
}

#[test]
fn k3_other_flavour_newer_does_not_count() {
    let dpkg = "linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n\
linux-image-6.8.0-1-aws\t6.8.0-1.1\tlinux-aws\t6.8.0-1.1\n";
    let (_, _, fs) = kernel_run(vec![clean_osv()[0].clone()], &[], &[(W2, ok(dpkg))]);
    assert_eq!(verdict(&fs, "web1", "K3"), "clean");
}

#[test]
fn k3_orders_versions_numerically() {
    // running -99, installed -100: 100 > 99 numerically (lexical order says otherwise)
    let dpkg = "linux-image-5.15.0-99-generic\t5.15.0-99.109\tlinux\t5.15.0-99.109\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n";
    let (_, _, fs) = kernel_run(
        vec![clean_osv()[0].clone()],
        &[],
        &[(W2, ok(dpkg)), (W24, ok("5.15.0-99-generic\n"))],
    );
    assert_eq!(verdict(&fs, "web1", "K3"), "violation");
}

#[test]
fn k3_running_the_newest_is_clean_and_failures_are_undetermined() {
    let (_, _, fs) = kernel_run(vec![clean_osv()[0].clone()], &[], &[]);
    assert_eq!(verdict(&fs, "web1", "K3"), "clean");
    let (_, _, fs) = kernel_run(vec![clean_osv()[0].clone()], &[], &[(W24, ssh_down())]);
    assert_eq!(verdict(&fs, "web1", "K3"), "undetermined");
}

// ---------------------------------------------------------------- K4

#[test]
fn k4_pending_kernel_security_update_is_violation() {
    let w14 = "Inst linux-image-5.15.0-105-generic (5.15.0-105.115 Ubuntu:22.04/jammy-updates, Ubuntu:22.04/jammy-security [amd64])\n";
    let (_, _, fs) = run(&host(&[(W14, ok(w14))]), supported());
    let f = finding(&fs, "web1", "K4");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        evidence_text(&f).contains("linux-image-5.15.0-105-generic"),
        "{f}"
    );
    assert!(
        fix_of(&f).contains("sudo apt update && sudo apt upgrade"),
        "{f}"
    );
}

#[test]
fn k4_non_security_kernel_or_non_kernel_security_is_clean() {
    let w14 =
        "Inst linux-image-5.15.0-105-generic (5.15.0-105.115 Ubuntu:22.04/jammy-updates [amd64])\n\
Inst libssl3 [3.0.2-0ubuntu1.10] (3.0.2-0ubuntu1.12 Ubuntu:22.04/jammy-security [amd64])\n";
    let (_, _, fs) = run(&host(&[(W14, ok(w14))]), supported());
    assert_eq!(verdict(&fs, "web1", "K4"), "clean");
    assert_eq!(
        verdict(&fs, "web1", "S1"),
        "violation",
        "S1 sees the libssl3 one"
    );
}

#[test]
fn k4_unusable_simulation_is_undetermined() {
    let (_, _, fs) = run(&host(&[(W14, ssh_down())]), supported());
    assert_eq!(verdict(&fs, "web1", "K4"), "undetermined");
}

// ---------------------------------------------------------------- K5

fn livepatch(status: &str) -> Resp {
    ok(&format!(
        r#"{{"services":[{{"name":"esm-infra","status":"enabled"}},{{"name":"livepatch","status":"{status}"}}]}}"#
    ))
}

fn k5(sev: &str, lp: Resp) -> Value {
    let (_, _, fs) = kernel_run(
        vec![linux_rec("UBUNTU-CVE-2024-7001", sev, "5.15.0-105.115")],
        &[],
        &[(W25, lp)],
    );
    finding(&fs, "web1", "K5")
}

#[test]
fn k5_high_kernel_cve_without_livepatch_is_warning() {
    for sev in ["high", "critical"] {
        let f = k5(sev, livepatch("disabled"));
        assert_eq!(f["verdict"], "warning", "{sev}: {f}");
        assert!(fix_of(&f).contains("sudo pro enable livepatch"), "{f}");
        assert!(all_text(&f).contains("再起動"), "{f}");
    }
}

#[test]
fn k5_clean_when_livepatch_enabled_or_no_high_kernel_cve() {
    assert_eq!(k5("high", livepatch("enabled"))["verdict"], "clean");
    assert_eq!(k5("medium", livepatch("disabled"))["verdict"], "clean");
}

#[test]
fn k5_tool_missing_is_undetermined_with_install_command() {
    let f = k5("high", notfound("pro"));
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(reason_of(&f).contains("ubuntu-pro-client"), "{f}");
}

#[test]
fn k5_unexpected_schema_is_undetermined() {
    let f = k5("high", ok(r#"{"nothing":"here"}"#));
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

#[test]
fn k5_and_k2_k3_k4_on_debian() {
    let mut env = Env::new();
    env.eol_feed("debian", &[cyc("12", 300)]);
    env.osv_feed(&json!([{
        "id": "DSA-1", "aliases": ["CVE-2024-9"],
        "affected": [{"package": {"ecosystem": "Debian:12", "name": "linux"},
                      "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced":"0"},{"fixed":"6.1.200-1"}]}]}]
    }]));
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n");
    env.ssh_host("web1", &debian_host());
    assert_eq!(env.run(&["select", "web1"]).code, 0);
    let o = env.run(&["audit", "--json"]);
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "K5"), "not_applicable", "{}", o.dbg());
    assert_eq!(
        verdict(&fs, "web1", "K2"),
        "violation",
        "Debian kernels are judged too"
    );
}
