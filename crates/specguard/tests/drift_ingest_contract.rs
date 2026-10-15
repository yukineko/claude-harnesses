// This whole file is integration test code: unwrap/expect/panic are fine here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// RED repros (independent test writer; nothing here is fixed yet).
//
// C2 — `commands/spec-audit.md` step 5 tells the agent to feed the per-shard
//   outputs of `specguard audit --json` back through `specguard ingest`, and
//   `auditmap.rs` says ingest "matches by `label`". But `read_ingest` only
//   matches DRIFT-scope shard labels (area name / `invariants` / `decisions`),
//   while `audit --json` labels shards by map-entry key (`src/main.rs`,
//   `docs/spec.md`). Fail-closed contract asserted below: a provided output
//   whose label matches no shard must never be dropped silently and reported
//   clean, and the documented audit -> ingest path must not lose findings.
//
// C1 — the ingest sections of `commands/run.md` / `commands/spec-audit.md`
//   must handle every exit code `specguard ingest` can return.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn sg(repo: &Path, baseline: &str, sub: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "--baseline",
            baseline,
            "--date",
            "2026-01-01",
        ])
        .args(sub)
        .output()
        .expect("specguard runs")
}

/// Repo with a spec doc + src file committed after `base`, a spec map built, and
/// a config with one area `src`. Returns (base, head).
fn fixture(repo: &Path) -> (String, String) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main(){}\n").unwrap();
    fs::write(repo.join("docs/spec.md"), "# spec\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "add"]);
    let head = git(repo, &["rev-parse", "HEAD"]);
    fs::write(
        repo.join("specguard.toml"),
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", "cat >/dev/null; echo unused"]

[output]
report_dir = "reports"
sentinel = ".pending"

[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#,
    )
    .unwrap();
    let b = sg(repo, &base, &["map", "build"]);
    assert!(b.status.success(), "map build must succeed (fixture)");
    (base, head)
}

/// Run `audit --json`, then build the ingest file EXACTLY as spec-audit.md step 5
/// instructs: one entry per audit shard, `label` copied verbatim, every output
/// carrying a valid marker and `needs_user: yes`.
fn audit_outputs_as_skill_instructs(repo: &Path, base: &str) -> (Vec<String>, std::path::PathBuf) {
    let a = sg(repo, base, &["audit", "--json"]);
    assert!(a.status.success(), "audit --json must succeed (fixture)");
    let env: Value = serde_json::from_slice(&a.stdout).expect("audit --json is JSON");
    let labels: Vec<String> = env["shards"]
        .as_array()
        .expect("shards array")
        .iter()
        .map(|s| s["label"].as_str().unwrap().to_string())
        .collect();
    assert!(!labels.is_empty(), "fixture must yield audit shards");
    let shards: Vec<Value> = labels
        .iter()
        .map(|l| {
            serde_json::json!({
                "label": l,
                "stdout": "# audit\n\nAUDITBODY\n\n<<<SPEC_AUDIT>>>\nneeds_user: yes\nsummary: audit drift found",
                "code": 0
            })
        })
        .collect();
    let p = repo.join(".specguard-audit-ingest.json");
    fs::write(&p, serde_json::json!({ "shards": shards }).to_string()).unwrap();
    (labels, p)
}

fn report_text(repo: &Path) -> String {
    fs::read_to_string(repo.join("reports/2026-01-01.md")).unwrap_or_default()
}

/// C2a (fail-open): empty drift scope + outputs provided whose labels match no
/// shard. Today: exit 0 "監査対象なし", report/sentinel clean. The provided
/// `needs_user: yes` outputs are silently discarded. Must NOT be exit 0.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn c2_ingest_with_unmatched_labels_and_empty_scope_must_not_report_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (_base, head) = fixture(repo);
    let (_labels, ingest) = audit_outputs_as_skill_instructs(repo, &head);
    // baseline == HEAD => empty drift scope (no shards), yet outputs were provided.
    let o = sg(repo, &head, &["ingest", "--from", ingest.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);
    let sentinel = repo.join(".pending").exists();
    assert!(
        o.status.code() != Some(0) || sentinel,
        "FAIL-OPEN: ingest was handed 2 audit outputs (needs_user: yes) whose labels match no \
         shard; it exited 0 with no sentinel, silently dropping them.\n\
         exit={:?}\nstdout={stdout}\nstderr={stderr}\nsentinel_exists={sentinel}",
        o.status.code()
    );
}

/// C2b: the documented audit -> ingest path itself. Non-empty drift scope, labels
/// copied verbatim from `audit --json`. Whatever the matching rule, the audit
/// findings must not be silently lost: after ingest, the provided audit output
/// must be present in the report AND the `needs_user: yes` must raise the
/// sentinel. Today: exit 4 "no output provided for shard 'src'", the report is
/// never written, and the audit outputs are discarded.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn c2_audit_json_outputs_fed_to_ingest_as_documented_must_not_be_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (base, _head) = fixture(repo);
    let (labels, ingest) = audit_outputs_as_skill_instructs(repo, &base);
    let o = sg(repo, &base, &["ingest", "--from", ingest.to_str().unwrap()]);
    let report = report_text(repo);
    let sentinel = repo.join(".pending").exists();
    assert!(
        report.contains("AUDITBODY") && sentinel,
        "audit findings fed to `specguard ingest` exactly as spec-audit.md step 5 instructs \
         (labels {labels:?}) were lost: report contains AUDITBODY = {}, sentinel = {sentinel}.\n\
         exit={:?}\nstderr={}",
        report.contains("AUDITBODY"),
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
}

// ---------------------------------------------------------------- C1

fn manifest(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

/// Body of top-level `fn <name>` in main.rs: from its `fn` line to the next line
/// starting with `}` at column 0.
fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("\nfn {name}("))
        .unwrap_or_else(|| panic!("fn {name} not found in main.rs"));
    let rest = &src[start + 1..];
    let end = rest.find("\n}\n").expect("fn end");
    &rest[..end]
}

fn const_value(src: &str, name: &str) -> u8 {
    let key = format!("const {name}: u8 = ");
    let i = src
        .find(&key)
        .unwrap_or_else(|| panic!("{name} not defined"));
    src[i + key.len()..]
        .split(';')
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Every exit code `specguard ingest` can return, derived from main.rs:
/// the `Command::Ingest` arm's callees `ratification_block` and `finish`, plus
/// `EXIT_USAGE` (what `main` maps any `Err` — bad JSON, unreadable file — to).
fn ingest_exit_codes() -> Vec<(String, u8)> {
    let src = manifest("src/main.rs");
    let mut names: Vec<String> = Vec::new();
    for f in ["ratification_block", "finish", "read_ingest"] {
        let body = fn_body(&src, f);
        for tok in body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if tok.starts_with("EXIT_") && !names.contains(&tok.to_string()) {
                names.push(tok.to_string());
            }
        }
    }
    // main(): `Err(e) => ... ExitCode::from(EXIT_USAGE)` — any `?` in the Ingest arm.
    assert!(fn_body(&src, "main").contains("EXIT_USAGE"));
    names.push("EXIT_USAGE".to_string());
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|n| {
            let v = const_value(&src, &n);
            (n, v)
        })
        .collect()
}

/// The section of an instruction file that interprets ingest's exit code: from
/// the `specguard ingest --from` line to the next `## ` heading.
fn ingest_section(md: &str) -> &str {
    let i = md
        .find("specguard ingest --from")
        .expect("ingest invocation present");
    let rest = &md[i..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

/// Sanity (not ignored): the derivation finds the real codes, including 5.
#[test]
fn c1_derivation_sees_unratified_and_usage_codes() {
    let codes = ingest_exit_codes();
    for want in [0u8, 2, 3, 4, 5] {
        assert!(
            codes.iter().any(|(_, v)| *v == want),
            "derived ingest exit codes {codes:?} lack {want}"
        );
    }
}

/// Observation that ingest really exits 5 on an unratified spec (not ignored).
#[test]
fn c1_ingest_really_exits_5_when_unratified() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (base, _head) = fixture(repo);
    fs::write(
        repo.join("tmpl.md"),
        "{{PROJECT_NAME}} {{DATE}} {{SCOPE_SUMMARY}} {{AREAS}} {{INVARIANTS}}\n{{MARKER}}\n",
    )
    .unwrap();
    let cfg = fs::read_to_string(repo.join("specguard.toml")).unwrap();
    fs::write(
        repo.join("specguard.toml"),
        format!("{cfg}\n[prompt]\ntemplate = \"tmpl.md\"\nrequire_ratification = true\n"),
    )
    .unwrap();
    fs::write(repo.join("in.json"), r#"{"shards":[]}"#).unwrap();
    let o = sg(repo, &base, &["ingest", "--from", "in.json"]);
    assert_eq!(
        o.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
#[ignore = "backlog TBD: RED repro"]
fn c1_ingest_instructions_handle_every_ingest_exit_code() {
    let codes = ingest_exit_codes();
    let mut gaps = Vec::new();
    for file in ["commands/run.md", "commands/spec-audit.md"] {
        let md = manifest(file);
        let sec = ingest_section(&md);
        for (name, v) in &codes {
            if !sec.contains(&format!("exit {v}")) && !sec.contains(&format!("exit **{v}**")) {
                gaps.push(format!(
                    "{file}: ingest section never mentions `exit {v}` ({name})"
                ));
            }
        }
    }
    assert!(
        gaps.is_empty(),
        "ingest exit codes {codes:?} not all handled:\n{}",
        gaps.join("\n")
    );
}
