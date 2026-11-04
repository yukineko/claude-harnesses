//! Canon ed704868 item 13 (CONTRACT.md 12.11): W2 install status (`rc` packages are
//! not installed), CVE exposure for any non-loopback listen address, B4 regression
//! guard (specific-IP binds stay out of B4, as v1 specifies).
//!
//! W2 fixtures in the other test files are 4-field rows; `common::w2_with_status`
//! prefixes `ii ` to them. Rows written here have 5 fields (status first), which the
//! shim leaves untouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;
use std::collections::BTreeMap;

fn row(status: &str, pkg: &str, ver: &str, src: &str, sver: &str) -> String {
    format!("{status}\t{pkg}\t{ver}\t{src}\t{sver}\n")
}

const KERNEL_II: &str =
    "ii \tlinux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n";
const SSH_II: &str = "ii \topenssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n";

fn host_with_w2(w2: &str) -> BTreeMap<String, Resp> {
    let mut h = clean_host();
    set(&mut h, W2, ok(w2));
    h
}

fn eol_nodejs_feed() -> Vec<Value> {
    vec![cyc("20", -250)]
}

// ---------------------------------------------------------------- wire string

#[test]
fn w2_wire_string_requests_status_abbrev_and_is_sent() {
    assert!(
        W2.contains("${db:Status-Abbrev}"),
        "fixture W2 must carry the status column: {W2}"
    );
    let mut env = Env::new();
    let o = env.audit_one(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let cmds = remote_cmds(&env);
    assert!(
        cmds.iter().any(|c| c == W2),
        "W2 (with ${{db:Status-Abbrev}}) must be sent byte-for-byte: {cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.contains("dpkg-query") && !c.contains("Status-Abbrev")),
        "the old status-less W2 must no longer be sent: {cmds:?}"
    );
}

// ---------------------------------------------------------------- B5

fn b5_with(w2: &str) -> Value {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-9001",
        &[],
        Some(("Ubuntu", "high")),
        "curl",
        Some("7.81.0-2"),
    )];
    let mut env = Env::new();
    let h = host_with_w2(w2);
    env.one_selected_host(&h);
    env.osv_feed(&Value::Array(feed));
    env.kev_feed(&[]);
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    finding(&o.findings(), "web1", "B5")
}

#[test]
fn b5_installed_vulnerable_package_is_a_violation_liveness() {
    let w2 = format!(
        "{KERNEL_II}{SSH_II}{}",
        row("ii ", "curl", "7.81.0-1", "curl", "7.81.0-1")
    );
    let f = b5_with(&w2);
    assert_eq!(f["verdict"], "violation", "{f}");
}

#[test]
fn b5_rc_package_is_ignored() {
    let w2 = format!(
        "{KERNEL_II}{SSH_II}{}",
        row("rc ", "curl", "7.81.0-1", "curl", "7.81.0-1")
    );
    let f = b5_with(&w2);
    assert_eq!(
        f["verdict"], "clean",
        "a removed (rc, config-files only) package is not installed: {f}"
    );
}

#[test]
fn b5_unparseable_status_is_undetermined() {
    for bad in ["", "i", "!!!", "ii extra"] {
        let w2 = format!(
            "{KERNEL_II}{SSH_II}{}",
            row(bad, "curl", "7.81.0-1", "curl", "7.81.0-1")
        );
        let f = b5_with(&w2);
        assert_eq!(
            f["verdict"], "undetermined",
            "status {bad:?} cannot be read, so the install state is unknown: {f}"
        );
    }
}

// ---------------------------------------------------------------- S9

#[test]
fn s9_removed_nodejs_rc_is_not_a_violation() {
    let base = format!("{KERNEL_II}{SSH_II}");
    // Liveness: installed nodejs 20 is a violation with this feed.
    let live = host_with_w2(&format!(
        "{base}{}",
        row(
            "ii ",
            "nodejs",
            "20.20.2-1nodesource1",
            "nodejs",
            "20.20.2-1nodesource1"
        )
    ));
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    env.eol_feed("nodejs", &eol_nodejs_feed());
    let o = env.audit_one(&live);
    assert_eq!(
        verdict(&o.findings(), "web1", "S9"),
        "violation",
        "{}",
        o.dbg()
    );
    // rc: ignored.
    let gone = host_with_w2(&format!(
        "{base}{}",
        row("rc ", "python3.9", "3.9.5-1", "python3.9", "3.9.5-1")
    ));
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    env.eol_feed("python", &[cyc("3.9", -400)]);
    let o = env.audit_one(&gone);
    let v = verdict(&o.findings(), "web1", "S9");
    assert!(
        v != "violation" && v != "undetermined",
        "removed python3.9 (rc) must not trip S9, got {v}: {}",
        o.dbg()
    );
    let gone_node = host_with_w2(&format!(
        "{base}{}",
        row(
            "rc ",
            "nodejs",
            "20.20.2-1nodesource1",
            "nodejs",
            "20.20.2-1nodesource1"
        )
    ));
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    env.eol_feed("nodejs", &eol_nodejs_feed());
    let o = env.audit_one(&gone_node);
    let v = verdict(&o.findings(), "web1", "S9");
    assert!(
        v != "violation" && v != "undetermined",
        "removed nodejs (rc) must not trip S9, got {v}: {}",
        o.dbg()
    );
}

// ---------------------------------------------------------------- K2 / K3

fn kernel_env(w2: &str) -> Out {
    let mut env = Env::new();
    let h = host_with_w2(w2);
    env.one_selected_host(&h);
    env.osv_feed(&Value::Array(vec![osv_rec(
        "UBUNTU-CVE-2024-7001",
        &[],
        Some(("Ubuntu", "high")),
        "linux",
        Some("5.15.0-105.115"),
    )]));
    env.kev_feed(&[]);
    env.eol_ubuntu_supported();
    env.run(&["audit", "--json"])
}

#[test]
fn k3_newer_kernel_in_rc_state_is_not_a_pending_reboot() {
    // Liveness: an installed newer kernel of the same flavour is reboot-pending.
    let live = format!(
        "{SSH_II}{KERNEL_II}{}",
        row(
            "ii ",
            "linux-image-5.15.0-105-generic",
            "5.15.0-105.115",
            "linux",
            "5.15.0-105.115"
        )
    );
    let o = kernel_env(&live);
    assert_eq!(
        verdict(&o.findings(), "web1", "K3"),
        "violation",
        "{}",
        o.dbg()
    );
    // rc: the newer kernel was removed.
    let gone = format!(
        "{SSH_II}{KERNEL_II}{}",
        row(
            "rc ",
            "linux-image-5.15.0-105-generic",
            "5.15.0-105.115",
            "linux",
            "5.15.0-105.115"
        )
    );
    let o = kernel_env(&gone);
    assert_eq!(
        verdict(&o.findings(), "web1", "K3"),
        "clean",
        "rc kernel is not installed: {}",
        o.dbg()
    );
}

#[test]
fn k2_running_kernel_package_only_in_rc_state_is_never_clean() {
    // The running package (linux-image-<uname -r>) is not installed per dpkg:
    // K2 cannot evaluate it, and must not read that as "no CVEs".
    let w2 = format!(
        "{SSH_II}{}",
        row(
            "rc ",
            "linux-image-5.15.0-100-generic",
            "5.15.0-100.110",
            "linux",
            "5.15.0-100.110"
        )
    );
    let o = kernel_env(&w2);
    let v = verdict(&o.findings(), "web1", "K2");
    assert_ne!(v, "clean", "{}", o.dbg());
}

#[test]
fn k2_rc_duplicate_version_does_not_shadow_the_installed_running_kernel() {
    // An rc row with an older (fixed-looking) version must not hide the
    // installed running kernel's CVE.
    let w2 = format!(
        "{SSH_II}{}{KERNEL_II}",
        row(
            "rc ",
            "linux-image-5.15.0-100-generic",
            "5.15.0-106.116",
            "linux",
            "5.15.0-106.116"
        )
    );
    let o = kernel_env(&w2);
    assert_eq!(
        verdict(&o.findings(), "web1", "K2"),
        "violation",
        "{}",
        o.dbg()
    );
}

// ---------------------------------------------------------------- exposure

const DPKG4: &str = "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n";

fn exposure_of(listen_lines: &str) -> Value {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0002",
        &[],
        Some(("Ubuntu", "high")),
        "openssh",
        Some("1:9.9p1-1"),
    )];
    let mut env = Env::new();
    let mut h = clean_host();
    set(&mut h, W2, ok(DPKG4));
    set(&mut h, W12, ok(listen_lines));
    env.one_selected_host(&h);
    env.osv_feed(&Value::Array(feed));
    env.kev_feed(&[]);
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    let f = finding(&o.findings(), "web1", "B5");
    f["cves"][0].clone()
}

fn sshd_at(addr: &str) -> String {
    format!("LISTEN 0 128 {addr} 0.0.0.0:* users:((\"sshd\",pid=812,fd=3))\n")
}

#[test]
fn exposure_specific_public_ipv4_is_exposed() {
    let c = exposure_of(&sshd_at("203.0.113.5:443"));
    assert_eq!(c["exposed"], "yes", "{c}");
}

#[test]
fn exposure_specific_public_ipv6_is_exposed() {
    let c = exposure_of(&sshd_at("[2001:db8::1]:443"));
    assert_eq!(c["exposed"], "yes", "{c}");
}

#[test]
fn exposure_private_rfc1918_address_is_exposed() {
    // Only loopback is exempt; a private LAN address is reachable by other hosts.
    let c = exposure_of(&sshd_at("10.0.0.7:22"));
    assert_eq!(c["exposed"], "yes", "{c}");
}

#[test]
fn exposure_whole_127_slash_8_and_ipv6_loopback_are_not_exposed() {
    for a in [
        "127.0.0.1:22",
        "127.0.0.2:22",
        "127.255.255.254:22",
        "[::1]:22",
    ] {
        let c = exposure_of(&sshd_at(a));
        assert_eq!(c["exposed"], "no", "{a}: {c}");
    }
}

#[test]
fn exposure_one_external_bind_among_loopback_ones_is_exposed() {
    let lines = format!("{}{}", sshd_at("127.0.0.1:22"), sshd_at("203.0.113.5:443"));
    let c = exposure_of(&lines);
    assert_eq!(c["exposed"], "yes", "{c}");
}

#[test]
fn exposure_lookalike_loopback_is_not_exempt() {
    // 127.0.0.1.evil / 1127.0.0.1 are not loopback; neither parses as 127/8.
    for a in ["1127.0.0.1:22", "128.0.0.1:22"] {
        let c = exposure_of(&sshd_at(a));
        assert_ne!(c["exposed"], "no", "{a}: {c}");
    }
}

// ---------------------------------------------------------------- B4 regression guard

#[test]
fn b4_specific_ip_db_bind_is_still_ignored() {
    // v1: B4 only looks at wildcard binds. Item 13 widens CVE exposure, not B4.
    let mut h = clean_host();
    set(
        &mut h,
        W3,
        ok("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\nLISTEN 0 4096 203.0.113.5:3306 0.0.0.0:*\n"),
    );
    let mut env = Env::new();
    let o = env.audit_one(&h);
    assert_eq!(verdict(&o.findings(), "web1", "B4"), "clean", "{}", o.dbg());
    // Liveness: the same port on the wildcard address is a violation.
    let mut h = clean_host();
    set(
        &mut h,
        W3,
        ok("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\nLISTEN 0 4096 0.0.0.0:3306 0.0.0.0:*\n"),
    );
    let mut env = Env::new();
    let o = env.audit_one(&h);
    assert_eq!(
        verdict(&o.findings(), "web1", "B4"),
        "violation",
        "{}",
        o.dbg()
    );
}
