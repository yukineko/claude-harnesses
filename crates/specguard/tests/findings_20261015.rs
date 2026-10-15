// This whole file is integration test code: unwrap/expect/panic are fine here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// RED repros written by an independent test writer (2026-10-15). Nothing here
// is fixed yet; each test is `#[ignore]`d until its backlog item lands.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DATE: &str = "2026-01-01";

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

/// Run the built binary in `repo` with an isolated HOME (the overwatch review
/// store lives under `$HOME/.overwatch`, so findings never touch the real one).
fn sg(repo: &Path, home: &Path, baseline: &str, sub: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("SPECGUARD_BASELINE_REF")
        .args([
            "--config",
            "specguard.toml",
            "--baseline",
            baseline,
            "--date",
            DATE,
        ])
        .args(sub)
        .output()
        .expect("specguard runs")
}

/// Same fixture as `drift_ingest_contract.rs`: a spec doc + src file committed
/// after `base`, a spec map built, one area `src`. Returns `base`.
fn fixture(repo: &Path, home: &Path) -> String {
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
    let b = sg(repo, home, &base, &["map", "build"]);
    assert!(b.status.success(), "map build must succeed (fixture)");
    base
}

fn labels_of(stdout: &[u8]) -> Vec<String> {
    let env: Value = serde_json::from_slice(stdout).expect("JSON envelope");
    env["shards"]
        .as_array()
        .expect("shards array")
        .iter()
        .map(|s| s["label"].as_str().unwrap().to_string())
        .collect()
}

/// Write an ingest file answering every label with `body` + marker + verdict.
fn ingest_file(
    repo: &Path,
    name: &str,
    labels: &[String],
    body: &str,
    needs_user: &str,
) -> PathBuf {
    let shards: Vec<Value> = labels
        .iter()
        .map(|l| {
            serde_json::json!({
                "label": l,
                "stdout": format!(
                    "# report\n\n{body}\n\n<<<SPEC_AUDIT>>>\nneeds_user: {needs_user}\nsummary: {body} found"
                ),
                "code": 0
            })
        })
        .collect();
    let p = repo.join(name);
    fs::write(&p, serde_json::json!({ "shards": shards }).to_string()).unwrap();
    p
}

/// Ingest audit outputs (`audit --json` labels, `--kind audit`), then drift
/// outputs (`prompt --json` labels, `--kind drift`), both on the same `--date`.
/// Both ingests must succeed (fixture); returns nothing — callers inspect files.
fn audit_then_drift(repo: &Path, home: &Path, base: &str, drift_needs_user: &str) {
    let a = sg(repo, home, base, &["audit", "--json"]);
    assert!(a.status.success(), "audit --json must succeed (fixture)");
    let audit_labels = labels_of(&a.stdout);
    assert!(!audit_labels.is_empty(), "fixture must yield audit shards");
    let ai = ingest_file(repo, "audit-in.json", &audit_labels, "AUDITBODY", "yes");
    let o = sg(
        repo,
        home,
        base,
        &["ingest", "--from", ai.to_str().unwrap(), "--kind", "audit"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "audit ingest (fixture) failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    // Observe that the audit report really existed before the drift ingest, so
    // a later absence can only mean it was destroyed, not never written.
    let audit_report = fs::read_to_string(repo.join(format!("reports/{DATE}.md"))).unwrap();
    assert!(
        audit_report.contains("AUDITBODY"),
        "fixture: audit ingest must have written its report"
    );

    let p = sg(repo, home, base, &["prompt", "--json"]);
    assert!(p.status.success(), "prompt --json must succeed (fixture)");
    let drift_labels = labels_of(&p.stdout);
    assert!(!drift_labels.is_empty(), "fixture must yield drift shards");
    let di = ingest_file(
        repo,
        "drift-in.json",
        &drift_labels,
        "DRIFTBODY",
        drift_needs_user,
    );
    let o = sg(
        repo,
        home,
        base,
        &["ingest", "--from", di.to_str().unwrap(), "--kind", "drift"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "drift ingest (fixture) failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn all_files_under(dir: &Path, acc: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                all_files_under(&p, acc);
            } else {
                acc.push(p);
            }
        }
    }
}

fn covers_of(sentinel: &str) -> Vec<String> {
    sentinel
        .lines()
        .filter_map(|l| l.strip_prefix("covers:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Defect 1: the spec-audit report (`finish_audit`) and the drift report
/// (`finish`) are both written to `reports/<date>.md` via the same
/// `report::Paths.report`, so a drift ingest after an audit ingest on the same
/// day overwrites the audit report. The sentinel raised by the audit still says
/// `report: reports/<date>.md`, pointing a human at a file that no longer holds
/// the audit findings.
///
/// Expected: after an audit ingest then a drift ingest on the same date, the
/// audit report's content (AUDITBODY) is still readable in some file under
/// `reports/` (distinct path or append — either fix satisfies this), and the
/// drift report's content (DRIFTBODY) is too.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn audit_report_survives_a_same_day_drift_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());
    audit_then_drift(repo, home.path(), &base, "no");

    let mut files = Vec::new();
    all_files_under(&repo.join("reports"), &mut files);
    let contents: Vec<(PathBuf, String)> = files
        .iter()
        .map(|p| (p.clone(), fs::read_to_string(p).unwrap_or_default()))
        .collect();
    let has = |needle: &str| contents.iter().any(|(_, c)| c.contains(needle));
    assert!(
        has("DRIFTBODY"),
        "fixture: drift report must exist; files = {files:?}"
    );
    assert!(
        has("AUDITBODY"),
        "the spec-audit report was overwritten by the same-day drift report: no file \
         under reports/ contains AUDITBODY any more. files = {:?}",
        contents
            .iter()
            .map(|(p, c)| format!("{}: {:.120}", p.display(), c.replace('\n', " | ")))
            .collect::<Vec<_>>()
    );
}

/// Defect 2: drift `finish` writes the sentinel with only the ids it just
/// emitted (`report::write_sentinel(.., &covers)` where `covers =
/// emit_drift_findings(..)`), overwriting an already-pending sentinel. An
/// earlier spec-audit sentinel's `covers:` ids are lost, so the eventual `ack`
/// cannot dispose of those findings. `finish_audit` already does the union in
/// the other direction.
///
/// Expected: after an audit ingest (needs_user: yes) then a drift ingest
/// (needs_user: yes), the sentinel's `covers:` ids are a superset of the ids
/// the audit sentinel carried.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn drift_sentinel_keeps_pending_audit_covers() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());

    // Capture the audit sentinel's covers between the two ingests.
    let a = sg(repo, home.path(), &base, &["audit", "--json"]);
    assert!(a.status.success());
    let audit_labels = labels_of(&a.stdout);
    let ai = ingest_file(repo, "audit-in.json", &audit_labels, "AUDITBODY", "yes");
    let o = sg(
        repo,
        home.path(),
        &base,
        &["ingest", "--from", ai.to_str().unwrap(), "--kind", "audit"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let audit_covers = covers_of(&fs::read_to_string(repo.join(".pending")).unwrap());
    assert!(
        !audit_covers.is_empty(),
        "fixture: the audit sentinel must carry covers ids"
    );

    let p = sg(repo, home.path(), &base, &["prompt", "--json"]);
    assert!(p.status.success());
    let drift_labels = labels_of(&p.stdout);
    let di = ingest_file(repo, "drift-in.json", &drift_labels, "DRIFTBODY", "yes");
    let o = sg(
        repo,
        home.path(),
        &base,
        &["ingest", "--from", di.to_str().unwrap(), "--kind", "drift"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    let after = fs::read_to_string(repo.join(".pending")).unwrap();
    let after_covers = covers_of(&after);
    let lost: Vec<&String> = audit_covers
        .iter()
        .filter(|id| !after_covers.contains(id))
        .collect();
    assert!(
        lost.is_empty(),
        "drift ingest overwrote the pending sentinel and dropped the audit's covers ids {lost:?}.\n\
         before = {audit_covers:?}\nsentinel after =\n{after}"
    );
}

// ---------------------------------------------------------------- defect 4

/// Long flags (`--x`) listed in the `Options:` section of a clap help text.
fn long_flags(help: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_opts = false;
    for line in help.lines() {
        if line.trim_end() == "Options:" {
            in_opts = true;
            continue;
        }
        if in_opts && !line.starts_with(' ') && !line.trim().is_empty() {
            in_opts = false;
        }
        // Only flag-definition lines (`  -c, --config <X>` / `      --from <X>`);
        // description lines also contain `--words` and must not be parsed.
        // `Possible values:` bullets (`- drift: ...`) start with "- " and are
        // descriptions too.
        let t = line.trim_start();
        if !in_opts || !t.starts_with('-') || t.starts_with("- ") {
            continue;
        }
        for tok in line.split(|c: char| c.is_whitespace() || c == ',') {
            if let Some(name) = tok.strip_prefix("--") {
                let name: String = name
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !name.is_empty() {
                    let f = format!("--{name}");
                    if !out.contains(&f) {
                        out.push(f);
                    }
                    break; // only the flag's own name, not words in its description
                }
            }
        }
    }
    out
}

fn bin_help(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .args(args)
        .output()
        .expect("specguard --help runs");
    assert!(o.status.success(), "{args:?} must succeed");
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// Defect 4: the README / README.ja "Subcommands (binary)" reference documents
/// `specguard ingest [--from <file>]` only, while the binary also takes
/// `--kind` and `--filter` (documented in commands/run.md and
/// commands/spec-audit.md and required for the audit path).
///
/// Expected: every ingest-specific long flag the LIVE binary advertises in
/// `specguard ingest --help` (minus `--help` and the global flags shown by
/// `specguard --help`) appears on some `specguard ingest` line of each README's
/// subcommand reference. Derived from the binary, so a future flag is caught too.
#[test]
#[ignore = "backlog TBD: RED repro"]
fn readme_ingest_usage_lists_every_ingest_flag() {
    let global = long_flags(&bin_help(&["--help"]));
    let ingest: Vec<String> = long_flags(&bin_help(&["ingest", "--help"]))
        .into_iter()
        .filter(|f| f != "--help" && f != "--version" && !global.contains(f))
        .collect();
    assert!(
        ingest.contains(&"--from".to_string()),
        "harness sanity: parsed ingest flags {ingest:?} must include --from"
    );

    let mut gaps = Vec::new();
    for readme in ["README.md", "README.ja.md"] {
        let md = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(readme)).unwrap();
        let usage: Vec<&str> = md
            .lines()
            .filter(|l| l.trim_start().starts_with("specguard ingest"))
            .collect();
        assert!(
            !usage.is_empty(),
            "{readme}: no `specguard ingest` usage line"
        );
        for f in &ingest {
            if !usage.iter().any(|l| l.contains(f.as_str())) {
                gaps.push(format!(
                    "{readme}: `specguard ingest` usage never mentions {f}"
                ));
            }
        }
    }
    assert!(
        gaps.is_empty(),
        "binary ingest flags {ingest:?} undocumented in README usage:\n{}",
        gaps.join("\n")
    );
}
