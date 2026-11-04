//! Post-review fail-open fixes for 8d92ab99 (CONTRACT.md §12.10, every item
//! marked [choice, post-review 2026-11-04]). Each test names the item number.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn reason_of(f: &Value) -> String {
    f["reason"].as_str().unwrap_or("").to_string()
}

fn host(over: &[(&str, Resp)]) -> BTreeMap<String, Resp> {
    let mut h = clean_host();
    for (k, r) in over {
        set(&mut h, k, r.clone());
    }
    h
}

fn audit(h: &BTreeMap<String, Resp>, feeds: &[(&str, Vec<Value>)]) -> (Env, Out, Vec<Value>) {
    let mut env = Env::new();
    env.eol_ubuntu_supported();
    for (slug, c) in feeds {
        env.eol_feed(slug, c);
    }
    let o = env.audit_one(h);
    let fs = o.findings();
    (env, o, fs)
}

fn rl(pid: u32) -> String {
    format!("sudo -n LC_ALL=C readlink /proc/{pid}/exe")
}

fn listen(name: &str, pid: u32) -> Resp {
    ok(&format!(
        "LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:((\"{name}\",pid={pid},fd=19))\n"
    ))
}

fn no_path(p: &str) -> Resp {
    fail(
        1,
        &format!("dpkg-query: no path found matching pattern {p}\n"),
    )
}

// ================================================================ 1. S5

fn s5_with_file(name: &str, body: &str) -> Value {
    let h = host(&[
        (W18_LS, ok(&format!("{name}\n"))),
        (
            &format!("LC_ALL=C cat /etc/apt/sources.list.d/{name}"),
            ok(body),
        ),
    ]);
    let (_, _, fs) = audit(&h, &[]);
    finding(&fs, "web1", "S5")
}

#[test]
fn item1_s5_file_read_but_no_repository_url_is_undetermined() {
    for (name, body) in [
        ("empty.list", ""),
        ("comments.list", "# nothing active\n#deb http://x/\n"),
        (
            "nouris.sources",
            "Types: deb\nSuites: jammy\nComponents: main\n",
        ),
    ] {
        let f = s5_with_file(name, body);
        assert_eq!(f["verdict"], "undetermined", "{name}: {f}");
        assert!(!reason_of(&f).is_empty(), "{f}");
    }
}

#[test]
fn item1_s5_deb822_field_names_are_case_insensitive() {
    for field in ["uris", "URIS", "Uris", "URIs"] {
        let body = format!("Types: deb\n{field}: https://foreign.example/\nSuites: jammy\n");
        let f = s5_with_file("x.sources", &body);
        assert_eq!(f["verdict"], "warning", "field `{field}`: {f}");
        assert!(evidence_text(&f).contains("foreign.example"), "{f}");
    }
}

#[test]
fn item1_s5_deb822_values_may_continue_on_following_lines() {
    // second URL on a continuation line
    let body = "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu\n https://foreign.example/ubuntu\nSuites: jammy\n";
    let f = s5_with_file("x.sources", body);
    assert_eq!(f["verdict"], "warning", "{f}");
    assert!(evidence_text(&f).contains("foreign.example"), "{f}");
    // value starts on the next line
    let body = "Types: deb\nURIs:\n https://foreign.example/ubuntu\nSuites: jammy\n";
    let f = s5_with_file("y.sources", body);
    assert_eq!(f["verdict"], "warning", "{f}");
}

// ================================================================ 2. KEV

fn cve_run(
    kev_body: Option<&str>,
    tweak: impl FnOnce(&mut BTreeMap<String, Resp>),
) -> (Env, Value) {
    let mut env = Env::new();
    let mut h = clean_host();
    set(
        &mut h,
        W2,
        ok(
            "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n",
        ),
    );
    tweak(&mut h);
    env.one_selected_host(&h);
    env.osv_feed(&json!([osv_rec(
        "UBUNTU-CVE-2024-0002",
        &[],
        Some(("Ubuntu", "high")),
        "openssh",
        Some("1:9.9p1-1")
    )]));
    if let Some(b) = kev_body {
        std::fs::write(env.fakes.join("kev.json"), b).unwrap();
    }
    env.eol_ubuntu_supported();
    let o = env.run(&["audit", "--json"]);
    let b5 = finding(&o.findings(), "web1", "B5");
    (env, b5)
}

fn first_cve(b5: &Value) -> Value {
    b5["cves"].as_array().unwrap_or_else(|| panic!("{b5}"))[0].clone()
}

#[test]
fn item2_empty_kev_list_is_unknown_never_not_in_kev() {
    for body in [r#"{"vulnerabilities":[]}"#, "{}", "[]"] {
        let (_, b5) = cve_run(Some(body), |_| {});
        let c = first_cve(&b5);
        assert_eq!(c["kev"], "unknown", "KEV body {body}: {c}");
        assert!(all_text(&b5).contains("KEV 不明"), "{body}: {b5}");
        assert_eq!(
            b5["verdict"], "violation",
            "B5 verdict is independent of KEV"
        );
    }
}

#[test]
fn item2_nonempty_kev_without_the_cve_is_still_no() {
    let (_, b5) = cve_run(
        Some(r#"{"vulnerabilities":[{"cveID":"CVE-1999-0001"}]}"#),
        |_| {},
    );
    assert_eq!(first_cve(&b5)["kev"], "no", "{b5}");
}

// ================================================================ 3. (deleted)

#[test]
fn item3_deleted_suffix_keeps_the_product_identity_but_s8_is_undetermined() {
    let h = host(&[
        (W12, listen("nginx", 900)),
        (&rl(900), ok("/usr/sbin/nginx (deleted)\n")),
    ]);
    let (env, o, fs) = audit(&h, &[("nginx", vec![cyc("1.27", 400)])]);
    let f = finding(&fs, "web1", "S8");
    assert_eq!(
        f["verdict"], "undetermined",
        "the item must be kept as Undetermined, not dropped (NotApplicable/Clean): {f}"
    );
    assert!(
        all_text(&f).contains("nginx"),
        "product identified from the basename: {f}"
    );
    let cmds = remote_cmds(&env);
    assert!(cmds.iter().any(|c| *c == rl(900)), "liveness: {cmds:?}");
    assert!(
        !cmds.iter().any(|c| c.contains("(deleted)")),
        "unsafe path reached ssh: {cmds:?}"
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.starts_with("LC_ALL=C /usr/sbin/nginx")),
        "W22 must not run for an unsafe path: {cmds:?}"
    );
    assert_eq!(o.code, 1);
}

// ================================================================ 4. versioned interpreters

#[test]
fn item4_versioned_python_basename_maps_to_python_and_w22_runs() {
    for (base, ver, cycle) in [
        ("python3.9", "Python 3.9.18", "3.9"),
        ("python3.12", "Python 3.12.3", "3.12"),
        ("python3", "Python 3.11.2", "3.11"),
    ] {
        let path = format!("/usr/local/bin/{base}");
        let h = host(&[
            (W12, listen(base, 900)),
            (&rl(900), ok(&format!("{path}\n"))),
            (&format!("LC_ALL=C dpkg -S {path}"), no_path(&path)),
            (
                &format!("LC_ALL=C {path} --version"),
                ok(&format!("{ver}\n")),
            ),
        ]);
        let (env, _, fs) = audit(&h, &[("python", vec![cyc(cycle, -100)])]);
        let f = finding(&fs, "web1", "S8");
        assert_eq!(f["verdict"], "violation", "{base}: {f}");
        assert!(
            remote_cmds(&env)
                .iter()
                .any(|c| *c == format!("LC_ALL=C {path} --version")),
            "{base}: W22 not run"
        );
    }
}

// ================================================================ 5. WN4

fn win(over: &[(&str, Resp)]) -> BTreeMap<String, Resp> {
    let mut h = win_host();
    for (k, r) in over {
        h.insert((*k).to_string(), r.clone());
    }
    h
}

fn win_feed() -> Value {
    json!({"cycle":"2022","latest":"10.0.20348","eol":ymd(NOW+1500*DAY),"extendedSupport":ymd(NOW+3000*DAY)})
}

fn wn(over: &[(&str, Resp)], rule: &str) -> Value {
    let mut env = Env::new();
    env.eol_feed("windows-server", &[win_feed()]);
    let o = env.audit_one(&win(over));
    finding(&o.findings(), "web1", rule)
}

fn wn1_with(cycles: &[Value], over: &[(&str, Resp)]) -> Value {
    let mut env = Env::new();
    env.eol_feed("windows-server", cycles);
    let o = env.audit_one(&win(over));
    finding(&o.findings(), "web1", "WN1")
}

#[test]
fn item5_wn4_enabled_values() {
    for (v, want) in [
        ("true", "clean"),
        ("1", "clean"),
        ("false", "violation"),
        ("0", "violation"),
        ("2", "undetermined"),
        ("\"maybe\"", "undetermined"),
        ("null", "undetermined"),
    ] {
        let r = ok(&format!(
            r#"[{{"Name":"Domain","Enabled":1}},{{"Name":"Public","Enabled":{v}}}]"#
        ));
        let f = wn(&[(WN4, r)], "WN4");
        assert_eq!(f["verdict"], want, "Enabled={v}: {f}");
    }
}

#[test]
fn item5_wn4_missing_enabled_field_is_undetermined() {
    let r = ok(r#"[{"Name":"Domain","Enabled":1},{"Name":"Public"}]"#);
    let f = wn(&[(WN4, r)], "WN4");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

// ================================================================ 6. WN1

#[test]
fn item6_wn1_prefix_needs_a_dot_boundary() {
    // "10.0.203480" starts with "10.0.20348" but is a different build.
    let wrong =
        json!({"cycle":"X","latest":"10.0.203480","eol":ymd(NOW-500*DAY),"extendedSupport":false});
    let f = wn1_with(&[wrong], &[]);
    assert_eq!(
        f["verdict"], "undetermined",
        "no dot-boundary match -> no cycle: {f}"
    );
}

#[test]
fn item6_wn1_more_than_one_matching_cycle_is_undetermined() {
    let a =
        json!({"cycle":"A","latest":"10.0.20348","eol":ymd(NOW-500*DAY),"extendedSupport":false});
    let b = json!({"cycle":"B","latest":"10.0.20348.2527","eol":ymd(NOW+900*DAY),"extendedSupport":false});
    let f = wn1_with(&[a, b], &[]);
    assert_eq!(f["verdict"], "undetermined", "ambiguous: {f}");
}

#[test]
fn item6_wn1_single_dot_boundary_match_still_works() {
    let a = json!({"cycle":"A","latest":"10.0.20348.2527","eol":ymd(NOW-500*DAY),"extendedSupport":false});
    let f = wn1_with(&[a], &[]);
    assert_eq!(f["verdict"], "violation", "{f}");
}

// ================================================================ 7. WN2 / WN3 future dates

#[test]
fn item7_future_installed_on_is_undetermined_for_wn2_and_wn3() {
    let future = ok(&format!(
        r#"{{"HotFixID":"KB1","InstalledOn":"{}T00:00:00"}}"#,
        ymd(NOW + 5 * DAY)
    ));
    let f = wn(&[(WN2, future.clone())], "WN2");
    assert_eq!(f["verdict"], "undetermined", "negative age: {f}");
    assert!(!reason_of(&f).is_empty());
    let f = wn(&[(WN2, future), (WN3, ok("True\r\n"))], "WN3");
    assert_eq!(f["verdict"], "undetermined", "{f}");
}

// ================================================================ 8. 127 + loader error

const LOADER: &str = "node: error while loading shared libraries: libfoo.so.1: cannot open shared object file: No such file or directory\n";

#[test]
fn item8_w21_exit_127_with_loader_error_is_undetermined_not_absent() {
    let h = host(&[(&w21("node --version"), fail(127, LOADER))]);
    let (_, o, fs) = audit(&h, &[("nodejs", vec![cyc("25", 400)])]);
    let f = finding(&fs, "web1", "S8");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert_eq!(o.code, 1);
}

#[test]
fn item8_plain_127_command_not_found_is_still_absent() {
    let (_, _, fs) = audit(&clean_host(), &[]);
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
}

#[test]
fn item8_w22_exit_127_with_loader_error_is_undetermined() {
    let h = host(&[
        (W12, listen("node", 900)),
        (&rl(900), ok("/opt/x/bin/node\n")),
        (
            "LC_ALL=C dpkg -S /opt/x/bin/node",
            no_path("/opt/x/bin/node"),
        ),
        ("LC_ALL=C /opt/x/bin/node --version", fail(127, LOADER)),
    ]);
    let (_, _, fs) = audit(&h, &[("nodejs", vec![cyc("25", 400)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
}

#[test]
fn item8_s9_w21_style_loader_error_does_not_hide_a_dpkg_item() {
    // dpkg item exists and is fine; the PATH probe is broken -> S8 undetermined, S9 unaffected.
    let h = host(&[
        (
            W2,
            ok(
                "openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n\
linux-image-5.15.0-100-generic\t5.15.0-100.110\tlinux\t5.15.0-100.110\n\
nodejs\t22.1.0-1\tnodejs\t22.1.0-1\n",
            ),
        ),
        (&w21("node --version"), fail(127, LOADER)),
    ]);
    let (_, _, fs) = audit(&h, &[("nodejs", vec![cyc("22", 400)])]);
    assert_eq!(verdict(&fs, "web1", "S9"), "clean");
    assert_eq!(verdict(&fs, "web1", "S8"), "undetermined");
}

// ================================================================ 9. malformed Inst origin group

#[test]
fn item9_inst_line_without_a_usable_origin_group_is_undetermined() {
    for line in [
        "Inst libssl3 [3.0.2-0ubuntu1.10]\n",
        "Inst libssl3 [3.0.2-0ubuntu1.10] (3.0.2-0ubuntu1.12\n",
        "Inst libssl3 [3.0.2-0ubuntu1.10] ()\n",
    ] {
        let h = host(&[(W14, ok(line))]);
        let (_, o, fs) = audit(&h, &[]);
        let f = finding(&fs, "web1", "S1");
        assert_eq!(f["verdict"], "undetermined", "{line:?}: {f}");
        assert!(!reason_of(&f).is_empty(), "{f}");
        assert_eq!(o.code, 1);
    }
}

#[test]
fn item9_malformed_kernel_inst_line_makes_k4_undetermined() {
    let h = host(&[(
        W14,
        ok("Inst linux-image-5.15.0-105-generic [5.15.0-100.110]\n"),
    )]);
    let (_, _, fs) = audit(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "K4"), "undetermined");
    assert_eq!(verdict(&fs, "web1", "S1"), "undetermined");
}

#[test]
fn item9_wellformed_non_security_lines_stay_clean() {
    let h = host(&[(
        W14,
        ok("Inst vim [2:8.2-1] (2:8.2-2 Ubuntu:22.04/jammy-updates [amd64])\n"),
    )]);
    let (_, _, fs) = audit(&h, &[]);
    assert_eq!(verdict(&fs, "web1", "S1"), "clean");
    assert_eq!(verdict(&fs, "web1", "K4"), "clean");
}

// ================================================================ 10. empty W12

#[test]
fn item10_empty_w12_output_makes_exposure_unknown() {
    let (_, b5) = cve_run(
        Some(r#"{"vulnerabilities":[{"cveID":"CVE-1999-0001"}]}"#),
        |h| {
            set(h, W12, ok(""));
        },
    );
    let c = first_cve(&b5);
    assert_eq!(c["exposed"], "unknown", "{c}");
    assert_eq!(c["priority"], "P1", "high + unknown exposure: {c}");
}

// ================================================================ 11. K1

#[test]
fn item11_feed_without_a_cycle_for_version_id_is_undetermined_not_w17_fallback() {
    let mut env = Env::new();
    env.eol_feed("ubuntu", &[json!({"cycle":"20.04","eol":ymd(NOW+400*DAY)})]);
    // W17 would say clean (jammy supported) if the fallback wrongly kicked in.
    let o = env.audit_one(&clean_host());
    let f = finding(&o.findings(), "web1", "K1");
    assert_eq!(f["verdict"], "undetermined", "{f}");
    assert!(!reason_of(&f).is_empty());
}

#[test]
fn item11_w17_without_codename_shaped_lines_is_undetermined_not_violation() {
    for out in [
        "",
        "\n\n",
        "E: something went wrong here\n",
        "Usage: ubuntu-distro-info [options]\n",
    ] {
        let h = host(&[(W17U, ok(out))]);
        let mut env = Env::new(); // no ubuntu feed: fallback path
        let o = env.audit_one(&h);
        let f = finding(&o.findings(), "web1", "K1");
        assert_eq!(f["verdict"], "undetermined", "W17={out:?}: {f}");
    }
}

#[test]
fn item11_w17_with_codenames_but_not_ours_is_still_violation() {
    let h = host(&[(W17U, ok("focal\nnoble\n"))]);
    let mut env = Env::new();
    let o = env.audit_one(&h);
    assert_eq!(verdict(&o.findings(), "web1", "K1"), "violation");
}

// ================================================================ 12. usrmerge

#[test]
fn item12_usrmerge_alias_is_queried_and_exposure_is_yes() {
    let (env, b5) = cve_run(Some(r#"{"vulnerabilities":[]}"#), |h| {
        set(
            h,
            "LC_ALL=C dpkg -S /usr/sbin/sshd",
            no_path("/usr/sbin/sshd"),
        );
        set(
            h,
            "LC_ALL=C dpkg -S /sbin/sshd",
            ok("openssh-server: /sbin/sshd\n"),
        );
    });
    let c = first_cve(&b5);
    assert_eq!(c["exposed"], "yes", "{c}");
    assert_eq!(c["priority"], "P1", "{c}");
    let cmds = remote_cmds(&env);
    assert!(
        cmds.iter().any(|x| x == "LC_ALL=C dpkg -S /sbin/sshd"),
        "stripped-prefix query missing: {cmds:?}"
    );
}

#[test]
fn item12_exposure_is_never_no_merely_because_of_usrmerge() {
    // First query says no path found, the alias query fails (ssh down): cannot know -> unknown.
    let (_, b5) = cve_run(Some(r#"{"vulnerabilities":[]}"#), |h| {
        set(
            h,
            "LC_ALL=C dpkg -S /usr/sbin/sshd",
            no_path("/usr/sbin/sshd"),
        );
        h.remove("LC_ALL=C dpkg -S /sbin/sshd");
    });
    assert_eq!(first_cve(&b5)["exposed"], "unknown", "{b5}");
}

#[test]
fn item12_both_forms_no_path_found_means_not_dpkg_managed() {
    let h = host(&[
        (W12, listen("node", 900)),
        (&rl(900), ok("/usr/bin/node\n")),
        ("LC_ALL=C dpkg -S /usr/bin/node", no_path("/usr/bin/node")),
        ("LC_ALL=C dpkg -S /bin/node", no_path("/bin/node")),
        ("LC_ALL=C /usr/bin/node --version", ok("v25.6.0\n")),
    ]);
    let (env, _, fs) = audit(&h, &[("nodejs", vec![cyc("25", -30)])]);
    assert_eq!(verdict(&fs, "web1", "S8"), "violation");
    let cmds = remote_cmds(&env);
    assert!(
        cmds.iter().any(|c| c == "LC_ALL=C dpkg -S /bin/node"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c == "LC_ALL=C /usr/bin/node --version"),
        "W22 runs against the original path: {cmds:?}"
    );
}

#[test]
fn item12_usrmerge_alias_owned_by_dpkg_is_not_an_s8_item() {
    let h = host(&[
        (W12, listen("node", 900)),
        (&rl(900), ok("/usr/bin/node\n")),
        ("LC_ALL=C dpkg -S /usr/bin/node", no_path("/usr/bin/node")),
        ("LC_ALL=C dpkg -S /bin/node", ok("nodejs: /bin/node\n")),
    ]);
    let (env, _, fs) = audit(&h, &[("nodejs", vec![cyc("25", -30)])]);
    assert_ne!(
        verdict(&fs, "web1", "S8"),
        "violation",
        "dpkg owns it via the alias"
    );
    assert!(
        !remote_cmds(&env)
            .iter()
            .any(|c| c.starts_with("LC_ALL=C /usr/bin/node")),
        "dpkg-owned executables are not executed"
    );
}
