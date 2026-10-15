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

// ------------------------------------------- follow-up: coverage of the fix
//
// The fix keeps both reports in `<report_dir>/<date>.md`, with the spec-audit
// report as a marked section and the drift report owning everything outside
// it, and makes drift `finish` union a pending sentinel's covers (refusing when
// the sentinel exists but cannot be read). These pin the paths the first three
// tests did not reach.

/// One ingest of `kind` ("audit" | "drift") answering every shard the matching
/// emitter (`audit --json` / `prompt --json`) currently produces with
/// `stdout_body` + marker + `needs_user`. Returns the ingest's raw output.
fn ingest_kind(
    repo: &Path,
    home: &Path,
    base: &str,
    kind: &str,
    stdout_body: &str,
    needs_user: &str,
) -> Output {
    let emitter: &[&str] = if kind == "audit" {
        &["audit", "--json"]
    } else {
        &["prompt", "--json"]
    };
    let e = sg(repo, home, base, emitter);
    assert!(e.status.success(), "{emitter:?} must succeed (fixture)");
    let labels = labels_of(&e.stdout);
    assert!(!labels.is_empty(), "fixture must yield {kind} shards");
    let f = ingest_file(
        repo,
        &format!("{kind}-in.json"),
        &labels,
        stdout_body,
        needs_user,
    );
    sg(
        repo,
        home,
        base,
        &["ingest", "--from", f.to_str().unwrap(), "--kind", kind],
    )
}

fn ok(o: &Output, what: &str) {
    assert_eq!(
        o.status.code(),
        Some(0),
        "{what} (fixture) failed: stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

fn report_path(repo: &Path) -> PathBuf {
    repo.join(format!("reports/{DATE}.md"))
}

/// The file a sentinel's `report:` line names, resolved against the repo root.
fn sentinel_report_text(repo: &Path, sentinel: &str) -> (String, String) {
    let rel = sentinel
        .lines()
        .find_map(|l| l.strip_prefix("report:"))
        .map(|v| v.trim().to_string())
        .expect("sentinel has a report: line");
    let text = fs::read_to_string(repo.join(&rel)).unwrap_or_default();
    (rel, text)
}

/// (a) Reverse order of defect 1/2: a drift ingest (needs_user: yes) then a
/// same-day spec-audit ingest (needs_user: yes). Expected: both report bodies
/// survive in the dated report; the drift sentinel's `report:` line and the
/// audit sentinel's `report:` line each resolve to a file that (still) holds
/// that run's findings; the final sentinel's covers are the union of both.
#[test]
fn followup_a_drift_then_audit_same_day_keeps_both() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());

    ok(
        &ingest_kind(repo, home.path(), &base, "drift", "DRIFTBODY", "yes"),
        "drift ingest",
    );
    let drift_sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    let drift_covers = covers_of(&drift_sentinel);
    let (drift_rel, drift_text) = sentinel_report_text(repo, &drift_sentinel);
    assert!(
        drift_text.contains("DRIFTBODY") && !drift_covers.is_empty(),
        "fixture: drift sentinel must name a report holding DRIFTBODY and carry covers"
    );

    ok(
        &ingest_kind(repo, home.path(), &base, "audit", "AUDITBODY", "yes"),
        "audit ingest",
    );
    let audit_sentinel = fs::read_to_string(repo.join(".pending")).unwrap();
    let (audit_rel, audit_text) = sentinel_report_text(repo, &audit_sentinel);
    let drift_text_after = fs::read_to_string(repo.join(&drift_rel)).unwrap_or_default();

    assert!(
        audit_text.contains("AUDITBODY"),
        "audit sentinel's report: {audit_rel} does not hold AUDITBODY:\n{audit_text}"
    );
    assert!(
        drift_text_after.contains("DRIFTBODY"),
        "the drift report ({drift_rel}, named by the drift sentinel) lost DRIFTBODY after a \
         same-day audit ingest:\n{drift_text_after}"
    );
    let after = covers_of(&audit_sentinel);
    let lost: Vec<&String> = drift_covers.iter().filter(|c| !after.contains(c)).collect();
    assert!(
        lost.is_empty() && after.iter().any(|c| c.starts_with("specguard:spec-audit:")),
        "covers union broken: lost drift ids {lost:?}; sentinel =\n{audit_sentinel}"
    );
}

/// (b) A same-day spec-audit RE-run replaces only the audit section: the first
/// audit's body is gone, the second's is present, the drift body is intact.
#[test]
fn followup_b_audit_rerun_replaces_only_audit_section() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());

    ok(
        &ingest_kind(repo, home.path(), &base, "drift", "DRIFTBODY", "yes"),
        "drift ingest",
    );
    ok(
        &ingest_kind(repo, home.path(), &base, "audit", "AUDITBODYONE", "yes"),
        "first audit ingest",
    );
    ok(
        &ingest_kind(repo, home.path(), &base, "audit", "AUDITBODYTWO", "yes"),
        "second audit ingest",
    );
    let text = fs::read_to_string(report_path(repo)).unwrap();
    assert!(
        text.contains("DRIFTBODY"),
        "drift body lost by an audit re-run:\n{text}"
    );
    assert!(
        text.contains("AUDITBODYTWO"),
        "second audit body missing:\n{text}"
    );
    assert!(
        !text.contains("AUDITBODYONE"),
        "first audit body was not replaced by the re-run (stale findings kept):\n{text}"
    );
}

/// (c) Refusal: the dated report already exists with an OPENING spec-audit
/// marker and no closing one (its extent cannot be determined). Both kinds of
/// ingest must exit non-zero, leave the report byte-identical, and raise no
/// sentinel pointing at a report that does not hold their findings.
#[test]
fn followup_c_unclosed_audit_section_refuses_both_kinds() {
    for kind in ["drift", "audit"] {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let base = fixture(repo, home.path());
        fs::create_dir_all(repo.join("reports")).unwrap();
        let original =
            "# earlier drift\n\nKEEPME\n\n<!-- specguard:spec-audit -->\n# audit\nOLDAUDIT\n";
        fs::write(report_path(repo), original).unwrap();

        let o = ingest_kind(repo, home.path(), &base, kind, "NEWBODY", "yes");
        let after = fs::read_to_string(report_path(repo)).unwrap();
        let sentinel = repo.join(".pending").exists();
        assert!(
            o.status.code() != Some(0) && after == original && !sentinel,
            "{kind} ingest over an unclosed spec-audit section must refuse without writing: \
             exit={:?} report_unchanged={} sentinel_raised={sentinel}\nreport now:\n{after}\nstderr={}",
            o.status.code(),
            after == original,
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// (d) Refusal: a pending sentinel that EXISTS but cannot be read (mode 0200:
/// writable, not readable — so a blind overwrite would succeed). A drift
/// ingest with findings must exit non-zero, must not overwrite the sentinel,
/// and (the fix reads before emitting) must not have put drift findings on the
/// review queue that no sentinel covers.
#[cfg(unix)]
#[test]
fn followup_d_unreadable_pending_sentinel_refuses_drift_raise() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());
    let sp = repo.join(".pending");
    let original = "date: 2025-12-31\nreport: reports/x.md\nsummary: s\nraised_at: abc\ncovers: specguard:spec-audit:OLDID\n";
    fs::write(&sp, original).unwrap();
    fs::set_permissions(&sp, fs::Permissions::from_mode(0o200)).unwrap();
    // Harness sanity: the file really is unreadable to us (not running as root).
    assert!(
        fs::read_to_string(&sp).is_err(),
        "fixture: sentinel must be unreadable"
    );

    let o = ingest_kind(repo, home.path(), &base, "drift", "DRIFTBODY", "yes");
    fs::set_permissions(&sp, fs::Permissions::from_mode(0o600)).unwrap();
    let after = fs::read_to_string(&sp).unwrap();

    let mut store = Vec::new();
    all_files_under(home.path(), &mut store);
    let queued_drift: Vec<String> = store
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "review_findings.jsonl"))
        .flat_map(|p| {
            fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("specguard:spec-drift:"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();

    assert!(
        o.status.code() != Some(0) && after == original && queued_drift.is_empty(),
        "drift ingest over an unreadable pending sentinel must refuse without side effects: \
         exit={:?} sentinel_unchanged={} queued_uncovered_drift={queued_drift:?}\n\
         sentinel now:\n{after}\nstderr={}",
        o.status.code(),
        after == original,
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (e) In-band marker injection. The section markers are plain text, and an
/// auditor's report body is copied into the section verbatim. A spec-audit
/// output that itself contains the closing marker (e.g. a report quoting this
/// very file format) makes `split_report` end the section early; the rest of
/// the audit body is then treated as drift-owned text and discarded by the next
/// drift write. Expected: after audit (body contains the closing marker) then a
/// same-day drift ingest, the whole audit body — including the text after the
/// quoted marker — is still in the report.
#[test]
fn followup_e_audit_body_quoting_close_marker_is_not_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = fixture(repo, home.path());
    ok(
        &ingest_kind(
            repo,
            home.path(),
            &base,
            "audit",
            "AUDITHEAD\n<!-- /specguard:spec-audit -->\nAUDITTAIL",
            "yes",
        ),
        "audit ingest",
    );
    let before = fs::read_to_string(report_path(repo)).unwrap();
    assert!(
        before.contains("AUDITTAIL"),
        "fixture: audit report holds its tail"
    );
    ok(
        &ingest_kind(repo, home.path(), &base, "drift", "DRIFTBODY", "yes"),
        "drift ingest",
    );
    let after = fs::read_to_string(report_path(repo)).unwrap();
    assert!(
        after.contains("AUDITHEAD") && after.contains("AUDITTAIL") && after.contains("DRIFTBODY"),
        "audit section truncated at the quoted closing marker by a same-day drift write:\n{after}"
    );
}
