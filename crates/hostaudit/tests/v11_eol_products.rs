//! v1.1 canon §4 / §4.1: S8 (non-dpkg processing systems and containers past
//! upstream support) and S9 (apt-installed ones), both driven by endoflife.date
//! (CONTRACT.md §12.7). Feeds are `file://` fakes; nothing touches the network.
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

/// One selected Ubuntu host `h`; endoflife feeds `feeds` (slug -> cycles).
fn run(h: &BTreeMap<String, Resp>, feeds: &[(&str, Vec<Value>)]) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    for (slug, cycles) in feeds {
        env.eol_feed(slug, cycles);
    }
    let o = env.audit_one(h);
    let fs = o.findings();
    (env, o, fs)
}

fn dpkg(extra: &str) -> Resp {
    ok(&format!(
        "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n{extra}"
    ))
}

fn dpkg_line(pkg: &str, ver: &str) -> String {
    format!("{pkg}\t{ver}\t{pkg}\t{ver}\n")
}

fn host(over: &[(String, Resp)]) -> BTreeMap<String, Resp> {
    let mut h = clean_host();
    for (k, r) in over {
        h.insert(k.clone(), r.clone());
    }
    h
}

fn path_cmd(cmd: &str, r: Resp) -> (String, Resp) {
    (w21(cmd), r)
}

fn ev_text(f: &Value) -> String {
    format!("{} {}", evidence_text(f), f["fix"].as_str().unwrap_or(""))
}

// ---------------------------------------------------------------- S9

#[test]
fn s9_apt_installed_eol_product_is_violation() {
    let h = host(&[(
        W2.into(),
        dpkg(&dpkg_line("nodejs", "20.20.2-1nodesource1")),
    )]);
    let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("20", -250)])]);
    let f = finding(&fs, "web1", "S9");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("nodejs"), "{f}");
    assert!(evidence_text(&f).contains("20.20.2"), "{f}");
    assert!(!fix_of(&f).trim().is_empty(), "{f}");
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert_eq!(
        f["eol_fetched_at"], NOW,
        "endoflife fetch time is recorded: {f}"
    );
}

#[test]
fn s9_supported_product_is_clean() {
    let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "22.1.0-1nodesource1")))]);
    let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("22", 400)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "clean");
    assert_eq!(o.code, 0, "{}", o.dbg());
}

#[test]
fn s9_eol_flag_forms() {
    for (eol, want) in [(json!(true), "violation"), (json!(false), "clean")] {
        let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "22.1.0-1")))]);
        let (_, _, fs) = run(&h, &[("nodejs", vec![json!({"cycle":"22","eol":eol})])]);
        assert_eq!(verdict(&fs, "web1", "S9"), want, "eol={eol}");
    }
}

#[test]
fn s9_every_dpkg_name_maps_to_its_product_and_cycle() {
    let table = [
        ("nodejs", "20.20.2-1nodesource1", "nodejs", "20"),
        ("python3.12", "3.12.3-1ubuntu0.8", "python", "3.12"),
        (
            "openjdk-21-jre-headless",
            "21.0.5+11-1ubuntu1",
            "java",
            "21",
        ),
        ("ruby3.2", "3.2.3-1ubuntu0.1", "ruby", "3.2"),
        ("docker.io", "29.1.3-0ubuntu1", "docker-engine", "29"),
        (
            "docker-ce",
            "5:27.3.1-1~ubuntu.22.04~jammy",
            "docker-engine",
            "27",
        ),
        ("containerd", "2.2.1-0ubuntu1", "containerd", "2.2"),
        ("containerd.io", "1.7.2-1", "containerd", "1.7"),
        ("openssl", "3.0.13-0ubuntu3.5", "openssl", "3.0"),
        ("libssl3", "3.0.13-0ubuntu3.5", "openssl", "3.0"),
        ("php8.3-cli", "8.3.6-0ubuntu0.24.04.4", "php", "8.3"),
        ("nginx", "1.24.0-2ubuntu7", "nginx", "1.24"),
        ("apache2", "2.4.58-1ubuntu8", "apache-http-server", "2.4"),
        ("postgresql-16", "16.4-0ubuntu0.24.04.1", "postgresql", "16"),
        (
            "mysql-server-8.0",
            "8.0.39-0ubuntu0.24.04.2",
            "mysql",
            "8.0",
        ),
        (
            "mysql-server-core-8.0",
            "8.0.39-0ubuntu0.24.04.2",
            "mysql",
            "8.0",
        ),
        (
            "mariadb-server",
            "1:10.11.8-0ubuntu0.24.04.1",
            "mariadb",
            "10.11",
        ),
        ("redis-server", "5:7.0.15-1build2", "redis", "7.0"),
        ("mongodb-org-server", "7.0.14", "mongodb", "7.0"),
        ("tomcat10", "10.1.16-1ubuntu0.1", "tomcat", "10.1"),
        ("golang-1.22-go", "1.22.2-2ubuntu0.4", "go", "1.22"),
    ];
    for (pkg, ver, slug, cycle) in table {
        // Only THIS product's feed exists: a wrong slug -> Undetermined, a wrong cycle -> Undetermined.
        let h = host(&[(W2.into(), dpkg(&dpkg_line(pkg, ver)))]);
        let (_, _, fs) = run(&h, &[(slug, vec![cyc(cycle, -100)])]);
        let f = finding(&fs, "web1", "S9");
        assert_eq!(
            f["verdict"], "violation",
            "{pkg} {ver} -> {slug}/{cycle}: {f}"
        );
    }
}

#[test]
fn cycle_is_the_longest_dot_boundary_prefix() {
    // containerd 2.2.1: cycles "2" (EOL) and "2.2" (supported) -> must pick "2.2".
    let h = host(&[(W2.into(), dpkg(&dpkg_line("containerd", "2.2.1-0ubuntu1")))]);
    let (_, _, fs) = run(&h, &[("containerd", vec![cyc("2", -100), cyc("2.2", 300)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "clean");
    // 2.20.1 must NOT match cycle "2.2" (dot boundary), only "2".
    let h = host(&[(W2.into(), dpkg(&dpkg_line("containerd", "2.20.1-1")))]);
    let (_, _, fs) = run(&h, &[("containerd", vec![cyc("2", -100), cyc("2.2", 300)])]);
    assert_eq!(
        verdict(&fs, "web1", "S9"),
        "violation",
        "2.20 is not in 2.2"
    );
}

#[test]
fn s9_undetermined_when_cycle_or_feed_is_missing() {
    let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "20.20.2-1")))]);
    // feed has no matching cycle
    let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("18", -100)])]);
    let f = finding(&fs, "web1", "S9");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(!reason_of(&f).is_empty());
    assert_eq!(o.code, 1, "{}", o.dbg());
    // no feed at all
    let (_, _, fs) = run(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "S9"), "undetermined");
}

#[test]
fn s9_unparseable_dpkg_version_is_undetermined() {
    let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "weird-version")))]);
    let (_, _, fs) = run(&h, &[("nodejs", vec![cyc("20", -100)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "undetermined");
}

#[test]
fn s9_endoflife_cache_is_used_while_fresh_then_dropped() {
    let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "20.20.2-1")))]);
    let (env, _, fs) = run(&h, &[("nodejs", vec![cyc("20", -100)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "violation");
    env.eol_remove("nodejs");
    let t1 = (NOW + 3600).to_string();
    let o = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &t1)]);
    let f = finding(&o.findings(), "web1", "S9");
    assert_eq!(
        f["verdict"], "violation",
        "1h-old endoflife cache must be used: {f}"
    );
    assert_eq!(f["eol_fetched_at"], NOW, "{f}");
    let t25 = (NOW + 25 * 3600).to_string();
    let o = env.run_with(&["audit", "--json"], &[("HOSTAUDIT_NOW", &t25)]);
    assert_eq!(
        verdict(&o.findings(), "web1", "S9"),
        "undetermined",
        "25h-old cache exceeds the shared 24h limit"
    );
}

// ---------------------------------------------------------------- dpkg vs PATH

#[test]
fn dpkg_node_and_path_node_are_reported_separately_by_s9_and_s8() {
    // The 2026-11-04 measured case: dpkg nodejs 20 (nodesource) + PATH node v25.
    let h = host(&[
        (
            W2.into(),
            dpkg(&dpkg_line("nodejs", "20.20.2-1nodesource1")),
        ),
        path_cmd("node --version", ok("v25.6.0\n")),
    ]);
    let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("20", -250), cyc("25", -30)])]);
    let s9 = finding(&fs, "web1", "S9");
    let s8 = finding(&fs, "web1", "S8");
    assert_eq!(s9["verdict"], "violation", "{s9}");
    assert_eq!(s8["verdict"], "violation", "{s8}");
    assert!(evidence_text(&s9).contains("20.20.2"), "{s9}");
    assert!(
        !evidence_text(&s9).contains("25.6.0"),
        "S9 is the dpkg item only: {s9}"
    );
    assert!(evidence_text(&s8).contains("25.6.0"), "{s8}");
    assert!(
        !evidence_text(&s8).contains("20.20.2"),
        "S8 is the PATH item only: {s8}"
    );
    assert_eq!(o.code, 1);
}

#[test]
fn path_binary_equal_to_the_dpkg_version_is_not_a_second_item() {
    // [choice] same cycle as the dpkg package -> it IS the dpkg binary.
    let h = host(&[
        (
            W2.into(),
            dpkg(&dpkg_line("nodejs", "20.20.2-1nodesource1")),
        ),
        path_cmd("node --version", ok("v20.20.2\n")),
    ]);
    let (_, _, fs) = run(&h, &[("nodejs", vec![cyc("20", -250)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "violation");
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
}

// ---------------------------------------------------------------- S8: PATH

#[test]
fn s8_every_monitored_version_command_is_read_from_stdout_or_stderr() {
    let maria = "mariadbd  Ver 10.11.8-MariaDB-0ubuntu0.24.04.1 for debian-linux-gnu on x86_64 (Ubuntu 24.04)\n";
    // (command, stdout, stderr, slug, cycle)
    let t: Vec<(&str, &str, &str, &str, &str)> = vec![
        ("node --version", "v25.6.0\n", "", "nodejs", "25"),
        ("python3 --version", "Python 3.14.0\n", "", "python", "3.14"),
        ("python2 --version", "", "Python 2.7.18\n", "python", "2.7"),
        (
            "java -version",
            "",
            "openjdk version \"21.0.5\" 2024-10-15\nOpenJDK Runtime Environment (build 21.0.5+11)\n",
            "java",
            "21",
        ),
        ("ruby -v", "ruby 3.2.3p157 (2024-01-30 revision 1) [x86_64-linux]\n", "", "ruby", "3.2"),
        ("docker --version", "Docker version 29.1.3, build 1234\n", "", "docker-engine", "29"),
        (
            "containerd --version",
            "containerd github.com/containerd/containerd/v2 v2.2.1 abc\n",
            "",
            "containerd",
            "2.2",
        ),
        (
            "openssl version",
            "OpenSSL 3.0.13 30 Jan 2024 (Library: OpenSSL 3.0.13 30 Jan 2024)\n",
            "",
            "openssl",
            "3.0",
        ),
        ("php -v", "PHP 8.3.6 (cli) (built: Jan 1 2024) (NTS)\n", "", "php", "8.3"),
        ("nginx -v", "", "nginx version: nginx/1.24.0\n", "nginx", "1.24"),
        (
            "apache2 -v",
            "Server version: Apache/2.4.58 (Ubuntu)\nServer built: x\n",
            "",
            "apache-http-server",
            "2.4",
        ),
        ("httpd -v", "Server version: Apache/2.4.58 (Unix)\n", "", "apache-http-server", "2.4"),
        (
            "postgres --version",
            "postgres (PostgreSQL) 16.4 (Ubuntu 16.4-0ubuntu0.24.04.1)\n",
            "",
            "postgresql",
            "16",
        ),
        (
            "/usr/lib/postgresql/*/bin/postgres --version",
            "postgres (PostgreSQL) 15.8\n",
            "",
            "postgresql",
            "15",
        ),
        (
            "mysqld --version",
            "mysqld  Ver 8.0.39-0ubuntu0.24.04.2 for Linux on x86_64 ((Ubuntu))\n",
            "",
            "mysql",
            "8.0",
        ),
        (
            "mysqld --version",
            "mysqld  Ver 10.11.8-MariaDB for debian-linux-gnu on x86_64 (MariaDB)\n",
            "",
            "mariadb",
            "10.11",
        ),
        ("mariadbd --version", maria, "", "mariadb", "10.11"),
        (
            "redis-server --version",
            "Redis server v=7.0.15 sha=00000000:0 malloc=jemalloc-5.3.0 bits=64 build=abc\n",
            "",
            "redis",
            "7.0",
        ),
        ("mongod --version", "db version v7.0.14\nBuild Info: {}\n", "", "mongodb", "7.0"),
        (
            "catalina.sh version",
            "Server version: Apache Tomcat/10.1.16\nServer built: x\n",
            "",
            "tomcat",
            "10.1",
        ),
        ("go version", "go version go1.22.2 linux/amd64\n", "", "go", "1.22"),
    ];
    for (cmd, out, err, slug, cycle) in t {
        let h = host(&[path_cmd(
            cmd,
            Resp {
                out: out.into(),
                err: err.into(),
                code: 0,
            },
        )]);
        let (_, _, fs) = run(&h, &[(slug, vec![cyc(cycle, -100)])]);
        let f = finding(&fs, "web1", "S8");
        assert_eq!(
            f["verdict"], "violation",
            "`{cmd}` -> {slug}/{cycle} (only that feed exists): {f}"
        );
        assert!(evidence_text(&f).contains(cycle), "{cmd}: {f}");
        assert_eq!(verdict(&fs, "web1", "S9"), "not_applicable", "{cmd}");
    }
}

#[test]
fn s8_supported_path_product_is_clean_and_close_expiry_is_warning() {
    let node = |days: i64| {
        let h = host(&[path_cmd("node --version", ok("v25.6.0\n"))]);
        let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("25", days)])]);
        (o, finding(&fs, "web1", "S8"))
    };
    let (_, f) = node(400);
    assert_eq!(f["verdict"], "clean", "{f}");
    let (o, f) = node(89);
    assert_eq!(f["verdict"], "warning", "within 90 days: {f}");
    assert!(!fix_of(&f).is_empty(), "{f}");
    assert_eq!(o.code, 0, "warning only -> exit 0: {}", o.dbg());
    let (_, f) = node(91);
    assert_eq!(f["verdict"], "clean", "outside 90 days: {f}");
}

#[test]
fn s8_unparseable_version_or_failed_command_is_undetermined() {
    let h = host(&[path_cmd("node --version", ok("something unexpected\n"))]);
    let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("25", -1)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
    assert_eq!(o.code, 1, "{}", o.dbg());
    // The command exists but fails with a non-"not found" error: not a clean/NA verdict.
    let h = host(&[path_cmd(
        "node --version",
        fail(1, "node: error while loading shared libraries\n"),
    )]);
    let (_, _, fs) = run(&h, &[("nodejs", vec![cyc("25", -1)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
    let h = host(&[path_cmd("node --version", ssh_down())]);
    let (_, _, fs) = run(&h, &[("nodejs", vec![cyc("25", -1)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
}

#[test]
fn s8_product_missing_from_endoflife_is_undetermined_not_clean() {
    let h = host(&[path_cmd(
        "go version",
        ok("go version go1.22.2 linux/amd64\n"),
    )]);
    let (_, _, fs) = run(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
}

#[test]
fn s8_mariadb_output_of_mysqld_is_not_attributed_to_mysql() {
    // Only the mysql feed exists; the binary says MariaDB -> must ask for the mariadb feed -> Undetermined.
    let h = host(&[path_cmd(
        "mysqld --version",
        ok("mysqld  Ver 10.11.8-MariaDB for debian-linux-gnu on x86_64 (MariaDB)\n"),
    )]);
    let (_, _, fs) = run(&h, &[("mysql", vec![cyc("10.11", -100)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
}

// ---------------------------------------------------------------- S8: process path (W12b -> W13 -> W22)

const LISTEN_NODE: &str = "LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:((\"node\",pid=900,fd=19))\n";

#[test]
fn s8_non_dpkg_listening_process_is_versioned_via_its_own_path() {
    let h = host(&[
        (W12.into(), ok(LISTEN_NODE)),
        (
            "sudo -n LC_ALL=C readlink /proc/900/exe".into(),
            ok("/opt/node-v25/bin/node\n"),
        ),
        (
            "LC_ALL=C dpkg -S /opt/node-v25/bin/node".into(),
            fail(
                1,
                "dpkg-query: no path found matching pattern /opt/node-v25/bin/node\n",
            ),
        ),
        (
            "LC_ALL=C /opt/node-v25/bin/node --version".into(),
            ok("v25.6.0\n"),
        ),
    ]);
    let (env, _, fs) = run(&h, &[("nodejs", vec![cyc("25", -30)])]);
    let f = finding(&fs, "web1", "S8");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(
        ev_text(&f).contains("/opt/node-v25/bin/node") || ev_text(&f).contains("25.6.0"),
        "{f}"
    );
    assert!(remote_cmds(&env)
        .iter()
        .any(|c| c == "LC_ALL=C /opt/node-v25/bin/node --version"));
}

#[test]
fn s8_dpkg_owned_process_is_not_run_and_not_an_s8_item() {
    let h = host(&[
        (W2.into(), dpkg(&dpkg_line("nodejs", "22.1.0-1"))),
        (W12.into(), ok(LISTEN_NODE)),
        (
            "sudo -n LC_ALL=C readlink /proc/900/exe".into(),
            ok("/usr/bin/node\n"),
        ),
        (
            "LC_ALL=C dpkg -S /usr/bin/node".into(),
            ok("nodejs: /usr/bin/node\n"),
        ),
    ]);
    let (env, _, fs) = run(&h, &[("nodejs", vec![cyc("22", 400)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
    assert_eq!(verdict(&fs, "web1", "S9"), "clean");
    assert!(
        !remote_cmds(&env)
            .iter()
            .any(|c| c.starts_with("LC_ALL=C /usr/bin/node")),
        "dpkg-owned executables are not executed"
    );
}

// ---------------------------------------------------------------- S8: containers

fn docker(out: &str) -> (String, Resp) {
    (W23.into(), ok(out))
}

#[test]
fn s8_container_image_past_eol_is_violation() {
    let h = host(&[docker("mysql:5.7\nmyapp:1.0\n")]);
    let (_, o, fs) = run(&h, &[("mysql", vec![cyc("5.7", -1200)])]);
    let f = finding(&fs, "web1", "S8");
    assert_eq!(f["verdict"], "violation", "{f}");
    assert!(evidence_text(&f).contains("mysql:5.7"), "{f}");
    assert!(
        !evidence_text(&f).contains("myapp"),
        "an image that is not a monitored product is ignored: {f}"
    );
    assert_eq!(o.code, 1);
}

#[test]
fn s8_container_tag_with_suffix_uses_its_leading_version() {
    let h = host(&[docker("redis:7.2-alpine\n")]);
    let (_, _, fs) = run(&h, &[("redis", vec![cyc("7.2", -10)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "violation");
}

#[test]
fn s8_container_without_a_usable_tag_is_undetermined() {
    for img in [
        "redis\n",
        "redis:latest\n",
        "postgres@sha256:0123456789abcdef\n",
    ] {
        let h = host(&[docker(img)]);
        let (_, o, fs) = run(
            &h,
            &[
                ("redis", vec![cyc("7.2", 400)]),
                ("postgresql", vec![cyc("16", 400)]),
            ],
        );
        let f = finding(&fs, "web1", "S8");
        assert_eq!(f["verdict"], "undetermined", "{img:?}: {f}");
        assert!(!reason_of(&f).is_empty(), "{f}");
        assert_eq!(o.code, 1, "{}", o.dbg());
    }
}

#[test]
fn s8_worst_item_decides_violation_over_undetermined_over_warning() {
    let feeds = [
        ("mysql", vec![cyc("5.7", -1200)]),
        ("redis", vec![cyc("7.2", 400)]),
        ("nginx", vec![cyc("1.27", 30)]),
    ];
    // violation + undetermined + warning -> violation
    let h = host(&[docker("mysql:5.7\nredis:latest\nnginx:1.27\n")]);
    let (_, _, fs) = run(&h, &feeds);
    let f = finding(&fs, "web1", "S8");
    assert_eq!(f["verdict"], "violation", "{f}");
    for item in ["mysql", "redis", "nginx"] {
        assert!(all_text(&f).contains(item), "{item} item missing: {f}");
    }
    // undetermined + warning -> undetermined
    let h = host(&[docker("redis:latest\nnginx:1.27\n")]);
    let (_, _, fs) = run(&h, &feeds);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
    // warning only -> warning
    let h = host(&[docker("nginx:1.27\n")]);
    let (_, _, fs) = run(&h, &feeds);
    assert_eq!(verdict(&fs, "web1", "S8"), "warning");
}

#[test]
fn s8_docker_absent_means_container_part_not_applicable() {
    // clean_host already has docker "command not found"
    let (_, o, fs) = run(&clean_host(), &[]);
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
    assert_eq!(o.code, 0, "{}", o.dbg());
    // the sudo wrapper form of the same condition
    let h = host(&[(W23.into(), fail(1, "sudo: docker: command not found\n"))]);
    let (_, _, fs) = run(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
    // docker present, nothing running
    let h = host(&[docker("")]);
    let (_, _, fs) = run(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
}

#[test]
fn s8_docker_absent_does_not_hide_a_path_violation() {
    let h = host(&[path_cmd("node --version", ok("v25.6.0\n"))]);
    let (_, _, fs) = run(&h, &[("nodejs", vec![cyc("25", -30)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "violation");
}

#[test]
fn s8_docker_unobservable_is_undetermined() {
    for r in [
        sudo_fail(),
        ssh_down(),
        fail(
            1,
            "permission denied while trying to connect to the Docker daemon socket\n",
        ),
    ] {
        let h = host(&[(W23.into(), r.clone())]);
        let (_, o, fs) = run(&h, &[]);
        let f = finding(&fs, "web1", "S8");
        assert_eq!(f["verdict"], "undetermined", "{r:?}: {f}");
        assert_eq!(o.code, 1, "{}", o.dbg());
    }
}

#[test]
fn s9_has_no_warning_band_near_eol() {
    for days in [30, 89] {
        let h = host(&[(W2.into(), dpkg(&dpkg_line("nodejs", "22.1.0-1nodesource1")))]);
        let (_, o, fs) = run(&h, &[("nodejs", vec![cyc("22", days)])]);
        let f = finding(&fs, "web1", "S9");
        assert_eq!(
            f["verdict"], "clean",
            "{days} days to EOL: S9 is clean, not warning: {f}"
        );
        assert_eq!(o.code, 0, "{}", o.dbg());
    }
}
