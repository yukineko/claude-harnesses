// 丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for `skills/continuous-audit/SKILL.md` claims about
//! `scripts/continuous-audit.sh` (and the overwatch binary it drives). One
//! `claim_<id>` test per claim. All state lives in temp HOME / temp cwd /
//! temp repo copies; the real ledger and real $HOME are never touched.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

fn ow_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn script() -> PathBuf {
    repo_root().join("scripts/continuous-audit.sh")
}

const DEFAULT_TARGETS: &str =
    "blastguard,propguard,specguard,stuckguard,mutategate,overwatch,parallelguard,backlog";

struct Sandbox {
    _tmp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("work");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    Sandbox {
        _tmp: tmp,
        home,
        work,
    }
}

impl Sandbox {
    /// Run the real script with the freshly built overwatch binary.
    fn ca(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(script())
            .args(args)
            .env("OVERWATCH_BIN", ow_bin())
            .env("HOME", &self.home)
            .current_dir(&self.work)
            .output()
            .unwrap()
    }
    fn ow(&self, args: &[&str]) -> Output {
        Command::new(ow_bin())
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.work)
            .output()
            .unwrap()
    }
    /// Find a ledger file by name anywhere under the sandbox HOME.
    fn ledger(&self, name: &str) -> Option<PathBuf> {
        fn walk(dir: &Path, name: &str) -> Option<PathBuf> {
            for e in fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(f) = walk(&p, name) {
                        return Some(f);
                    }
                } else if p.file_name().is_some_and(|n| n == name) {
                    return Some(p);
                }
            }
            None
        }
        walk(&self.home, name)
    }
    fn jsonl(&self, name: &str) -> Vec<serde_json::Value> {
        let Some(p) = self.ledger(name) else {
            return vec![];
        };
        fs::read_to_string(p)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn findings(&self) -> Vec<serde_json::Value> {
        self.jsonl("review_findings.jsonl")
    }
    fn finding(&self, id: &str) -> serde_json::Value {
        self.findings()
            .into_iter()
            .find(|f| f["finding_id"] == id)
            .unwrap_or_else(|| panic!("finding {id} not in review store"))
    }
    fn metrics(&self) -> serde_json::Value {
        let o = self.ow(&["audit-metrics", "--json"]);
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn queue(&self) -> Vec<serde_json::Value> {
        let o = self.ow(&["review-queue", "--json"]);
        serde_json::from_slice(&o.stdout).unwrap()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn str_of(v: &serde_json::Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------

/// Lines of `path` that mention continuous-audit and are neither comments nor
/// heredoc body text (i.e. lines that could actually execute it).
fn executable_mentions(path: &Path) -> Vec<String> {
    let body = fs::read_to_string(path).unwrap_or_default();
    let mut out = vec![];
    let mut heredoc_end: Option<String> = None;
    for line in body.lines() {
        if let Some(end) = &heredoc_end {
            if line.trim() == end {
                heredoc_end = None;
            }
            continue;
        }
        // A real heredoc opener is `<<TAG` / `<<'TAG'` / `<<-TAG` (not `<<<`).
        let bytes = line.as_bytes();
        for (i, _) in line.match_indices("<<") {
            if i > 0 && bytes[i - 1] == b'<' || bytes.get(i + 2) == Some(&b'<') {
                continue;
            }
            let tag: String = line[i + 2..]
                .trim_start_matches('-')
                .trim_start()
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !tag.is_empty() {
                heredoc_end = Some(tag);
            }
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.contains("continuous-audit") {
            out.push(line.to_string());
        }
    }
    out
}

#[test]
fn claim_ca_opt_in_not_wired() {
    let repo = repo_root();
    // 1. No plugin hooks.json / hook file references the script or the skill.
    let mut hook_files = 0;
    for e in fs::read_dir(repo.join("crates")).unwrap().flatten() {
        let hooks = e.path().join("hooks");
        if !hooks.is_dir() {
            continue;
        }
        for h in fs::read_dir(&hooks).unwrap().flatten() {
            if h.path().is_file() {
                hook_files += 1;
                let body = fs::read_to_string(h.path()).unwrap_or_default();
                assert!(
                    !body.contains("continuous-audit"),
                    "{} wires continuous-audit",
                    h.path().display()
                );
            }
        }
    }
    assert!(hook_files > 10, "scan was vacuous: {hook_files} hook files");
    // 2. Claude settings do not reference it.
    let settings = fs::read_to_string(repo.join(".claude/settings.json")).unwrap();
    assert!(!settings.contains("continuous-audit"));
    // 3. .githooks/* never EXECUTE it (comments and advisory heredoc text only).
    let mut git_hooks = 0;
    for h in fs::read_dir(repo.join(".githooks")).unwrap().flatten() {
        git_hooks += 1;
        let m = executable_mentions(&h.path());
        assert!(m.is_empty(), "{} executes: {m:?}", h.path().display());
    }
    assert!(git_hooks >= 5);
    // 4. rollout / rebuild scripts and the condukt crate do not invoke it.
    for rel in ["scripts/rollout-plugins.sh", "scripts/rebuild-plugins.sh"] {
        let m = executable_mentions(&repo.join(rel));
        assert!(m.is_empty(), "{rel} executes: {m:?}");
    }
    fn walk(dir: &Path, hits: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, hits);
            } else if let Ok(b) = fs::read_to_string(&p) {
                if b.contains("continuous-audit.sh") {
                    hits.push(p);
                }
            }
        }
    }
    let mut hits = vec![];
    for sub in ["src", "skills", "hooks", "bin"] {
        let d = repo.join("crates/condukt").join(sub);
        if d.is_dir() {
            walk(&d, &mut hits);
        }
    }
    assert!(hits.is_empty(), "condukt references the script: {hits:?}");
}

// ---------------------------------------------------------------------------

fn fake_exe(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn claim_ca_binary_resolution() {
    // A throwaway repo layout so REPO (= script dir/..) points at temp.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::copy(script(), repo.join("scripts/continuous-audit.sh")).unwrap();
    let envbin = tmp.path().join("envbin/overwatch");
    let pathdir = tmp.path().join("pathdir");
    let on_path = pathdir.join("overwatch");
    let release = repo.join("target/release/overwatch");
    let debug = repo.join("target/debug/overwatch");
    for p in [&envbin, &on_path, &release, &debug] {
        fake_exe(p);
    }
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let run = |ovbin: Option<&Path>| -> String {
        let mut c = Command::new("/bin/bash");
        c.arg(repo.join("scripts/continuous-audit.sh"))
            .arg("--dry-run")
            .env("PATH", format!("{}:/usr/bin:/bin", pathdir.display()))
            .env("HOME", &home)
            .current_dir(tmp.path());
        match ovbin {
            Some(p) => c.env("OVERWATCH_BIN", p),
            None => c.env_remove("OVERWATCH_BIN"),
        };
        text(&c.output().unwrap())
    };
    let chosen = |out: &str| -> String {
        out.lines()
            .find_map(|l| l.trim().strip_prefix("overwatch: ").map(str::to_string))
            .unwrap_or_else(|| panic!("no 'overwatch:' line in:\n{out}"))
    };

    // Tier 1: OVERWATCH_BIN wins over everything.
    assert_eq!(chosen(&run(Some(&envbin))), envbin.display().to_string());
    // Tier 2: PATH.
    assert_eq!(chosen(&run(None)), on_path.display().to_string());
    // Tier 3: target/release once PATH has none.
    fs::remove_file(&on_path).unwrap();
    assert_eq!(chosen(&run(None)), release.display().to_string());
    // Tier 4: target/debug once release is gone too.
    fs::remove_file(&release).unwrap();
    assert_eq!(chosen(&run(None)), debug.display().to_string());
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_target_override() {
    let sb = sandbox();
    let d = sb.ca(&["--dry-run", "--round", "1"]);
    assert!(
        text(&d).contains(&format!("targets: {DEFAULT_TARGETS}")),
        "{}",
        text(&d)
    );
    let d = sb.ca(&["--dry-run", "--round", "1", "--target", "a,b"]);
    let t = text(&d);
    assert!(t.contains("targets: a,b"), "{t}");
    assert!(!t.contains(DEFAULT_TARGETS), "{t}");

    // Real run records the override in the audit-round ledger.
    let r = sb.ca(&["--round", "R1", "--target", "a,b"]);
    assert!(r.status.success(), "{}", text(&r));
    let rounds = sb.jsonl("audit_rounds.jsonl");
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0]["targets"], serde_json::json!(["a", "b"]));
    // ...and without --target the default set is what gets recorded.
    let r = sb.ca(&["--round", "R2"]);
    assert!(r.status.success(), "{}", text(&r));
    let rounds = sb.jsonl("audit_rounds.jsonl");
    let want: Vec<&str> = DEFAULT_TARGETS.split(',').collect();
    assert_eq!(rounds[1]["targets"], serde_json::json!(want));
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_unverified_finding_flag_records_unverified() {
    let sb = sandbox();
    // Dry run lists the two verdict tokens.
    let d = sb.ca(&[
        "--dry-run",
        "--round",
        "R1",
        "--finding",
        "C-1|high|c|f.rs|why",
        "--unverified-finding",
        "U-1|high|u|g.rs|why",
    ]);
    let t = text(&d);
    let conf_line = t
        .lines()
        .find(|l| l.contains("C-1") && l.contains("record-finding"))
        .unwrap_or_else(|| panic!("{t}"));
    let unv_line = t
        .lines()
        .find(|l| l.contains("U-1") && l.contains("record-finding"))
        .unwrap_or_else(|| panic!("{t}"));
    assert!(conf_line.contains("record-finding --verdict confirmed"));
    assert!(unv_line.contains("record-finding --verdict unverified"));
    assert!(sb.findings().is_empty(), "dry-run must not write findings");

    // Real run stores the verdicts and the queue marks the unverified row.
    let r = sb.ca(&[
        "--round",
        "R1",
        "--new-findings",
        "2",
        "--confirmed",
        "1",
        "--unverified",
        "1",
        "--regression-tests-added",
        "1",
        "--finding",
        "C-1|high|c sum|f.rs|why",
        "--unverified-finding",
        "U-1|high|u sum|g.rs|why",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(sb.finding("C-1")["verdict"], "confirmed");
    assert_eq!(sb.finding("U-1")["verdict"], "unverified");
    let q = sb.queue();
    let u = q.iter().find(|r| r["identifier"] == "U-1").unwrap();
    assert!(str_of(u, "summary").contains("[UNVERIFIED]"), "{u}");
    let c = q.iter().find(|r| r["identifier"] == "C-1").unwrap();
    assert!(!str_of(c, "summary").contains("[UNVERIFIED]"), "{c}");
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_rationale_flag() {
    let sb = sandbox();
    // CLI: --rationale stored verbatim.
    let why = "because src/x.rs:42 unwraps an untrusted score (quoted: `s.parse().unwrap()`)";
    let o = sb.ow(&[
        "record-finding",
        "--finding-id",
        "K-1",
        "--source",
        "continuous-audit",
        "--summary",
        "s",
        "--verdict",
        "confirmed",
        "--rationale",
        why,
    ]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(sb.finding("K-1")["rationale"], why);
    // CLI: omitting it still succeeds; stored rationale is empty/absent.
    let o = sb.ow(&[
        "record-finding",
        "--finding-id",
        "K-2",
        "--source",
        "continuous-audit",
        "--summary",
        "s",
        "--verdict",
        "confirmed",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(str_of(&sb.finding("K-2"), "rationale"), "");

    // Script: 5th field forwarded; 4-field legacy form still records.
    let r = sb.ca(&[
        "--round",
        "R1",
        "--new-findings",
        "2",
        "--confirmed",
        "2",
        "--regression-tests-added",
        "2",
        "--finding",
        "S-5|high|five|f.rs|the verifier's reason, with spaces",
        "--finding",
        "S-4|high|four|f.rs",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(
        sb.finding("S-5")["rationale"],
        "the verifier's reason, with spaces"
    );
    assert_eq!(str_of(&sb.finding("S-4"), "rationale"), "");
    assert_eq!(sb.finding("S-4")["file"], "f.rs");
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_finding_flags_repeat_and_format() {
    let sb = sandbox();
    let r = sb.ca(&[
        "--round",
        "R1",
        "--new-findings",
        "4",
        "--confirmed",
        "2",
        "--unverified",
        "2",
        "--regression-tests-added",
        "2",
        "--finding",
        "C-3|high|three fields",
        "--finding",
        "C-5|low|five fields|c.rs|c why",
        "--unverified-finding",
        "U-3|high|three fields",
        "--unverified-finding",
        "U-5|low|five fields|u.rs|u why",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    let rows: Vec<_> = sb
        .findings()
        .into_iter()
        .filter(|f| f["source"] == "continuous-audit")
        .collect();
    assert_eq!(rows.len(), 4, "{rows:?}");
    // Repeated flags: 2 confirmed + 2 unverified rows.
    let verdicts = |v: &str| rows.iter().filter(|f| f["verdict"] == v).count();
    assert_eq!(verdicts("confirmed"), 2);
    assert_eq!(verdicts("unverified"), 2);
    // Identical layout under both flags.
    for (id3, id5, sev5, file5, why5) in [
        ("C-3", "C-5", "low", "c.rs", "c why"),
        ("U-3", "U-5", "low", "u.rs", "u why"),
    ] {
        let f3 = sb.finding(id3);
        assert_eq!(f3["severity"], "high");
        assert_eq!(f3["summary"], "three fields");
        assert_eq!(str_of(&f3, "file"), "");
        assert_eq!(str_of(&f3, "rationale"), "");
        let f5 = sb.finding(id5);
        assert_eq!(f5["severity"], sev5);
        assert_eq!(f5["summary"], "five fields");
        assert_eq!(f5["file"], file5);
        assert_eq!(f5["rationale"], why5);
    }
    // severity may be omitted (empty field) and the entry still records.
    let r = sb.ca(&[
        "--round",
        "R2",
        "--finding",
        "C-0||no severity",
        "--unverified-finding",
        "U-0||no severity",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(sb.finding("C-0")["verdict"], "confirmed");
    assert_eq!(sb.finding("U-0")["verdict"], "unverified");
    assert_eq!(sb.finding("C-0")["summary"], "no severity");
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_script_never_writes_refuted() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("argv.log");
    let fake = tmp.path().join("overwatch");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\nprintf -- '---\\n' >> '{}'\nexit 0\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let mut verdicts_seen = vec![];
    for dry in [false, true] {
        let _ = fs::remove_file(&log);
        let mut args = vec!["--round", "R1"];
        if dry {
            args.push("--dry-run");
        }
        args.extend([
            "--finding",
            "C-1|high|REFUTED by verifier|f.rs|refuted: --verdict refuted",
            "--unverified-finding",
            "U-1|high|refuted?|g.rs|verdict refuted",
        ]);
        let o = Command::new("bash")
            .arg(script())
            .args(&args)
            .env("OVERWATCH_BIN", &fake)
            .env("HOME", &home)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        if dry {
            // Dry-run plan: the verdict token printed before the `<-` entry.
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            for l in stdout
                .lines()
                .filter(|l| l.contains("record-finding --verdict"))
            {
                let head = l.split("<-").next().unwrap();
                let tok = head.split("--verdict").nth(1).unwrap().trim().to_string();
                verdicts_seen.push(tok);
            }
            continue;
        }
        let argv = fs::read_to_string(&log).unwrap();
        let toks: Vec<&str> = argv.lines().collect();
        for (i, t) in toks.iter().enumerate() {
            if *t == "--verdict" {
                verdicts_seen.push(toks[i + 1].to_string());
            }
        }
    }
    // Anti-vacuity: both legal tokens were forwarded, and only those.
    assert!(verdicts_seen.contains(&"confirmed".to_string()));
    assert!(verdicts_seen.contains(&"unverified".to_string()));
    assert!(
        verdicts_seen
            .iter()
            .all(|v| v == "confirmed" || v == "unverified"),
        "{verdicts_seen:?}"
    );
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_unverified_count_flag() {
    let sb = sandbox();
    let r = sb.ca(&[
        "--round",
        "R9",
        "--new-findings",
        "4",
        "--confirmed",
        "3",
        "--unverified",
        "2",
        "--regression-tests-added",
        "3",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    let m = sb.metrics();
    let round = &m["rounds"][0];
    assert_eq!(round["round"], "R9");
    assert_eq!(round["new_findings"], 4);
    assert_eq!(round["confirmed"], 3);
    assert_eq!(round["unverified"], 2);
    assert_eq!(round["regression_tests_added"], 3);
    assert_eq!(m["cumulative_unverified"], 2);
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_script_forwards_model_flags() {
    let collides = |q: &[serde_json::Value], round: &str| {
        let id = format!("audit-round-model-collision-{round}");
        q.iter().any(|r| r["identifier"] == id.as_str())
    };
    let same = sandbox();
    let r = same.ca(&[
        "--round",
        "R1",
        "--finder-model",
        "opus",
        "--verifier-model",
        "opus",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    let q = same.queue();
    assert!(collides(&q, "R1"), "{q:?}");
    let row = q
        .iter()
        .find(|r| r["identifier"] == "audit-round-model-collision-R1")
        .unwrap();
    assert_eq!(row["severity"], "high");

    let diff = sandbox();
    let r = diff.ca(&[
        "--round",
        "R1",
        "--finder-model",
        "opus",
        "--verifier-model",
        "sonnet",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(!collides(&diff.queue(), "R1"));
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_finding_entries_independent_of_counts() {
    let ai_rows = |sb: &Sandbox| {
        sb.queue()
            .into_iter()
            .filter(|r| r["kind"] == "ai-finding")
            .count()
    };
    // counts > entries
    let sb = sandbox();
    let r = sb.ca(&[
        "--round",
        "R1",
        "--new-findings",
        "4",
        "--confirmed",
        "3",
        "--regression-tests-added",
        "3",
        "--finding",
        "C-1|high|only one|f.rs|why",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(ai_rows(&sb), 1);
    assert_eq!(sb.metrics()["rounds"][0]["confirmed"], 3);
    // entries > counts
    let sb = sandbox();
    let r = sb.ca(&[
        "--round",
        "R1",
        "--new-findings",
        "1",
        "--confirmed",
        "1",
        "--regression-tests-added",
        "1",
        "--finding",
        "C-1|high|a|f.rs|why",
        "--finding",
        "C-2|high|b|f.rs|why",
        "--finding",
        "C-3|high|c|f.rs|why",
    ]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(ai_rows(&sb), 3);
    assert_eq!(sb.metrics()["rounds"][0]["confirmed"], 1);
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_converging_ignores_closure_rate() {
    use overwatch::audit_round::{compute_metrics, AuditRound, DEFAULT_CONVERGENCE_WINDOW};
    let ledger = |news: &[u64], tests: u64| -> Vec<AuditRound> {
        news.iter()
            .enumerate()
            .map(|(i, n)| {
                AuditRound::new(
                    format!("R{i}"),
                    &["specguard".to_string()],
                    *n,
                    2,
                    tests,
                    i as i64,
                )
            })
            .collect()
    };
    for (news, want) in [(&[5u64, 3, 1][..], true), (&[1u64, 3, 5][..], false)] {
        let open = compute_metrics(&ledger(news, 0), DEFAULT_CONVERGENCE_WINDOW);
        let closed = compute_metrics(&ledger(news, 2), DEFAULT_CONVERGENCE_WINDOW);
        // The two ledgers really differ in closure_rate...
        assert_eq!(open.closure_rate, Some(0.0));
        assert_eq!(closed.closure_rate, Some(1.0));
        // ...and `converging` is identical, driven by new_findings alone.
        assert_eq!(open.converging, Some(want), "{news:?}");
        assert_eq!(closed.converging, Some(want), "{news:?}");
    }
}

// ---------------------------------------------------------------------------

#[test]
fn claim_ca_automation_templates_exist() {
    let repo = repo_root();
    let cron = fs::read_to_string(repo.join("scripts/continuous-audit.cron.example")).unwrap();
    let sh = fs::read_to_string(script()).unwrap();
    let header = sh.split("set -uo pipefail").next().unwrap();

    for (name, body) in [
        ("cron.example", &cron),
        ("script header", &header.to_string()),
    ] {
        // weekly crontab line (Mondays 04:00) invoking the script with --round
        assert!(
            body.lines()
                .any(|l| l.contains("0 4 * * 1")
                    && l.contains("scripts/continuous-audit.sh --round")),
            "{name}: no weekly crontab line"
        );
        // a pre-push fragment
        assert!(body.contains(".git/hooks/pre-push"), "{name}: no pre-push");
    }

    // The pre-push fragment in the example is advisory: actually run it.
    let lines: Vec<&str> = cron.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("#   #!/usr/bin/env bash"))
        .expect("pre-push fragment start");
    let end = start
        + lines[start..]
            .iter()
            .position(|l| l.trim() == "#   exit 0")
            .expect("pre-push fragment ends in exit 0");
    let frag: String = lines[start..=end]
        .iter()
        .map(|l| l.strip_prefix("#   ").unwrap_or(l).to_string() + "\n")
        .collect();
    assert!(!frag.contains("exit 1"));
    let tmp = tempfile::tempdir().unwrap();
    let hook = tmp.path().join("pre-push");
    fs::write(&hook, &frag).unwrap();
    // Run inside a fresh git repo with a gate-crate change after a base commit
    // and no upstream: the hook must still exit 0 (never blocks the push).
    let git = |a: &[&str]| {
        Command::new("git")
            .args(a)
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    fs::create_dir_all(tmp.path().join("crates/specguard")).unwrap();
    fs::write(tmp.path().join("crates/specguard/x"), "x").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-qm", "c"]);
    let o = Command::new("bash")
        .arg(&hook)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
}
