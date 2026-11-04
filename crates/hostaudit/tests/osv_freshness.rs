//! B5 — OSV advisories by SOURCE package + dpkg version ordering, cache freshness (CONTRACT.md §5).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};

const UBUNTU: &str = "PRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nID=ubuntu\nVERSION_ID=\"22.04\"\n";

fn record(id: &str, eco: &str, name: &str, fixed: &str) -> Value {
    json!({
        "id": id,
        "affected": [{
            "package": {"ecosystem": eco, "name": name},
            "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0"}, {"fixed": fixed}]}]
        }]
    })
}

/// Audit one ubuntu host whose dpkg list is `dpkg` against `feed`.
fn b5(os_release: &str, dpkg: &str, feed: &Value) -> (Out, Value) {
    let mut env = Env::new();
    let mut h = clean_host();
    h.insert(W1.into(), ok(os_release));
    h.insert(W2.into(), ok(dpkg));
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n");
    env.ssh_host("web1", &h);
    env.osv_feed(feed);
    let s = env.run(&["select", "web1"]);
    assert_eq!(s.code, 0, "{}", s.dbg());
    let o = env.run(&["audit", "--json"]);
    let f = finding(&o.findings(), "web1", "B5");
    (o, f)
}

#[test]
fn b5_matches_by_source_package_not_binary_name() {
    let feed = json!([record(
        "USN-9999-1",
        "Ubuntu:22.04:LTS",
        "openssl",
        "3.0.2-0ubuntu1.12"
    )]);
    let (o, f) = b5(
        UBUNTU,
        "libssl3\t3.0.2-0ubuntu1.10\topenssl\t3.0.2-0ubuntu1.10\n",
        &feed,
    );
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(f["verdict"], "violation", "{f}");
    let fix = f["fix"].as_str().unwrap_or("");
    assert!(fix.contains("--only-upgrade"), "{f}");
    assert!(
        fix.contains("libssl3"),
        "fix must name the binary package: {f}"
    );
    assert!(
        fix.contains("USN-9999-1"),
        "fix must carry the advisory id: {f}"
    );
    assert!(evidence_text(&f).contains("USN-9999-1"), "{f}");
}

#[test]
fn b5_record_named_after_binary_package_does_not_match() {
    // Only a record for the *binary* name exists; source is "openssl" → no match.
    let feed = json!([record("USN-9999-2", "Ubuntu:22.04:LTS", "libssl3", "9.9.9")]);
    let (o, f) = b5(
        UBUNTU,
        "libssl3\t3.0.2-0ubuntu1.10\topenssl\t3.0.2-0ubuntu1.10\n",
        &feed,
    );
    assert_eq!(f["verdict"], "clean", "{f}");
    assert_eq!(o.code, 0, "{}", o.dbg());
}

#[test]
fn b5_epoch_outranks_upstream_version() {
    // installed 1:2.0-1 > fixed 1.9-1 (epoch 1 vs 0) → not affected.
    let feed = json!([record("UBUNTU-CVE-1", "Ubuntu:22.04:LTS", "foo", "1.9-1")]);
    let (_, f) = b5(UBUNTU, "foo\t1:2.0-1\tfoo\t1:2.0-1\n", &feed);
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn b5_tilde_sorts_before_release() {
    // installed 2.0~rc1-1 < fixed 2.0-1 → affected.
    let feed = json!([record("UBUNTU-CVE-2", "Ubuntu:22.04:LTS", "foo", "2.0-1")]);
    let (_, f) = b5(UBUNTU, "foo\t2.0~rc1-1\tfoo\t2.0~rc1-1\n", &feed);
    assert_eq!(f["verdict"], "violation", "{f}");
}

#[test]
fn b5_numeric_not_lexical_revision_ordering() {
    // installed ...0.10 vs fixed ...0.9 : 10 > 9 numerically → not affected.
    let feed = json!([record(
        "UBUNTU-CVE-3",
        "Ubuntu:22.04:LTS",
        "openssh",
        "1:8.9p1-3ubuntu0.9"
    )]);
    let (_, f) = b5(
        UBUNTU,
        "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n",
        &feed,
    );
    assert_eq!(f["verdict"], "clean", "{f}");
}

#[test]
fn b5_debian_ecosystem_matches() {
    let feed = json!([record(
        "DSA-5000-1",
        "Debian:12",
        "openssl",
        "3.0.11-1~deb12u2"
    )]);
    let (_, f) = b5(
        "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\nVERSION_ID=\"12\"\n",
        "libssl3\t3.0.11-1~deb12u1\topenssl\t3.0.11-1~deb12u1\n",
        &feed,
    );
    assert_eq!(f["verdict"], "violation", "{f}");
}

#[test]
fn b5_zero_records_for_os_ecosystem_is_undetermined() {
    // Feed has data, but none for Ubuntu 22.04 (Pro stream does not count).
    let feed = json!([
        record("DSA-1", "Debian:12", "openssl", "9.9"),
        record("UBUNTU-PRO-1", "Ubuntu:Pro:22.04:LTS", "openssl", "9.9")
    ]);
    let (o, f) = b5(
        UBUNTU,
        "libssl3\t3.0.2-0ubuntu1.10\topenssl\t3.0.2-0ubuntu1.10\n",
        &feed,
    );
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert_eq!(o.code, 1, "{}", o.dbg());
}

#[test]
fn b5_non_debian_os_is_undetermined() {
    let (o, f) = b5(
        "NAME=\"Fedora Linux\"\nID=fedora\nID_LIKE=\"rhel debian\"\nVERSION_ID=39\n",
        "openssl\t3.0.2\topenssl\t3.0.2\n",
        &clean_osv(),
    );
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert_eq!(o.code, 1, "{}", o.dbg());
}

#[test]
fn b5_fetch_failure_without_cache_is_undetermined() {
    let mut env = Env::new();
    env.one_selected_host(&clean_host());
    env.osv_broken();
    let o = env.run(&["audit", "--json"]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let f = finding(&o.findings(), "web1", "B5");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

#[test]
fn b5_fresh_cache_is_used_and_stale_cache_is_undetermined() {
    let mut env = Env::new();
    env.one_selected_host(&clean_host()); // clean feed installed
    let first = env.run(&["audit", "--json"]);
    assert_eq!(first.code, 0, "{}", first.dbg());
    assert_eq!(verdict(&first.findings(), "web1", "B5"), "clean");

    env.osv_broken();
    let later = (NOW + 3600).to_string();
    let fresh = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &later)]);
    let f = finding(&fresh.findings(), "web1", "B5");
    assert_eq!(f["verdict"], "clean", "1h-old cache must be used: {f}");
    assert_eq!(f["advisory_fetched_at"], NOW, "cache time recorded: {f}");

    let stale = (NOW + 25 * 3600).to_string();
    let old = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &stale)]);
    assert_eq!(old.code, 1, "{}", old.dbg());
    assert_eq!(
        verdict(&old.findings(), "web1", "B5"),
        "undetermined",
        "25h-old cache exceeds the 24h limit"
    );
}

#[test]
fn b5_report_records_advisory_fetch_time() {
    let mut env = Env::new();
    let o = env.audit_one(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let rows = env.reports();
    let b5: Vec<_> = rows
        .iter()
        .filter(|r| r["rule"] == "B5" && r["target"] == "web1")
        .collect();
    assert_eq!(b5.len(), 1, "{rows:?}");
    assert_eq!(b5[0]["advisory_fetched_at"], NOW, "{}", b5[0]);
}
