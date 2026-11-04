//! inventory / select / audit-refusal contract (CONTRACT.md §2, §3, §4.1–4.3).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;

fn aliases(v: &Value) -> Vec<String> {
    let mut a: Vec<String> = v["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("no hosts array: {v}"))
        .iter()
        .map(|h| h["alias"].as_str().unwrap().to_string())
        .collect();
    a.sort();
    a
}

fn host<'a>(v: &'a Value, alias: &str) -> &'a Value {
    v["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["alias"] == alias)
        .unwrap_or_else(|| panic!("alias {alias} not in {v}"))
}

#[test]
fn inventory_parses_fields_follows_include_and_skips_wildcards() {
    let env = Env::new();
    env.ssh_config(
        "# main config\n\
         Include config.d/*\n\
         Host web1\n\
         \x20 HostName 10.0.0.1\n\
         \x20 User ubuntu\n\
         \x20 Port 2222\n\
         \x20 ProxyJump bastion\n\
         \n\
         Host bastion\n\
         \x20 hostname=bastion.example.com\n\
         Host web3 *.internal !secret\n\
         \x20 HostName 10.0.0.3\n\
         Host web?\n\
         \x20 User wild\n\
         Host *\n\
         \x20 User nobody\n",
    );
    write(
        &env.home.join(".ssh/config.d/extra"),
        "Host db1\n  HostName 10.0.0.9\n",
    );
    let o = env.run(&["inventory", "--json"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let v = o.json();
    assert_eq!(v["ssh_config"], "present", "{v}");
    assert_eq!(aliases(&v), vec!["bastion", "db1", "web1", "web3"], "{v}");
    let w = host(&v, "web1");
    assert_eq!(w["hostname"], "10.0.0.1");
    assert_eq!(w["user"], "ubuntu");
    assert_eq!(w["port"], 2222);
    assert_eq!(w["proxy_jump"], "bastion");
    assert_eq!(w["source"], "ssh_config");
    assert_eq!(host(&v, "bastion")["hostname"], "bastion.example.com");
    assert_eq!(host(&v, "db1")["hostname"], "10.0.0.9");
    assert_eq!(host(&v, "web3")["hostname"], "10.0.0.3");
}

#[test]
fn inventory_unreadable_ssh_config_is_undetermined_not_empty() {
    let env = Env::new();
    // A directory where the file should be: unreadable even as root.
    std::fs::create_dir_all(env.home.join(".ssh/config")).unwrap();
    let o = env.run(&["inventory", "--json"]);
    assert_eq!(o.code, 4, "unreadable ssh config must exit 4: {}", o.dbg());
    if let Ok(v) = serde_json::from_str::<Value>(o.stdout.trim()) {
        assert!(v.get("hosts").is_none(), "must not print a host list: {v}");
    }
}

#[test]
fn inventory_unparseable_ssh_config_is_undetermined_not_empty() {
    let env = Env::new();
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n  Port notanumber\n");
    let o = env.run(&["inventory", "--json"]);
    assert_eq!(o.code, 4, "unparseable ssh config must exit 4: {}", o.dbg());
    if let Ok(v) = serde_json::from_str::<Value>(o.stdout.trim()) {
        assert!(v.get("hosts").is_none(), "must not print a host list: {v}");
    }
}

#[test]
fn inventory_absent_ssh_config_is_empty_with_absent_marker() {
    let env = Env::new();
    let o = env.run(&["inventory", "--json"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let v = o.json();
    assert_eq!(v["ssh_config"], "absent", "{v}");
    assert_eq!(v["hosts"], serde_json::json!([]), "{v}");
}

#[test]
fn inventory_from_cmd_lines_become_hosts() {
    let env = Env::new();
    let o = env.run(&[
        "inventory",
        "--json",
        "--from-cmd",
        "printf 'h1\\n\\n# comment\\n  h2  \\n'",
    ]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let v = o.json();
    assert_eq!(aliases(&v), vec!["h1", "h2"], "{v}");
    assert_eq!(host(&v, "h2")["source"], "from_cmd");
    assert_eq!(host(&v, "h2")["hostname"], "h2");
}

#[test]
fn inventory_from_cmd_nonzero_is_undetermined() {
    let env = Env::new();
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n");
    let o = env.run(&["inventory", "--json", "--from-cmd", "echo h1; exit 3"]);
    assert_eq!(o.code, 4, "--from-cmd exit 3 must exit 4: {}", o.dbg());
    assert!(
        !o.stdout.contains("\"h1\""),
        "output of a failed --from-cmd must not be used: {}",
        o.dbg()
    );
}

#[test]
fn select_unknown_alias_is_refused_and_writes_nothing() {
    let env = Env::new();
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n");
    let o = env.run(&["select", "web1", "nosuch"]);
    assert_eq!(o.code, 3, "unknown alias must be refused (3): {}", o.dbg());
    assert!(
        !env.hostaudit_dir().join("selection.toml").exists(),
        "selection.toml written despite refusal"
    );
    assert!(
        o.stderr.contains("nosuch"),
        "refusal must name the alias: {}",
        o.dbg()
    );
}

#[test]
fn audit_without_selection_is_refused_and_never_connects() {
    let mut env = Env::new();
    env.ssh_config("Host web1\n  HostName 10.0.0.1\n");
    env.ssh_host("web1", &clean_host());
    env.osv_feed(&clean_osv());
    let o = env.run(&["audit", "--json"]);
    assert_eq!(
        o.code,
        3,
        "audit with no selection must be refused (3): {}",
        o.dbg()
    );
    assert!(
        env.ssh_log().is_empty(),
        "ssh was invoked: {:?}",
        env.ssh_log()
    );
}

#[test]
fn select_all_resolves_against_inventory() {
    let mut env = Env::new();
    env.ssh_config(
        "Host web1\n  HostName 10.0.0.1\nHost web2\n  HostName 10.0.0.2\nHost *\n  User x\n",
    );
    env.ssh_host("web1", &clean_host());
    env.ssh_host("web2", &clean_host());
    env.osv_feed(&clean_osv());
    let o = env.run(&["select", "--all"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let sel = std::fs::read_to_string(env.hostaudit_dir().join("selection.toml")).unwrap();
    let t: toml::Value = toml::from_str(&sel).unwrap();
    let mut got: Vec<String> = t["host"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["alias"].as_str().unwrap().to_string())
        .collect();
    got.sort();
    assert_eq!(got, vec!["web1", "web2"], "{sel}");

    let a = env.run(&["audit", "--json"]);
    assert_eq!(a.code, 0, "{}", a.dbg());
    let fs = a.findings();
    assert_eq!(verdict(&fs, "web1", "B1"), "clean");
    assert_eq!(verdict(&fs, "web2", "B1"), "clean");
}

#[test]
fn select_all_on_empty_inventory_is_refused_and_audit_refused() {
    let mut env = Env::new();
    env.ssh_fake_only();
    env.osv_feed(&clean_osv());
    let o = env.run(&["select", "--all"]);
    assert_eq!(
        o.code,
        3,
        "--all on empty inventory must be refused (3): {}",
        o.dbg()
    );
    assert!(!env.hostaudit_dir().join("selection.toml").exists());
    let a = env.run(&["audit", "--json"]);
    assert_eq!(a.code, 3, "audit must be refused (3): {}", a.dbg());
    assert!(env.ssh_log().is_empty(), "ssh invoked: {:?}", env.ssh_log());
}

#[test]
fn unselected_host_is_never_passed_to_ssh() {
    let mut env = Env::new();
    env.ssh_config("Host web1\n  HostName 10.0.0.1\nHost web2\n  HostName 10.0.0.2\n");
    env.ssh_host("web1", &clean_host());
    env.ssh_host("web2", &clean_host());
    env.osv_feed(&clean_osv());
    let s = env.run(&["select", "web1"]);
    assert_eq!(s.code, 0, "{}", s.dbg());
    let a = env.run(&["audit", "--json"]);
    assert_eq!(a.code, 0, "{}", a.dbg());
    let log = env.ssh_log();
    assert!(!log.is_empty(), "web1 was never contacted");
    for argv in &log {
        assert_eq!(argv[argv.len() - 2], "web1", "destination: {argv:?}");
        for arg in argv {
            assert!(
                !arg.contains("web2") && !arg.contains("10.0.0.2"),
                "unselected host leaked into ssh argv: {argv:?}"
            );
        }
    }
    assert!(
        a.findings().iter().all(|f| f["target"] != "web2"),
        "{}",
        a.stdout
    );
}

#[test]
fn inventory_includes_ec2_instances_with_aws_flag() {
    let mut env = Env::new();
    env.aws_clean_account();
    let o = env.run(&["inventory", "--json", "--aws"]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let v = o.json();
    let h = host(&v, "i-0123web");
    assert_eq!(h["source"], "ec2");
    assert_eq!(h["hostname"], "198.51.100.10");
    assert_eq!(h["public_ip"], "198.51.100.10");
    assert_eq!(h["private_ip"], "10.1.0.5");
    assert_eq!(h["region"], "ap-northeast-1");
    assert_eq!(h["name"], "web");
}

#[test]
fn inventory_aws_with_expired_login_is_undetermined() {
    let mut env = Env::new();
    env.aws_resp(
        "sts.get-caller-identity",
        fail(
            255,
            "Error when retrieving token from sso: Token has expired and refresh failed\n",
        ),
    );
    let o = env.run(&["inventory", "--json", "--aws"]);
    assert_eq!(o.code, 4, "{}", o.dbg());
}
