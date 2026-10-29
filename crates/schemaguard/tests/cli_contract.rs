// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! One test per prose claim in README.md / README.ja.md. Each test drives the
//! real binary (or reads the real packaging files) and asserts the specific
//! outcome the prose states.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("schemaguard-pc-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_in(home: &Path, args: &[&str], payload: &str) -> Out {
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_schemaguard")))
            .args(args)
            .current_dir(home)
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let o = child.wait_with_output().expect("binary runs");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn run_bytes(args: &[&str], payload: &[u8]) -> Out {
    let home = temp_dir();
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_schemaguard")))
            .args(args)
            .current_dir(&home)
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload);
    }
    let o = child.wait_with_output().expect("binary runs");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn run(args: &[&str], payload: &str) -> Out {
    run_in(&temp_dir(), args, payload)
}

fn json(out: &Out) -> Value {
    serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout is not a single JSON value ({e}): {:?}", out.stdout);
    })
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
    k.sort();
    k
}

const VALID_DECOMP: &str =
    r#"{"goal":"g","tasks":[{"id":"t1","title":"x","class":"serial","done_criteria":"d"}]}"#;
const BAD_DECOMP: &str = r#"{"tasks":[]}"#; // `goal` missing
const NESTED_BAD_DECOMP: &str = r#"{"goal":"g","tasks":[{"title":"x"}]}"#; // tasks[0].id missing
const PROBE_UNDET: &str = r#"{"name":"n","items_any":"not-an-array"}"#;

#[test]
fn claim_check_file_input() {
    for (payload, want_code) in [(VALID_DECOMP, 0), (BAD_DECOMP, 1)] {
        let dir = temp_dir();
        let f = dir.join("in.json");
        std::fs::write(&f, payload).unwrap();
        let via_file = run_in(
            &dir,
            &[
                "check",
                "--schema",
                "decomposition",
                "--file",
                f.to_str().unwrap(),
            ],
            "",
        );
        let via_stdin = run_in(&dir, &["check", "--schema", "decomposition"], payload);
        assert_eq!(via_file.code, want_code, "file: {}", via_file.stdout);
        assert_eq!(via_stdin.code, want_code, "stdin: {}", via_stdin.stdout);
        assert_eq!(json(&via_file), json(&via_stdin));
    }
}

#[test]
fn claim_check_output_keys() {
    let clean = run(&["check", "--schema", "decomposition"], VALID_DECOMP);
    assert_eq!(clean.code, 0);
    assert_eq!(
        keys(&json(&clean)),
        ["errors", "not_checked", "schema", "valid"]
    );

    let viol = run(&["check", "--schema", "decomposition"], BAD_DECOMP);
    assert_eq!(viol.code, 1);
    assert_eq!(
        keys(&json(&viol)),
        ["errors", "not_checked", "schema", "undetermined", "valid"]
    );

    let undet = run(&["check", "--schema", "undetermined-probe"], PROBE_UNDET);
    assert_eq!(undet.code, 2);
    assert_eq!(
        keys(&json(&undet)),
        ["errors", "not_checked", "schema", "undetermined", "valid"]
    );
}

#[test]
fn claim_check_exit_codes() {
    assert_eq!(
        run(&["check", "--schema", "decomposition"], VALID_DECOMP).code,
        0
    );
    assert_eq!(
        run(&["check", "--schema", "decomposition"], BAD_DECOMP).code,
        1
    );
    assert_eq!(
        run(&["check", "--schema", "decomposition"], "{not json").code,
        2
    );
    assert_eq!(run(&["check", "--schema", "no-such"], "{}").code, 2);
    assert_eq!(
        run(&["check", "--schema", "undetermined-probe"], PROBE_UNDET).code,
        2
    );
}

#[test]
fn claim_metrics_reads_store() {
    let home = temp_dir();
    for _ in 0..3 {
        let r = run_in(&home, &["check", "--schema", "decomposition"], BAD_DECOMP);
        assert_eq!(r.code, 1);
    }
    let store = home.join(".schemaguard").join("rejects.jsonl");
    assert!(store.is_file(), "store missing at {}", store.display());
    let j = run_in(&home, &["metrics", "--json"], "");
    assert_eq!(j.code, 0, "{}", j.stderr);
    let v = json(&j);
    assert_eq!(v["decomposition"], 3, "got {v}");
    let h = run_in(&home, &["metrics"], "");
    assert_eq!(h.code, 0);
    let line = h
        .stdout
        .lines()
        .find(|l| l.starts_with("decomposition"))
        .unwrap_or_else(|| panic!("no decomposition row: {}", h.stdout));
    assert_eq!(line.split_whitespace().last(), Some("3"));
}

#[test]
fn claim_metrics_exit() {
    // absent store -> 0
    let home = temp_dir();
    assert_eq!(run_in(&home, &["metrics"], "").code, 0);
    assert_eq!(run_in(&home, &["metrics", "--json"], "").code, 0);

    // readable store -> 0
    run_in(&home, &["check", "--schema", "decomposition"], BAD_DECOMP);
    assert_eq!(run_in(&home, &["metrics"], "").code, 0);
    assert_eq!(run_in(&home, &["metrics", "--json"], "").code, 0);

    // unreadable store (a directory) -> 2, with unknown status on stdout
    let h2 = temp_dir();
    std::fs::create_dir_all(h2.join(".schemaguard").join("rejects.jsonl")).unwrap();
    let r = run_in(&h2, &["metrics", "--json"], "");
    assert_eq!(r.code, 2, "stdout={} stderr={}", r.stdout, r.stderr);
    assert_eq!(json(&r)["status"], "unknown");
    assert_eq!(run_in(&h2, &["metrics"], "").code, 2);

    // malformed line in an otherwise readable store -> 2
    let h3 = temp_dir();
    std::fs::create_dir_all(h3.join(".schemaguard")).unwrap();
    std::fs::write(
        h3.join(".schemaguard").join("rejects.jsonl"),
        "{\"schema\":\"a\",\"violations\":1}\nnot json\n",
    )
    .unwrap();
    let r = run_in(&h3, &["metrics", "--json"], "");
    assert_eq!(r.code, 2, "stdout={} stderr={}", r.stdout, r.stderr);
    assert_eq!(json(&r)["status"], "unknown", "stdout={}", r.stdout);
}

#[test]
fn claim_check_single_json_stdout() {
    let cases: [(&str, &str); 4] = [
        ("decomposition", VALID_DECOMP),
        ("decomposition", BAD_DECOMP),
        ("decomposition", "{not json"),
        ("undetermined-probe", PROBE_UNDET),
    ];
    for (schema, payload) in cases {
        let o = run(&["check", "--schema", schema], payload);
        assert!(json(&o).is_object(), "{}", o.stdout);
        assert_eq!(o.stdout.trim().lines().count(), 1, "{}", o.stdout);
    }
    // unreadable stdin (non-UTF-8 bytes): one JSON object, exit 2
    let o = run_bytes(&["check", "--schema", "decomposition"], b"\xff\xfe");
    assert_eq!(o.code, 2);
    assert_eq!(o.stdout.trim().lines().count(), 1, "{}", o.stdout);
    let v = json(&o);
    assert_eq!(v["valid"], false);
    assert!(
        v["error"].as_str().unwrap().contains("cannot read stdin"),
        "{v}"
    );
    // `check` with no --schema is a clap usage error: nothing on stdout, exit 2
    let o = run(&["check"], "{}");
    assert_eq!(o.code, 2);
    assert_eq!(o.stdout, "");
    // Observed: an unknown schema prints NO JSON verdict on stdout.
    let o = run(&["check", "--schema", "no-such"], "{}");
    assert_eq!(o.code, 2);
    assert_eq!(o.stdout, "");
}

#[test]
fn claim_clean_arm_shape() {
    let o = run(&["check", "--schema", "decomposition"], VALID_DECOMP);
    let v = json(&o);
    assert_eq!(v["valid"], true);
    assert_eq!(v["errors"], serde_json::json!([]));
    assert!(v.get("undetermined").is_none(), "{v}");
}

#[test]
fn claim_errors_entry_path_problem() {
    let o = run(&["check", "--schema", "decomposition"], NESTED_BAD_DECOMP);
    assert_eq!(o.code, 1);
    let v = json(&o);
    assert_eq!(v["valid"], false);
    let errs = v["errors"].as_array().unwrap();
    assert!(!errs.is_empty());
    for e in errs {
        assert_eq!(keys(e), ["path", "problem"], "{e}");
        assert!(e["path"].is_string());
        assert!(!e["problem"].as_str().unwrap().is_empty());
    }
    // Observed form: dotted object path with [index] for array elements.
    assert!(
        errs.iter().any(|e| e["path"] == "tasks[0].id"),
        "paths: {errs:?}"
    );
}

#[test]
fn claim_not_checked_entry_reason() {
    // `role`/`class`/`cost_usd` optional and absent in episode -> waived.
    let o = run(
        &["check", "--schema", "episode"],
        r#"{"title":"t","model":"m","pass":true,"extra_key":1}"#,
    );
    assert_eq!(o.code, 0, "{}", o.stdout);
    let v = json(&o);
    let nc = v["not_checked"].as_array().unwrap();
    assert!(nc.len() >= 2, "{v}");
    for w in nc {
        assert!(w["path"].is_string(), "{w}");
        assert!(!w["reason"].as_str().unwrap_or("").is_empty(), "{w}");
    }
    assert!(nc.iter().any(|w| w["path"] == "extra_key"));
    assert!(nc.iter().any(|w| w["path"] == "role"));
}

#[test]
fn claim_array_without_items_not_checked() {
    use schemaguard::schema::{validate_report, Field, Ty};
    static FIELDS: &[Field] = &[Field {
        name: "xs",
        ty: Ty::Array,
        required: true,
        enum_values: &[],
        items: &[],
    }];
    let r = validate_report(&serde_json::json!({"xs": [1, "a", {"b": 2}]}), FIELDS, "");
    assert!(r.violations().is_empty());
    assert!(r.undetermined().is_empty());
    let w = r
        .waived()
        .iter()
        .find(|w| w.path == "xs")
        .unwrap_or_else(|| panic!("xs not in waived: {:?}", r.waived()));
    assert!(w.reason.contains("no items schema"), "{}", w.reason);
    // And it does not block.
    assert!(matches!(
        r.verdict(),
        harness_core::verdict::Verdict::Clean(_)
    ));
}

#[test]
fn claim_input_failure_shape() {
    let o = run(&["check", "--schema", "decomposition"], "{not json");
    assert_eq!(o.code, 2);
    let v = json(&o);
    assert_eq!(keys(&v), ["error", "valid"]);
    assert_eq!(v["valid"], false);
    assert!(v["error"].is_string());

    let o = run_bytes(&["check", "--schema", "decomposition"], b"\xff\xfe");
    assert_eq!(o.code, 2);
    let v = json(&o);
    assert_eq!(keys(&v), ["error", "valid"]);
    assert_eq!(v["valid"], false);
    assert!(
        v["error"].as_str().unwrap().contains("cannot read stdin"),
        "{v}"
    );

    let home = temp_dir();
    let missing = home.join("nope.json");
    let o = run_in(
        &home,
        &[
            "check",
            "--schema",
            "decomposition",
            "--file",
            missing.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(o.code, 2);
    let v = json(&o);
    assert_eq!(keys(&v), ["error", "valid"]);
    assert_eq!(v["valid"], false);
    assert!(v["error"].as_str().unwrap().contains("cannot read file"));
}

#[test]
fn claim_unknown_schema_stderr() {
    let home = temp_dir();
    let missing = home.join("nope.json");
    let o = run_in(
        &home,
        &[
            "check",
            "--schema",
            "no-such",
            "--file",
            missing.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(o.code, 2);
    assert_eq!(o.stdout, "");
    assert!(
        o.stderr.contains("unknown schema 'no-such'"),
        "{}",
        o.stderr
    );
    assert!(!o.stderr.contains("cannot read file"), "{}", o.stderr);
}

#[test]
fn claim_rejects_counted() {
    let home = temp_dir();
    let metrics = |h: &Path| -> Value {
        let m = run_in(h, &["metrics", "--json"], "");
        assert_eq!(m.code, 0, "{}", m.stderr);
        json(&m)
    };
    // clean check adds nothing
    assert_eq!(
        run_in(
            &home,
            &["check", "--schema", "verdict"],
            r#"{"candidate":"c","pass":true}"#
        )
        .code,
        0
    );
    assert_eq!(metrics(&home), serde_json::json!({}));

    // parse failure
    assert_eq!(
        run_in(&home, &["check", "--schema", "episode"], "{bad").code,
        2
    );
    // field violation (playbook: title missing -> exactly 1)
    assert_eq!(
        run_in(&home, &["check", "--schema", "playbook"], "{}").code,
        1
    );
    // undetermined
    assert_eq!(
        run_in(
            &home,
            &["check", "--schema", "undetermined-probe"],
            PROBE_UNDET
        )
        .code,
        2
    );
    let m = metrics(&home);
    assert_eq!(m["episode"], 1, "{m}");
    assert_eq!(m["playbook"], 1, "{m}");
    assert_eq!(m["undetermined-probe"], 1, "{m}");
    assert!(m.get("verdict").is_none(), "{m}");
}

#[test]
fn claim_list_exact_names() {
    let o = run(&["list"], "");
    assert_eq!(o.code, 0);
    let mut got: Vec<&str> = o.stdout.lines().collect();
    got.sort();
    assert_eq!(
        got,
        [
            "decomposition",
            "episode",
            "playbook",
            "scout-measure",
            "undetermined-probe",
            "verdict"
        ]
    );
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_dir().join("..").join("..")
}

fn git(args: &[&str]) -> Out {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("git runs");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// Pure predicate over the packaging facts, so the test can be mutated at its input.
fn no_hooks(plugin_json: &str, has_hooks_dir: bool) -> bool {
    let v: Value = serde_json::from_str(plugin_json).expect("plugin.json parses");
    !has_hooks_dir && v.get("hooks").is_none()
}

#[test]
fn claim_plugin_no_hooks() {
    let launcher = crate_dir().join("bin").join("schemaguard");
    assert!(launcher.is_file());
    let idx = git(&["ls-files", "-s", "crates/schemaguard/bin/schemaguard"]);
    assert!(idx.stdout.starts_with("100755 "), "index: {:?}", idx.stdout);

    let pj = std::fs::read_to_string(crate_dir().join(".claude-plugin/plugin.json")).unwrap();
    let has_hooks_dir = crate_dir().join("hooks").exists();
    assert!(no_hooks(&pj, has_hooks_dir));
    // Input mutations flip the predicate (RED for this claim).
    assert!(!no_hooks(&pj, true));
    let mut v: Value = serde_json::from_str(&pj).unwrap();
    v["hooks"] = serde_json::json!({});
    assert!(!no_hooks(&v.to_string(), false));
}

#[test]
fn claim_undetermined_exit_codes() {
    let o = run(
        &["check", "--schema", "undetermined-probe"],
        r#"{"name":"n","items_any":"s"}"#,
    );
    assert_eq!(o.code, 2, "{}", o.stdout);
    let v = json(&o);
    assert_eq!(v["valid"], false);
    assert_eq!(v["errors"], serde_json::json!([]), "{v}");
    assert!(
        v["undetermined"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["path"] == "items_any"),
        "{v}"
    );

    // name missing AND items undetermined: observed exit 1, both lists populated.
    let o = run(
        &["check", "--schema", "undetermined-probe"],
        r#"{"items_any":"s"}"#,
    );
    assert_eq!(o.code, 1, "{}", o.stdout);
    let v = json(&o);
    assert!(
        v["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "name"),
        "{v}"
    );
    assert!(
        v["undetermined"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["path"] == "items_any"),
        "{v}"
    );
}

/// A line that is code, not a comment or an echo.
fn code_line_containing(script: &str, needle: &str) -> bool {
    script.lines().any(|l| {
        let t = l.trim();
        !t.starts_with('#') && !t.starts_with("echo") && t.contains(needle)
    })
}

#[test]
fn claim_launcher_only_committed() {
    let tracked = git(&["ls-files", "crates/schemaguard/bin"]);
    assert_eq!(tracked.stdout.trim(), "crates/schemaguard/bin/schemaguard");

    for plat in [
        "darwin-arm64",
        "darwin-x86_64",
        "linux-arm64",
        "linux-x86_64",
    ] {
        let p = format!("crates/schemaguard/bin/schemaguard-{plat}");
        assert_eq!(git(&["check-ignore", &p]).code, 0, "not ignored: {p}");
    }
    // Observed: the windows binary is NOT covered by .gitignore.
    assert_eq!(
        git(&[
            "check-ignore",
            "crates/schemaguard/bin/schemaguard-windows-x86_64.exe"
        ])
        .code,
        1
    );
    // the launcher itself is NOT ignored
    assert_eq!(
        git(&["check-ignore", "crates/schemaguard/bin/schemaguard"]).code,
        1
    );

    // rollout-plugins.sh delegates to rebuild-plugins.sh, which builds and
    // copies `<bin>-<os>-<arch>`; the checks skip comment and echo lines.
    let rollout = std::fs::read_to_string(repo_root().join("scripts/rollout-plugins.sh")).unwrap();
    let rebuild = std::fs::read_to_string(repo_root().join("scripts/rebuild-plugins.sh")).unwrap();
    let call = "bash \"$REPO/scripts/rebuild-plugins.sh\"";
    let build = "cargo build --release";
    assert!(code_line_containing(&rollout, call));
    assert!(code_line_containing(&rebuild, build));
    assert!(rebuild.contains("SUF=\"$os-$arch\""));
    assert!(rebuild.contains("cp -f \"$src\" \"$binfile\""));
    // RED on the predicate: comment-only / echo-only copies must not satisfy it.
    let prefix_lines = |text: &str, needle: &str, pre: &str| -> String {
        text.lines()
            .map(|l| {
                if l.contains(needle) {
                    format!("{pre}{l}")
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let commented = prefix_lines(&rollout, call, "# ");
    assert!(!code_line_containing(&commented, call));
    let echoed = prefix_lines(&rebuild, build, "echo ");
    assert!(!code_line_containing(&echoed, build));
}

#[test]
fn claim_metrics_sums_violations() {
    let home = temp_dir();
    // goal missing + tasks[0].id missing = 2 violations in ONE check.
    let o = run_in(
        &home,
        &["check", "--schema", "decomposition"],
        r#"{"tasks":[{"title":"x"}]}"#,
    );
    assert_eq!(o.code, 1);
    let k = json(&o)["errors"].as_array().unwrap().len();
    assert_eq!(k, 2, "{}", o.stdout);
    // one parse failure on another schema
    assert_eq!(
        run_in(&home, &["check", "--schema", "episode"], "{bad").code,
        2
    );
    let m = run_in(&home, &["metrics", "--json"], "");
    assert_eq!(m.code, 0, "{}", m.stderr);
    let v = json(&m);
    assert_eq!(v["decomposition"], k, "sum of violations, got {v}");
    assert_eq!(v["episode"], 1, "{v}");
}
