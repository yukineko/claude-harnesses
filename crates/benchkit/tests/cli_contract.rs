//! Claim-reproduction tests for the benchkit README prose.
//!
//! Every external tool (curl / git / python) is a fake `/bin/sh` script in a
//! fresh tempdir placed FIRST on PATH; nothing here touches the real network,
//! a real clone, or real pytest. The one exception to "no host tools":
//! `claim_no_redistribution` runs the HOST `git ls-files` (read-only, in this
//! crate's directory) to list tracked files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use benchkit::{dashboard, loader, scorer};

mod support;

const BIN: &str = env!("CARGO_BIN_EXE_benchkit");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const ASTROPY: &str = "astropy__astropy-12907";

fn fixture() -> PathBuf {
    Path::new(ROOT).join("tests/fixtures/instances.jsonl")
}

fn write_exec(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Sandbox: fake bin dir + log + home + tmp + cwd, all under one tempdir.
struct Sandbox {
    _td: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().canonicalize().unwrap();
        for d in ["bin", "home", "tmp", "cwd"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let sb = Sandbox { _td: td, root };
        sb.install_fakes();
        sb
    }
    fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    fn cwd(&self) -> PathBuf {
        self.root.join("cwd")
    }
    fn log(&self) -> PathBuf {
        self.root.join("calls.log")
    }
    fn applied(&self) -> PathBuf {
        self.root.join("applied.txt")
    }
    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn install_fakes(&self) {
        let b = self.bin();
        // curl: log argv, write fixed bytes to the -o target.
        write_exec(
            &b,
            "curl",
            r#"#!/bin/sh
echo "curl $*" >> "$FAKE_LOG"
out=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
printf 'PAR1-not-jsonl-bytes\n' > "$out"
exit 0
"#,
        );
        // git: log argv; clone mkdirs its last arg; apply records stdin.
        write_exec(
            &b,
            "git",
            r#"#!/bin/sh
echo "git $*" >> "$FAKE_LOG"
case "$1" in
  clone)
    for a in "$@"; do last="$a"; done
    mkdir -p "$last"
    ;;
esac
for a in "$@"; do
  if [ "$a" = "apply" ]; then
    cat >> "$FAKE_APPLIED"
    echo "---SEP---" >> "$FAKE_APPLIED"
  fi
done
exit 0
"#,
        );
        // python: log argv; PASSED for every test id arg (FAILED for $FAKE_FAIL_ID).
        write_exec(
            &b,
            "python",
            r#"#!/bin/sh
echo "python $*" >> "$FAKE_LOG"
shift; shift
for a in "$@"; do
  case "$a" in
    -*) ;;
    *)
      if [ "$a" = "$FAKE_FAIL_ID" ]; then echo "FAILED $a - boom"; else echo "PASSED $a"; fi
      ;;
  esac
done
exit 0
"#,
        );
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin().display()))
            .env("HOME", self.root.join("home"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("FAKE_LOG", self.log())
            .env("FAKE_APPLIED", self.applied())
            .env("FAKE_FAIL_ID", "")
            .current_dir(self.cwd())
            .stdin(Stdio::null());
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }
}

fn so(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn se(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn astropy() -> benchkit::Instance {
    loader::load_instances(fixture())
        .unwrap()
        .into_iter()
        .find(|i| i.instance_id == ASTROPY)
        .unwrap()
}

/// Strip full-line `//` comments and everything from the first `#[cfg(test)]`.
fn code_only(src: &str) -> String {
    let cut = src.find("#[cfg(test)]").unwrap_or(src.len());
    src[..cut]
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_src(name: &str) -> String {
    std::fs::read_to_string(Path::new(ROOT).join("src").join(name)).unwrap()
}

// ---------------------------------------------------------------- subcommands

#[test]
fn claim_subcommands() {
    let sb = Sandbox::new();
    let out = sb.run(&["--help"]);
    assert!(out.status.success());
    let text = so(&out);
    let mut names: Vec<String> = Vec::new();
    let mut in_cmds = false;
    for line in text.lines() {
        if line.starts_with("Commands:") {
            in_cmds = true;
            continue;
        }
        if in_cmds {
            if line.trim().is_empty() {
                break;
            }
            if line.starts_with("  ") && !line.starts_with("   ") {
                if let Some(n) = line.split_whitespace().next() {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.sort();
    assert_eq!(
        names,
        ["audit-sample", "download", "help", "load", "run-instance"],
        "help was:\n{text}"
    );

    let out = sb.run(&["dashboard"]);
    assert!(
        !out.status.success(),
        "`dashboard` must not be a subcommand"
    );
    assert!(
        se(&out).contains("unrecognized subcommand"),
        "stderr: {}",
        se(&out)
    );
}

// ---------------------------------------------------------- dashboard append

#[test]
fn claim_dashboard_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let rec = |ts: i64, model: &str| dashboard::RunRecord {
        timestamp: ts,
        resolution_rate: 0.5,
        instances: vec![dashboard::InstanceResult {
            instance_id: "a__a-1".into(),
            resolved: true,
        }],
        model: model.into(),
        cost: 1.0,
    };
    dashboard::append_run(dir.path(), &rec(1, "A")).unwrap();
    let p = dir.path().join("runs.jsonl");
    let after_a = std::fs::read(&p).unwrap();
    assert_eq!(after_a.iter().filter(|b| **b == b'\n').count(), 1);
    dashboard::append_run(dir.path(), &rec(2, "B")).unwrap();
    let after_b = std::fs::read(&p).unwrap();
    let text = String::from_utf8(after_b.clone()).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(
        after_b.starts_with(&after_a),
        "earlier bytes must be unchanged by the second append"
    );
    let first_line = text.lines().next().unwrap();
    assert_eq!(format!("{first_line}\n").as_bytes(), after_a.as_slice());
}

// -------------------------------------------------------- stub generator

#[test]
fn claim_stub_generator_empty() {
    let sb = Sandbox::new();
    let inst = astropy();
    assert!(!inst.test_patch.trim().is_empty());
    let out = sb.run(&[
        "run-instance",
        "--real",
        fixture().to_str().unwrap(),
        ASTROPY,
    ]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let applies: Vec<String> = sb
        .log_lines()
        .into_iter()
        .filter(|l| l.starts_with("git ") && l.contains(" apply"))
        .collect();
    assert_eq!(applies.len(), 1, "only test_patch is applied: {applies:?}");
    // The single apply carried exactly test_patch on stdin -> the candidate was empty.
    let applied = std::fs::read_to_string(sb.applied()).unwrap();
    assert_eq!(applied, format!("{}---SEP---\n", inst.test_patch));
}

// ------------------------------------------- download never in cargo test

/// Mechanical check on download.rs's `#[cfg(test)]` module: every call that
/// reaches `execute(` passes `false` for force on a pre-created cache.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn download_tests_never_reach_curl(download_rs: &str) -> Result<(), String> {
    let start = download_rs
        .find("#[cfg(test)]")
        .ok_or("no test module found")?;
    let tests = &download_rs[start..];
    let mut calls = 0;
    for line in tests.lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        if t.contains("execute(") || t.contains("fetch(") {
            calls += 1;
            if !t.contains("execute(Some(") || !t.contains(", false)") {
                return Err(format!("test reaches fetch path: {t}"));
            }
        }
    }
    if calls == 0 {
        return Err("no execute call found (scan would be vacuous)".into());
    }
    let w = tests
        .find("std::fs::write(&cache")
        .ok_or("cache not pre-created")?;
    let e = tests.find("execute(").unwrap();
    if w > e {
        return Err("cache created after execute".into());
    }
    Ok(())
}

#[test]
fn claim_download_not_in_cargo_test() {
    let good = read_src("download.rs");
    download_tests_never_reach_curl(&good).unwrap();
    // Self-check: the scan rejects a force=true test and a missing precreate.
    let forced = good.replace(
        "execute(Some(cache.clone()), false)",
        "execute(Some(cache.clone()), true)",
    );
    assert!(download_tests_never_reach_curl(&forced).is_err());
    let nofile = good.replace("std::fs::write(&cache", "std::fs::read(&cache");
    assert!(download_tests_never_reach_curl(&nofile).is_err());

    // No other file in src/ or tests/ calls into download's execute/fetch;
    // cli_contract.rs only reaches download through the BINARY + fake curl.
    let mut files: Vec<PathBuf> = Vec::new();
    for d in ["src", "tests"] {
        rs_files(&Path::new(ROOT).join(d), &mut files);
    }
    assert!(files.len() >= 9);
    let needle_a = concat!("download", "::execute");
    let needle_b = concat!("download", "::fetch");
    for p in files {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name == "download.rs" || name == "main.rs" {
            continue;
        }
        let s = std::fs::read_to_string(&p).unwrap();
        if name == "cli_contract.rs" {
            // only this file's own needle literals may mention them
            let stripped = s.replace(needle_a, "").replace(needle_b, "");
            assert!(!stripped.contains(concat!("use benchkit::", "download")));
            continue;
        }
        assert!(
            !s.contains(needle_a) && !s.contains(needle_b),
            "{name} calls into download"
        );
    }
}

/// This file's own binary runs must resolve `curl` to the fake: exactly one
/// spawn of the binary (in `Sandbox::cmd`), which clears the env and puts the
/// fake bin dir first on PATH.
fn own_runs_use_fake_path(me: &str) -> Result<(), String> {
    let spawn = concat!("Command::new(", "BIN)");
    let n = me.matches(spawn).count();
    if n != 1 {
        return Err(format!("{spawn} occurs {n} times, expected 1"));
    }
    let head = concat!("fn cmd(&self)", " -> Command {");
    let i = me.find(head).ok_or("Sandbox::cmd not found")?;
    let end = me[i..].find("\n    }\n").ok_or("cmd end not found")?;
    let body = &me[i..i + end];
    if !body.contains(spawn) {
        return Err("the single spawn is not inside Sandbox::cmd".into());
    }
    let env_clear = body.find(".env_clear()").ok_or("no env_clear")?;
    let path = body
        .find(".env(\"PATH\", format!(\"{}:/usr/bin:/bin\", self.bin().display()))")
        .ok_or("PATH is not fake-bin-first")?;
    if env_clear > path {
        return Err("PATH set before env_clear".into());
    }
    Ok(())
}

#[test]
fn claim_download_not_in_cargo_test_binary_runs() {
    // (a) curl is a bare PATH-resolved command in download.rs.
    let dl_code = code_only(&read_src("download.rs"));
    assert!(dl_code.contains("Command::new(\"curl\")"));
    assert!(!dl_code.contains("Command::new(\"/"));
    // (b) this file's own binary spawns go through the fake PATH.
    let me = std::fs::read_to_string(Path::new(ROOT).join("tests/cli_contract.rs")).unwrap();
    own_runs_use_fake_path(&me).unwrap();
    // self-checks: the scan rejects a second spawn and a PATH not fake-first.
    let spawn = concat!("Command::new(", "BIN)");
    assert!(own_runs_use_fake_path(&format!("{me}\n{spawn};")).is_err());
    let real_first = me.replace("self.bin().display()", "\"/usr/bin\"");
    assert!(own_runs_use_fake_path(&real_first).is_err());
    // (c) behavioural: one logged call proves the FAKE curl ran.
    let sb = Sandbox::new();
    let out = dl(&sb, &["--dest", "x.jsonl"]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let log = sb.log_lines();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("curl "), "{}", log[0]);
}

// ------------------------------------------------------- no redistribution

fn dataset_violations(tracked: &[String], fixture_text: &str) -> Vec<String> {
    let mut bad = Vec::new();
    for f in tracked {
        let ext = Path::new(f)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let in_fix = f.starts_with("tests/fixtures/");
        let plugin_meta = f.starts_with(".claude-plugin/") && ext == "json";
        if !in_fix
            && !plugin_meta
            && ["jsonl", "json", "parquet", "csv", "tsv", "arrow", "db"].contains(&ext.as_str())
        {
            bad.push(format!("dataset-like file outside fixtures: {f}"));
        }
        if f.starts_with(".benchkit-cache") {
            bad.push(format!("cache tracked: {f}"));
        }
    }
    if !tracked
        .iter()
        .any(|f| f == "tests/fixtures/instances.jsonl")
    {
        bad.push("fixture instances.jsonl not tracked".into());
    }
    let rows = fixture_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    if rows != 2 {
        bad.push(format!("fixture has {rows} rows, expected 2"));
    }
    bad
}

#[test]
fn claim_no_redistribution() {
    let out = support::git_env::scrub(&mut Command::new("git"))
        .args(["ls-files", "--", "."])
        .current_dir(ROOT)
        .output()
        .expect("git must be runnable to list tracked files");
    assert!(out.status.success(), "git ls-files failed: {}", se(&out));
    let tracked: Vec<String> = so(&out).lines().map(str::to_string).collect();
    assert!(tracked.len() > 5, "listing looks empty: {tracked:?}");
    let text = std::fs::read_to_string(fixture()).unwrap();
    assert_eq!(dataset_violations(&tracked, &text), Vec::<String>::new());

    // Self-check (input mutation): the check does fire.
    let mut more = tracked.clone();
    more.push("data/test-00000-of-00001.parquet".into());
    assert!(!dataset_violations(&more, &text).is_empty());
    let mut more = tracked.clone();
    more.push("dataset.jsonl".into());
    assert!(!dataset_violations(&more, &text).is_empty());
    let mut more = tracked.clone();
    more.push("data/test.json".into());
    assert!(!dataset_violations(&more, &text).is_empty());
    assert!(
        tracked.iter().any(|f| f == ".claude-plugin/plugin.json"),
        "plugin metadata should be tracked (and allowed)"
    );
    let three = format!("{text}{}\n", text.lines().next().unwrap());
    assert!(!dataset_violations(&tracked, &three).is_empty());
}

// ------------------------------------------------------- shell I/O confinement

#[test]
fn claim_shell_io_only_download_and_real() {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for e in std::fs::read_dir(Path::new(ROOT).join("src")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let code = code_only(&std::fs::read_to_string(&p).unwrap());
        let n = code.matches("Command::new").count();
        if n > 0 {
            seen.insert(name.clone(), n);
        }
        for tok in ["TcpStream", "UdpSocket", "TcpListener", "reqwest", "ureq"] {
            assert!(!code.contains(tok), "{name} mentions {tok}");
        }
    }
    assert_eq!(
        seen.keys().map(String::as_str).collect::<Vec<_>>(),
        ["download.rs", "harness.rs"],
        "Command::new found in {seen:?}"
    );
    // Within harness.rs every Command::new sits inside RealExecSource's impl.
    let h = code_only(&read_src("harness.rs"));
    let from = h.find("impl TestResultSource for RealExecSource").unwrap();
    let to = h.find("pub fn parse_pytest_output").unwrap();
    assert_eq!(
        h[from..to].matches("Command::new").count(),
        h.matches("Command::new").count(),
        "Command::new outside RealExecSource in harness.rs"
    );

    // Behavioural: the non-real subcommands invoke no fake tool.
    let sb = Sandbox::new();
    let fx = fixture();
    let fx = fx.to_str().unwrap();
    assert!(sb.run(&["load", fx]).status.success());
    assert!(sb.run(&["run-instance", fx, ASTROPY]).status.success());
    let changes = sb.cwd().join("changes.jsonl");
    std::fs::write(
        &changes,
        "{\"change_id\":\"c1\",\"gates\":[\"propguard\"]}\n",
    )
    .unwrap();
    assert!(sb
        .run(&[
            "audit-sample",
            changes.to_str().unwrap(),
            "--fraction",
            "1.0"
        ])
        .status
        .success());
    assert!(sb
        .run(&[
            "audit-sample",
            changes.to_str().unwrap(),
            "--from-violations",
            "--fraction",
            "1.0"
        ])
        .status
        .success());
    assert_eq!(
        sb.log_lines(),
        Vec::<String>::new(),
        "a fake tool was invoked"
    );
    assert!(!sb.applied().exists());
}

// ---------------------------------------------------------- loader purity

#[test]
fn claim_loader_pure_deterministic() {
    let a = loader::load_instances(fixture()).unwrap();
    let b = loader::load_instances(fixture()).unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a, b);
    for f in ["loader.rs", "model.rs"] {
        let code = code_only(&read_src(f));
        for tok in [
            "SystemTime",
            "Instant",
            "Command",
            "env::var",
            "TcpStream",
            "chrono",
        ] {
            assert!(!code.contains(tok), "{f} contains {tok}");
        }
    }
}

// ------------------------------------------------------ Instance fields

type Row = serde_json::Map<String, serde_json::Value>;

fn row0() -> Row {
    let l = std::fs::read_to_string(fixture()).unwrap();
    serde_json::from_str(l.lines().next().unwrap()).unwrap()
}

fn load_row(td: &Path, name: &str, row: &Row) -> anyhow::Result<Vec<benchkit::Instance>> {
    let p = td.join(name);
    std::fs::write(&p, format!("{}\n", serde_json::Value::Object(row.clone()))).unwrap();
    loader::load_instances(&p)
}

const REQUIRED: [&str; 6] = [
    "instance_id",
    "repo",
    "base_commit",
    "patch",
    "test_patch",
    "problem_statement",
];

#[test]
fn claim_instance_required_fields() {
    let td = tempfile::tempdir().unwrap();
    let r0 = row0();
    let inst = astropy();
    assert_eq!(inst.instance_id, ASTROPY);
    assert_eq!(inst.repo, "astropy/astropy");
    assert_eq!(inst.version, "4.3");
    assert_eq!(inst.created_at, r0["created_at"].as_str().unwrap());
    assert_eq!(inst.created_at, "2022-03-03T15:14:54Z");

    for key in REQUIRED {
        let mut r = r0.clone();
        assert!(r.remove(key).is_some());
        assert!(
            load_row(td.path(), "m.jsonl", &r).is_err(),
            "removing required key {key} must fail"
        );
    }

    // Only the required keys: optional strings default to "".
    let mut r = Row::new();
    for key in REQUIRED {
        r.insert(key.into(), r0[key].clone());
    }
    let got = load_row(td.path(), "min.jsonl", &r).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].hints_text, "");
    assert_eq!(got[0].created_at, "");
    assert_eq!(got[0].version, "");
    assert_eq!(got[0].environment_setup_commit, "");

    // created_at is kept verbatim (no normalization).
    r.insert("created_at".into(), "2022-03-03 15:14:54+09:00 ".into());
    let got = load_row(td.path(), "ts.jsonl", &r).unwrap();
    assert_eq!(got[0].created_at, "2022-03-03 15:14:54+09:00 ");
}

// ------------------------------------------------------ F2P / P2P semantics

#[test]
fn claim_f2p_p2p_semantics() {
    let td = tempfile::tempdir().unwrap();
    let mut r = row0();
    r.insert("FAIL_TO_PASS".into(), serde_json::json!(["a"]));
    r.insert("PASS_TO_PASS".into(), serde_json::json!(["b"]));
    let inst = load_row(td.path(), "s.jsonl", &r).unwrap().remove(0);
    assert_eq!(inst.fail_to_pass, ["a"]);
    assert_eq!(inst.pass_to_pass, ["b"]);
    let res = |a: bool, b: bool| -> BTreeMap<String, bool> {
        [("a".to_string(), a), ("b".to_string(), b)].into()
    };
    let score =
        |m: &BTreeMap<String, bool>| scorer::is_resolved(m, &inst.fail_to_pass, &inst.pass_to_pass);
    assert!(score(&res(true, true)));
    assert!(!score(&res(false, true)));
    assert!(!score(&res(true, false)));
    assert!(!score(&res(false, false)));
}

// ------------------------------------------------------------- download

fn dl(sb: &Sandbox, args: &[&str]) -> Output {
    let mut a = vec!["download"];
    a.extend_from_slice(args);
    sb.run(&a)
}

#[test]
fn claim_download_shape() {
    // CLAIM (README): `download` writes the upstream .parquet file to the cache
    // path byte-for-byte and does not convert it to JSONL. Pinned here: curl's
    // output lands verbatim at the .jsonl path and the URL is a .parquet file.
    let sb = Sandbox::new();
    let out = dl(&sb, &["--dest", "c.jsonl"]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let bytes = std::fs::read(sb.cwd().join("c.jsonl")).unwrap();
    assert_eq!(
        bytes, b"PAR1-not-jsonl-bytes\n",
        "written verbatim, unnormalized"
    );
    let log = sb.log_lines();
    assert_eq!(log.len(), 1);
    let url = log[0].split_whitespace().last().unwrap();
    assert!(url.starts_with("https://huggingface.co/"), "{url}");
    assert!(url.ends_with(".parquet"), "{url}");
}

#[test]
fn claim_download_default_dest() {
    let sb = Sandbox::new();
    let out = dl(&sb, &[]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    assert_eq!(
        so(&out).trim(),
        "fetched: .benchkit-cache/swe-bench-verified.jsonl"
    );
    assert!(sb
        .cwd()
        .join(".benchkit-cache/swe-bench-verified.jsonl")
        .is_file());
}

#[test]
fn claim_download_dest_flag() {
    let sb = Sandbox::new();
    let out = dl(&sb, &["--dest", "x/y.jsonl"]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    assert_eq!(so(&out).trim(), "fetched: x/y.jsonl");
    assert!(sb.cwd().join("x/y.jsonl").is_file());
    assert!(!sb.cwd().join(".benchkit-cache").exists());
}

#[test]
fn claim_download_force_refetch() {
    let sb = Sandbox::new();
    let cache = sb.cwd().join("cache.jsonl");
    std::fs::write(&cache, "OLD\n").unwrap();

    let out = dl(&sb, &["--dest", "cache.jsonl"]);
    assert!(out.status.success());
    assert!(so(&out).contains("cache hit"), "stdout: {}", so(&out));
    assert!(sb.log_lines().is_empty(), "curl called without --force");
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), "OLD\n");

    let out = dl(&sb, &["--dest", "cache.jsonl", "--force"]);
    assert!(out.status.success());
    assert!(so(&out).starts_with("fetched:"), "stdout: {}", so(&out));
    let log = sb.log_lines();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_ne!(std::fs::read_to_string(&cache).unwrap(), "OLD\n");
}

#[test]
fn claim_download_curl_timeout() {
    let sb = Sandbox::new();
    assert!(dl(&sb, &["--dest", "c.jsonl"]).status.success());
    let log = sb.log_lines();
    assert_eq!(log.len(), 1);
    assert!(log[0].starts_with("curl "), "{}", log[0]);
    let argv: Vec<&str> = log[0].split_whitespace().collect();
    let i = argv
        .iter()
        .position(|a| *a == "--max-time")
        .expect("--max-time");
    assert_eq!(argv[i + 1], "120");

    let toml = std::fs::read_to_string(Path::new(ROOT).join("Cargo.toml")).unwrap();
    let deps: String = toml
        .lines()
        .skip_while(|l| l.trim() != "[dependencies]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        deps.contains("serde"),
        "dependency section not found: {deps}"
    );
    for http in [
        "reqwest",
        "ureq",
        "hyper",
        "isahc",
        "curl",
        "attohttpc",
        "minreq",
    ] {
        assert!(
            !deps.lines().any(|l| l.trim_start().starts_with(http)),
            "HTTP client crate {http} in [dependencies]"
        );
    }
}

// -------------------------------------------------------------- run-instance

#[test]
fn claim_dry_run() {
    let sb = Sandbox::new();
    let out = sb.run(&["run-instance", fixture().to_str().unwrap(), ASTROPY]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    assert_eq!(
        so(&out).trim(),
        "astropy__astropy-12907: resolved=false (0 test results)"
    );
    assert!(sb.log_lines().is_empty(), "a fake tool was invoked");
    assert!(!sb.applied().exists());
}

fn real(sb: &Sandbox, fail_id: &str) -> Output {
    sb.cmd()
        .env("FAKE_FAIL_ID", fail_id)
        .args([
            "run-instance",
            "--real",
            fixture().to_str().unwrap(),
            ASTROPY,
        ])
        .output()
        .unwrap()
}

#[test]
fn claim_real_path() {
    let sb = Sandbox::new();
    let inst = astropy();
    let out = real(&sb, "");
    assert!(out.status.success(), "stderr: {}", se(&out));
    assert_eq!(
        so(&out).trim(),
        "astropy__astropy-12907: resolved=true (4 test results)"
    );
    let log = sb.log_lines();
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(
        log[0].starts_with("git clone --quiet https://github.com/astropy/astropy.git "),
        "{}",
        log[0]
    );
    assert!(
        log[1].contains(" checkout --quiet ") && log[1].ends_with(&inst.base_commit),
        "{}",
        log[1]
    );
    assert!(
        log[2].starts_with("git -C ") && log[2].ends_with(" apply"),
        "{}",
        log[2]
    );
    assert!(log[3].starts_with("python -m pytest "), "{}", log[3]);
    for id in inst.fail_to_pass.iter().chain(&inst.pass_to_pass) {
        assert!(log[3].contains(id.as_str()), "pytest argv lacks {id}");
    }
    assert_eq!(
        std::fs::read_to_string(sb.applied()).unwrap(),
        format!("{}---SEP---\n", inst.test_patch)
    );

    // Variant: one id FAILED -> unresolved.
    let sb2 = Sandbox::new();
    let out = real(&sb2, &inst.fail_to_pass[0]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    assert_eq!(
        so(&out).trim(),
        "astropy__astropy-12907: resolved=false (4 test results)"
    );
}

// ---------------------------------------------------------------- audit-sample

#[test]
fn claim_audit_sample_cli() {
    let sb = Sandbox::new();
    let changes = sb.cwd().join("changes.jsonl");
    std::fs::write(
        &changes,
        "{\"change_id\":\"c1\",\"gates\":[\"propguard\"]}\n{\"change_id\":\"c2\",\"gates\":[\"tdd\"]}\n",
    )
    .unwrap();
    let audits = sb.cwd().join("audits.jsonl");
    std::fs::write(
        &audits,
        "{\"change_id\":\"c1\",\"miss\":true,\"gate\":\"propguard\",\"invariant_hint\":\"auth_kept\"}\n{\"change_id\":\"c2\",\"miss\":false}\n",
    )
    .unwrap();
    let (c, a) = (changes.to_str().unwrap(), audits.to_str().unwrap());

    let out = sb.run(&["audit-sample", c, "--audits", a, "--from-violations"]);
    assert!(!out.status.success());
    assert!(
        se(&out).contains("cannot be used with"),
        "stderr: {}",
        se(&out)
    );

    let out = sb.run(&[
        "audit-sample",
        c,
        "--audits",
        a,
        "--fraction",
        "1.0",
        "--seed",
        "0",
        "--json",
    ]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let v: serde_json::Value = serde_json::from_str(so(&out).trim()).unwrap();
    assert_eq!(v["population"], 2);
    assert_eq!(v["sampled"], serde_json::json!(["c1", "c2"]));
    assert_eq!(
        v["invariant_candidates"],
        serde_json::json!([{"change_id":"c1","invariant":"auth_kept"}])
    );
    assert_eq!(
        v["ratify_queue"],
        serde_json::json!([{"gate":"propguard","change_id":"c1","miss_count":1}])
    );
}

// ------------------------------------------------- additional claims

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if p.is_dir() {
                out.insert(rel, b"<dir>".to_vec());
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn claim_ratify_queue_cli_never_applies() {
    let sb = Sandbox::new();
    // Inputs live in the sandbox root, outside cwd/ home/ tmp/.
    let changes = sb.root.join("changes.jsonl");
    std::fs::write(
        &changes,
        "{\"change_id\":\"c1\",\"gates\":[\"propguard\"]}\n",
    )
    .unwrap();
    let audits = sb.root.join("audits.jsonl");
    std::fs::write(
        &audits,
        "{\"change_id\":\"c1\",\"miss\":true,\"gate\":\"propguard\",\"invariant_hint\":\"auth_kept\"}\n",
    )
    .unwrap();
    let trees = || ["cwd", "home", "tmp"].map(|d| snapshot(&sb.root.join(d)));
    let before = trees();
    let out = sb.run(&[
        "audit-sample",
        changes.to_str().unwrap(),
        "--audits",
        audits.to_str().unwrap(),
        "--fraction",
        "1.0",
        "--json",
    ]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let v: serde_json::Value = serde_json::from_str(so(&out).trim()).unwrap();
    assert!(
        !v["ratify_queue"].as_array().unwrap().is_empty(),
        "report: {v}"
    );
    assert_eq!(before, trees(), "the run wrote or changed files");
}

#[test]
fn claim_malformed_row_path_line() {
    let td = tempfile::tempdir().unwrap();
    let bad = td.path().join("bad.jsonl");
    let first = std::fs::read_to_string(fixture())
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    std::fs::write(&bad, format!("{first}\nthis is not json\n")).unwrap();
    let sb = Sandbox::new();
    let out = sb.run(&["load", bad.to_str().unwrap()]);
    assert!(!out.status.success());
    let want = format!("{}:2", bad.display());
    assert!(se(&out).contains(&want), "stderr: {}", se(&out));
}

static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn claim_from_violations_flag_routes() {
    let sb = Sandbox::new();
    let changes = sb.root.join("changes.jsonl");
    std::fs::write(
        &changes,
        "{\"change_id\":\"c001\",\"gates\":[\"mutategate\"]}\n{\"change_id\":\"c002\",\"gates\":[\"propguard\"]}\n",
    )
    .unwrap();

    // Seed a REAL overwatch violation in the sandbox HOME (overwatch resolves
    // its store under $HOME). HOME is process-global: serialize + restore, and
    // refuse to write unless the resolved path is inside the sandbox.
    {
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", sb.root.join("home"));
        let path = overwatch::store::violations_path(&sb.cwd()).unwrap();
        let inside = path.starts_with(sb.root.join("home"));
        if inside {
            let ev = overwatch::violation::ViolationEvent {
                source: overwatch::violation::ViolationSource::Mutategate,
                signature: "mutategate:op-swap".into(),
                task_key: "c001".into(),
                session_id: "s1".into(),
                ts: 1000,
                detail: None,
            };
            overwatch::store::append_violation(&sb.cwd(), &ev).unwrap();
        }
        match prev {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert!(
            inside,
            "violation store resolved outside the sandbox: {path:?}"
        );
    }

    let out = sb.run(&[
        "audit-sample",
        changes.to_str().unwrap(),
        "--from-violations",
        "--fraction",
        "1.0",
        "--json",
    ]);
    assert!(out.status.success(), "stderr: {}", se(&out));
    let v: serde_json::Value = serde_json::from_str(so(&out).trim()).unwrap();
    assert_eq!(
        v["invariant_candidates"],
        serde_json::json!([{"change_id":"c001","invariant":"mutategate:op-swap"}]),
        "report: {v}"
    );
    assert_eq!(
        v["ratify_queue"],
        serde_json::json!([{"gate":"mutategate","change_id":"c001","miss_count":1}]),
        "report: {v}"
    );
}
