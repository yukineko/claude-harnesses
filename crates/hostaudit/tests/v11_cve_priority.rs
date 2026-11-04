//! v1.1 canon §3: B5 carries per-CVE evaluation (severity, KEV, fixed, exposure)
//! and a deterministic P0..P3 priority (CONTRACT.md §12.4).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};

const DPKG: &str = "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n\
curl\t7.81.0-1\tcurl\t7.81.0-1\n\
tar\t1.34-1\ttar\t1.34-1\n\
gzip\t1.10-1\tgzip\t1.10-1\n";

const SSHD_ANY: &str = "LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=812,fd=3))\n";
const SSHD_LOCAL: &str = "LISTEN 0 128 127.0.0.1:22 0.0.0.0:* users:((\"sshd\",pid=812,fd=3))\n";

struct Run {
    env: Env,
    out: Out,
    b5: Value,
}

fn run(
    feed: Vec<Value>,
    kev: Option<&[&str]>,
    tweak: impl FnOnce(&mut std::collections::BTreeMap<String, Resp>),
) -> Run {
    let mut env = Env::new();
    let mut h = clean_host();
    set(&mut h, W2, ok(DPKG));
    tweak(&mut h);
    env.one_selected_host(&h);
    env.osv_feed(&Value::Array(feed));
    if let Some(k) = kev {
        env.kev_feed(k);
    }
    env.eol_ubuntu_supported();
    let out = env.run(&["audit", "--json"]);
    let b5 = finding(&out.findings(), "web1", "B5");
    Run { env, out, b5 }
}

fn cves(f: &Value) -> Vec<Value> {
    f["cves"]
        .as_array()
        .unwrap_or_else(|| panic!("B5 finding lacks cves[]: {f}"))
        .clone()
}

fn cve_of<'a>(cs: &'a [Value], cve: &str) -> &'a Value {
    cs.iter()
        .find(|c| c["cve"] == cve)
        .unwrap_or_else(|| panic!("no cves[] entry for {cve}: {cs:?}"))
}

#[test]
fn priority_table_p0_to_p3_and_counts() {
    let feed = vec![
        osv_rec(
            "UBUNTU-CVE-2024-0001",
            &[],
            Some(("Ubuntu", "low")),
            "tar",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0002",
            &[],
            Some(("Ubuntu", "high")),
            "openssh",
            Some("1:9.9p1-1"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0003",
            &[],
            Some(("Ubuntu", "high")),
            "curl",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0004",
            &[],
            Some(("Ubuntu", "low")),
            "gzip",
            Some("9.9"),
        ),
    ];
    let r = run(feed, Some(&["CVE-2024-0001"]), |h| {
        set(h, W12, ok(SSHD_ANY))
    });
    assert_eq!(r.out.code, 1, "{}", r.out.dbg());
    assert_eq!(r.b5["verdict"], "violation", "{}", r.b5);
    let cs = cves(&r.b5);
    assert_eq!(cs.len(), 4, "{cs:?}");
    // P0: KEV beats low severity and no exposure.
    let a = cve_of(&cs, "CVE-2024-0001");
    assert_eq!(
        (a["priority"].as_str(), a["kev"].as_str()),
        (Some("P0"), Some("yes")),
        "{a}"
    );
    // P1: high + exposed (sshd listens on 0.0.0.0, owned by source `openssh`).
    let b = cve_of(&cs, "CVE-2024-0002");
    assert_eq!(
        (b["priority"].as_str(), b["exposed"].as_str()),
        (Some("P1"), Some("yes")),
        "{b}"
    );
    assert_eq!(b["severity"], "high", "{b}");
    // P2: high but nothing of that source listens externally.
    let c = cve_of(&cs, "CVE-2024-0003");
    assert_eq!(
        (c["priority"].as_str(), c["exposed"].as_str()),
        (Some("P2"), Some("no")),
        "{c}"
    );
    // P3: low, not KEV.
    let d = cve_of(&cs, "CVE-2024-0004");
    assert_eq!(
        (d["priority"].as_str(), d["kev"].as_str()),
        (Some("P3"), Some("no")),
        "{d}"
    );
    assert_eq!(
        r.b5["priority_counts"],
        json!({"P0": 1, "P1": 1, "P2": 1, "P3": 1}),
        "{}",
        r.b5
    );
    // [choice] cves[] is ordered P0 first.
    let order: Vec<&str> = cs.iter().filter_map(|c| c["priority"].as_str()).collect();
    assert_eq!(order, ["P0", "P1", "P2", "P3"], "{cs:?}");
}

#[test]
fn high_with_unobservable_exposure_is_p1_not_downgraded() {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0002",
        &[],
        Some(("Ubuntu", "high")),
        "openssh",
        Some("1:9.9p1-1"),
    )];
    let r = run(feed, Some(&[]), |h| set(h, W12, sudo_fail()));
    let c = &cves(&r.b5)[0];
    assert_eq!(c["exposed"], "unknown", "{c}");
    assert_eq!(
        c["priority"], "P1",
        "unknown exposure must not lower the priority: {c}"
    );
}

#[test]
fn high_on_loopback_only_service_is_not_exposed() {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0002",
        &[],
        Some(("Ubuntu", "critical")),
        "openssh",
        Some("1:9.9p1-1"),
    )];
    let r = run(feed, Some(&[]), |h| set(h, W12, ok(SSHD_LOCAL)));
    let c = &cves(&r.b5)[0];
    assert_eq!(c["exposed"], "no", "{c}");
    assert_eq!(c["priority"], "P2", "{c}");
}

#[test]
fn unknown_severity_is_p2_never_p3() {
    // No severity at all, advisory id only (no CVE alias).
    let feed = vec![osv_rec("USN-6001-1", &[], None, "curl", Some("9.9"))];
    let r = run(feed, Some(&[]), |_| {});
    let c = &cves(&r.b5)[0];
    assert_eq!(c["severity"], "unknown", "{c}");
    assert_eq!(c["priority"], "P2", "{c}");
    assert_eq!(c["id"], "USN-6001-1", "{c}");
    assert!(
        c["cve"].is_null(),
        "advisory-only record has no CVE id: {c}"
    );
}

#[test]
fn cve_id_comes_from_id_or_aliases() {
    let feed = vec![
        osv_rec(
            "UBUNTU-CVE-2024-1234",
            &[],
            Some(("Ubuntu", "low")),
            "tar",
            Some("9.9"),
        ),
        osv_rec(
            "USN-6000-1",
            &["CVE-2024-5555"],
            Some(("Ubuntu", "low")),
            "gzip",
            Some("9.9"),
        ),
    ];
    let r = run(feed, Some(&[]), |_| {});
    let cs = cves(&r.b5);
    let a = cs
        .iter()
        .find(|c| c["id"] == "UBUNTU-CVE-2024-1234")
        .unwrap();
    assert_eq!(a["cve"], "CVE-2024-1234", "{a}");
    let b = cs.iter().find(|c| c["id"] == "USN-6000-1").unwrap();
    assert_eq!(b["cve"], "CVE-2024-5555", "{b}");
}

#[test]
fn kev_match_uses_the_alias_cve() {
    let feed = vec![osv_rec(
        "USN-6000-1",
        &["CVE-2024-5555"],
        Some(("Ubuntu", "low")),
        "gzip",
        Some("9.9"),
    )];
    let r = run(feed, Some(&["CVE-2024-5555"]), |_| {});
    let c = &cves(&r.b5)[0];
    assert_eq!(
        (c["kev"].as_str(), c["priority"].as_str()),
        (Some("yes"), Some("P0")),
        "{c}"
    );
}

#[test]
fn severity_falls_back_to_cvss_v3_base_score() {
    let feed = vec![
        osv_rec(
            "UBUNTU-CVE-2024-0011",
            &[],
            Some(("CVSS_V3", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H")),
            "tar",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0012",
            &[],
            Some(("CVSS_V3", "CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:H/I:H/A:H")),
            "gzip",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0013",
            &[],
            Some(("CVSS_V3", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:N/A:N")),
            "curl",
            Some("9.9"),
        ),
    ];
    let r = run(feed, Some(&[]), |_| {});
    let cs = cves(&r.b5);
    assert_eq!(cve_of(&cs, "CVE-2024-0011")["severity"], "critical", "9.8");
    assert_eq!(cve_of(&cs, "CVE-2024-0012")["severity"], "high", "8.8");
    assert_eq!(cve_of(&cs, "CVE-2024-0013")["severity"], "medium", "5.3");
}

#[test]
fn ubuntu_severity_wins_over_cvss() {
    let mut rec = osv_rec(
        "UBUNTU-CVE-2024-0014",
        &[],
        Some(("Ubuntu", "low")),
        "tar",
        Some("9.9"),
    );
    rec["severity"] = json!([
        {"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"},
        {"type": "Ubuntu", "score": "low"}
    ]);
    let r = run(vec![rec], Some(&[]), |_| {});
    assert_eq!(cves(&r.b5)[0]["severity"], "low");
}

#[test]
fn unreadable_kev_feed_marks_kev_unknown_and_keeps_b5_violation() {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0001",
        &[],
        Some(("Ubuntu", "low")),
        "tar",
        Some("9.9"),
    )];
    let r = run(feed, None, |_| {}); // kev.json never written
    assert_eq!(
        r.b5["verdict"], "violation",
        "B5 verdict must not depend on KEV: {}",
        r.b5
    );
    let c = &cves(&r.b5)[0];
    assert_eq!(c["kev"], "unknown", "{c}");
    // Cannot decide P0: the rest of the table gives P3 for low; "KEV 不明" is shown alongside.
    assert_eq!(c["priority"], "P3", "{c}");
    assert!(
        all_text(&r.b5).contains("KEV"),
        "KEV unknown must be stated: {}",
        r.b5
    );
    assert_eq!(r.out.code, 1, "{}", r.out.dbg());
}

#[test]
fn fixed_version_is_reported_and_missing_fix_demands_a_workaround() {
    let feed = vec![
        osv_rec(
            "UBUNTU-CVE-2024-0021",
            &[],
            Some(("Ubuntu", "low")),
            "tar",
            Some("1.34-9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0022",
            &[],
            Some(("Ubuntu", "low")),
            "gzip",
            None,
        ),
    ];
    let r = run(feed, Some(&[]), |_| {});
    let cs = cves(&r.b5);
    assert_eq!(cve_of(&cs, "CVE-2024-0021")["fixed"], "1.34-9");
    assert_eq!(cve_of(&cs, "CVE-2024-0022")["fixed"], "unknown");
    let fix = r.b5["fix"].as_str().unwrap();
    assert!(
        fix.contains("回避策"),
        "no-fix CVE needs a workaround note: {fix}"
    );
    assert!(
        fix.contains("CVE-2024-0022") || fix.contains("gzip"),
        "{fix}"
    );
}

#[test]
fn fixes_are_grouped_into_one_apt_command() {
    let feed = vec![
        osv_rec(
            "UBUNTU-CVE-2024-0031",
            &[],
            Some(("Ubuntu", "low")),
            "tar",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0032",
            &[],
            Some(("Ubuntu", "low")),
            "gzip",
            Some("9.9"),
        ),
        osv_rec(
            "UBUNTU-CVE-2024-0033",
            &[],
            Some(("Ubuntu", "medium")),
            "tar",
            Some("9.9"),
        ),
    ];
    let r = run(feed, Some(&[]), |_| {});
    let fix = r.b5["fix"].as_str().unwrap();
    assert_eq!(
        fix.matches("--only-upgrade").count(),
        1,
        "one command for all: {fix}"
    );
    assert!(fix.contains("tar") && fix.contains("gzip"), "{fix}");
    assert_eq!(fix.matches("apt install").count(), 1, "{fix}");
}

#[test]
fn kev_cache_is_used_while_fresh_and_dropped_when_stale() {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0001",
        &[],
        Some(("Ubuntu", "low")),
        "tar",
        Some("9.9"),
    )];
    let r = run(feed, Some(&["CVE-2024-0001"]), |_| {});
    assert_eq!(cves(&r.b5)[0]["kev"], "yes");
    assert_eq!(r.b5["kev_fetched_at"], NOW, "{}", r.b5);
    let env = r.env;
    env.kev_remove();
    let t1 = (NOW + 3600).to_string();
    let fresh = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &t1)]);
    let f = finding(&fresh.findings(), "web1", "B5");
    assert_eq!(
        cves(&f)[0]["kev"],
        "yes",
        "1h-old KEV cache must be used: {f}"
    );
    assert_eq!(f["kev_fetched_at"], NOW, "cache time recorded: {f}");
    let t25 = (NOW + 25 * 3600).to_string();
    let old = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &t25)]);
    let f = finding(&old.findings(), "web1", "B5");
    assert_eq!(f["verdict"], "violation", "OSV still fetchable: {f}");
    assert_eq!(
        cves(&f)[0]["kev"],
        "unknown",
        "25h-old KEV cache is stale: {f}"
    );
}

#[test]
fn kev_cache_shares_the_osv_max_age_setting() {
    let feed = vec![osv_rec(
        "UBUNTU-CVE-2024-0001",
        &[],
        Some(("Ubuntu", "low")),
        "tar",
        Some("9.9"),
    )];
    let r = run(feed, Some(&["CVE-2024-0001"]), |_| {});
    let env = r.env;
    write(
        &env.hostaudit_dir().join("config.toml"),
        "osv_cache_max_age_hours = 2\n",
    );
    env.kev_remove();
    let t = (NOW + 3 * 3600).to_string();
    let o = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &t)]);
    let f = finding(&o.findings(), "web1", "B5");
    assert_eq!(cves(&f)[0]["kev"], "unknown", "3h > configured 2h: {f}");
}
