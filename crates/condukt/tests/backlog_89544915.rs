// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Backlog 89544915 — condukt-gate findings (`gate-exec:<run>:<task>`) are
// closed by OBSERVING the run state, never by commit-message self-report.
// Independent tests (author != implementer).
//
// CONTRACT ASSUMED — the implementer MUST match these exactly:
//
// CLI:  `condukt gate reconcile-findings [--json]`
//   * cwd = the project repo (finding store + run-state resolved from cwd/HOME
//   exactly like `condukt gate check`).
//   * For every open review finding whose id starts with `gate-exec:`
//   (id = `gate-exec:<run_id>:<task_id>`):
//   - run state loads, task present, status in {done, verified, cancelled,
//   discarded}  => append a disposition (same ledger `overwatch
//   record-disposition` writes) with verdict `resolved`, `reviewer`
//   non-empty, `evidence` (string) naming run id, task id and the
//   observed status, and non-empty `observed_source`;
//   - status pending / running / failed => still present: NOT closed, and
//   NOT undetermined (exit 0);
//   - run-state file corrupt/unreadable, or task id absent from a loaded
//   run => UNDETERMINED: NOT closed;
//   - run-state file ABSENT => UNDETERMINED until the absence has been
//   observed for >= 30 days (30*86400 s), then closed `resolved` with
//   evidence containing the text `absent since` and the first-absence
//   timestamp as a DECIMAL unix-epoch integer. The first-absence time is
//   recorded by the FIRST reconcile run that observes the absence (not
//   derived from the finding's ts or any file mtime). Corrupt state is
//   never aged out.
//   * Exit code: 0 when nothing is undetermined (closing zero or more);
//   3 when ANY finding is undetermined (other findings are still processed).
//   * stderr, per undetermined finding: a line containing `NOT closed` and the
//   finding id and a reason.
//   * `--json` stdout: `{"resolved": ["<finding_id>", ...],
//   "undetermined": [{"finding_id": "..", "why": ".."}]}`
//   (both keys always present; `why` non-empty).
//   * Non-`gate-exec:` findings are never touched.
//
// Clock seam (no existing now-override exists in condukt/harness-core):
//   env `CONDUKT_NOW_EPOCH=<unix seconds>` makes `gate reconcile-findings`
//   (including its first-absence stamp and the 30-day comparison) use that
//   instant as "now". Unset => the real clock.
//
// SessionStart (R5): `condukt restore` (stdin `{"cwd":"<repo>"}`) must also run
// the reconcile. Always exits 0 (observability hook); the effect is the
// disposition row.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DAY: i64 = 86_400;
const T0: i64 = 1_800_000_000;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fx {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-89544915-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        Self {
            repo,
            home,
            state_dir,
        }
    }

    fn run(&self, args: &[&str], now: Option<i64>) -> Output {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_AUTONOMOUS", "0")
            .env_remove("CONDUKT_NOW_EPOCH");
        if let Some(n) = now {
            c.env("CONDUKT_NOW_EPOCH", n.to_string());
        }
        c.output().expect("spawn condukt")
    }

    /// `state init` a run with tasks t1..tN; returns the run id.
    fn init_run(&self, n: usize) -> String {
        let tasks: Vec<String> = (1..=n)
            .map(|i| {
                format!(
                    r#"{{"id":"t{i}","title":"rename helper {i}","touched_files":["src/f{i}.rs"],"deps":[],"class":"serial","done_criteria":"done {i}"}}"#
                )
            })
            .collect();
        let json = format!(r#"{{"goal":"g","tasks":[{}]}}"#, tasks.join(","));
        let p = self.repo.join("decomp.json");
        std::fs::write(&p, json).unwrap();
        let out = self.run(&["state", "init", "--file", p.to_str().unwrap()], None);
        assert!(out.status.success(), "init failed: {out:?}");
        let s = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        s.lines()
            .map(str::trim)
            .find(|l| l.starts_with("run-"))
            .expect("run id")
            .to_string()
    }

    fn set_status(&self, run: &str, task: &str, status: &str) {
        let out = self.run(
            &[
                "state", "set", "--run", run, "--task", task, "--status", status,
            ],
            None,
        );
        assert!(out.status.success(), "state set {task}={status}: {out:?}");
    }

    fn run_file(&self, run: &str) -> PathBuf {
        find_by_suffix(&self.state_dir, &format!("{run}.json")).expect("run-state file")
    }

    fn findings_path(&self) -> PathBuf {
        let root = harness_core::projkey::repo_root(&self.repo);
        let key = harness_core::projkey::project_key(&root);
        self.home
            .join(".overwatch")
            .join(key)
            .join("overwatch")
            .join("review_findings.jsonl")
    }

    /// Seed an open condukt-gate finding exactly as `gate check` would.
    fn seed_finding(&self, id: &str) {
        let f = overwatch::review_finding::ReviewFinding::new(
            id.to_string(),
            "condukt-gate".to_string(),
            Some("medium".to_string()),
            format!("gate-check escalated: {id}"),
            None,
            None,
            1_700_000_000,
        );
        let p = self.findings_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut txt = std::fs::read_to_string(&p).unwrap_or_default();
        txt.push_str(&serde_json::to_string(&f).unwrap());
        txt.push('\n');
        std::fs::write(&p, txt).unwrap();
    }

    fn dispositions(&self) -> Vec<Value> {
        let p = self.findings_path().with_file_name("dispositions.jsonl");
        match std::fs::read_to_string(p) {
            Ok(t) => t
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn disposition_of(&self, id: &str) -> Option<Value> {
        self.dispositions()
            .into_iter()
            .find(|d| d["finding_id"] == id)
    }

    fn reconcile(&self, now: Option<i64>) -> (Output, Value) {
        let out = self.run(&["gate", "reconcile-findings", "--json"], now);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("`condukt gate reconcile-findings --json` must print JSON ({e}); out={out:?}")
        });
        (out, v)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn find_by_suffix(root: &Path, suffix: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_by_suffix(&p, suffix) {
                return Some(f);
            }
        } else if p.to_string_lossy().ends_with(suffix) {
            return Some(p);
        }
    }
    None
}

fn resolved_ids(v: &Value) -> Vec<String> {
    v["resolved"]
        .as_array()
        .expect("`resolved` array")
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn undet_ids(v: &Value) -> Vec<String> {
    v["undetermined"]
        .as_array()
        .expect("`undetermined` array")
        .iter()
        .map(|x| x["finding_id"].as_str().unwrap().to_string())
        .collect()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

// --------------------------------------------------------------------- T1

/// T1 (RED): every terminal status closes with verdict `resolved` + evidence
/// naming run/task/status. Pending and failed tasks of the SAME run stay open.
#[test]
fn t1_terminal_tasks_close_resolved_with_evidence() {
    let fx = Fx::new("t1");
    let rid = fx.init_run(6);
    for (t, s) in [
        ("t1", "done"),
        ("t2", "verified"),
        ("t3", "cancelled"),
        ("t4", "discarded"),
        ("t6", "failed"),
    ] {
        fx.set_status(&rid, t, s);
    }
    // t5 stays pending.
    for i in 1..=6 {
        fx.seed_finding(&format!("gate-exec:{rid}:t{i}"));
    }
    let (out, v) = fx.reconcile(None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "pending/failed are still-present, not undetermined: {out:?}"
    );
    let mut got = resolved_ids(&v);
    got.sort();
    let want: Vec<String> = (1..=4).map(|i| format!("gate-exec:{rid}:t{i}")).collect();
    assert_eq!(got, want, "{v}");
    for (i, status) in [
        (1, "done"),
        (2, "verified"),
        (3, "cancelled"),
        (4, "discarded"),
    ] {
        let id = format!("gate-exec:{rid}:t{i}");
        let d = fx
            .disposition_of(&id)
            .unwrap_or_else(|| panic!("no row {id}"));
        assert_eq!(d["verdict"], "resolved", "{d}");
        assert!(!d["reviewer"].as_str().unwrap_or("").is_empty(), "{d}");
        assert!(
            !d["observed_source"].as_str().unwrap_or("").is_empty(),
            "{d}"
        );
        let ev = d["evidence"].as_str().expect("evidence string");
        assert!(
            ev.contains(&rid) && ev.contains(&format!("t{i}")) && ev.contains(status),
            "evidence must name run, task and status: {ev}"
        );
    }
    assert!(fx.disposition_of(&format!("gate-exec:{rid}:t5")).is_none());
    assert!(fx.disposition_of(&format!("gate-exec:{rid}:t6")).is_none());
}

// --------------------------------------------------------------------- T2

/// T2: pending / running / failed stay open, exit 0, nothing written. (On
/// current code it fails only because the subcommand does not exist; it is the
/// "stays open" half of T1 and must keep holding once implemented.)
#[test]
fn t2_pending_and_failed_stay_open() {
    let fx = Fx::new("t2");
    let rid = fx.init_run(3);
    fx.set_status(&rid, "t2", "failed");
    fx.set_status(&rid, "t3", "running");
    for i in 1..=3 {
        fx.seed_finding(&format!("gate-exec:{rid}:t{i}"));
    }
    let (out, v) = fx.reconcile(None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(resolved_ids(&v), Vec::<String>::new(), "{v}");
    assert_eq!(undet_ids(&v), Vec::<String>::new(), "{v}");
    assert!(fx.dispositions().is_empty(), "{:?}", fx.dispositions());
}

// --------------------------------------------------------------------- T3

/// T3: corrupt run state => undetermined, exit 3, reason visible, open.
#[test]
fn t3_corrupt_run_state_is_undetermined() {
    let fx = Fx::new("t3c");
    let rid = fx.init_run(1);
    fx.set_status(&rid, "t1", "done");
    std::fs::write(fx.run_file(&rid), "{ this is not json").unwrap();
    let id = format!("gate-exec:{rid}:t1");
    fx.seed_finding(&id);
    let (out, v) = fx.reconcile(None);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        stderr(&out).contains("NOT closed") && stderr(&out).contains(&id),
        "stderr must say `NOT closed` and name the finding: {}",
        stderr(&out)
    );
    assert_eq!(undet_ids(&v), vec![id.clone()], "{v}");
    assert!(!v["undetermined"][0]["why"].as_str().unwrap().is_empty());
    assert!(resolved_ids(&v).is_empty());
    assert!(fx.disposition_of(&id).is_none());
}

/// T3: the finding names a task id that the (loadable) run does not contain.
/// Control in the same run: a done task IS closed (the command works).
#[test]
fn t3_task_id_missing_from_run_is_undetermined() {
    let fx = Fx::new("t3m");
    let rid = fx.init_run(2);
    fx.set_status(&rid, "t1", "done");
    let ok = format!("gate-exec:{rid}:t1");
    let missing = format!("gate-exec:{rid}:t99");
    fx.seed_finding(&ok);
    fx.seed_finding(&missing);
    let (out, v) = fx.reconcile(None);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert_eq!(resolved_ids(&v), vec![ok.clone()], "control closes: {v}");
    assert_eq!(undet_ids(&v), vec![missing.clone()], "{v}");
    assert!(stderr(&out).contains("NOT closed") && stderr(&out).contains(&missing));
    assert!(fx.disposition_of(&missing).is_none());
    assert!(fx.disposition_of(&ok).is_some());
}

/// T3: run state ABSENT, first observation => undetermined (open, exit 3), even
/// when "now" is far in the future relative to the finding's own timestamp
/// (the age must be measured from the first OBSERVED absence, not guessed).
#[test]
fn t3_absent_run_state_first_observation_is_undetermined_even_far_in_future() {
    let fx = Fx::new("t3a");
    let id = "gate-exec:run-gone-1:t1";
    fx.seed_finding(id);
    // finding ts is 1_700_000_000; "now" is 100 days after T0. Must NOT close.
    let (out, v) = fx.reconcile(Some(T0 + 100 * DAY));
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert_eq!(undet_ids(&v), vec![id.to_string()], "{v}");
    assert!(stderr(&out).contains("NOT closed") && stderr(&out).contains(id));
    assert!(fx.disposition_of(id).is_none());
}

// -------------------------------------------------------------------- T3b

/// T3b: absence observed at T0; 30 days minus 1 second later still open; at
/// exactly 30 days closes `resolved` with `absent since <T0>` evidence.
#[test]
fn t3b_absent_state_closes_only_after_30_days_observed() {
    let fx = Fx::new("t3b");
    let id = "gate-exec:run-gone-2:t1";
    fx.seed_finding(id);

    let (o1, _) = fx.reconcile(Some(T0));
    assert_eq!(o1.status.code(), Some(3), "first sighting: {o1:?}");
    assert!(fx.disposition_of(id).is_none());

    let (o2, v2) = fx.reconcile(Some(T0 + 30 * DAY - 1));
    assert_eq!(o2.status.code(), Some(3), "29d23h59m59s: {o2:?}");
    assert_eq!(undet_ids(&v2), vec![id.to_string()], "{v2}");
    assert!(fx.disposition_of(id).is_none());

    let (o3, v3) = fx.reconcile(Some(T0 + 30 * DAY));
    assert_eq!(o3.status.code(), Some(0), "30d reached: {o3:?}");
    assert_eq!(resolved_ids(&v3), vec![id.to_string()], "{v3}");
    let d = fx.disposition_of(id).expect("closed row");
    assert_eq!(d["verdict"], "resolved", "{d}");
    let ev = d["evidence"].as_str().expect("evidence string");
    assert!(
        ev.contains("absent since") && ev.contains(&T0.to_string()),
        "evidence must record `absent since <first-absence epoch {T0}>`: {ev}"
    );
    assert!(!d["observed_source"].as_str().unwrap_or("").is_empty());

    // Idempotent: a further run adds no second row.
    let (o4, _) = fx.reconcile(Some(T0 + 31 * DAY));
    assert_eq!(o4.status.code(), Some(0), "{o4:?}");
    assert_eq!(fx.dispositions().len(), 1);
}

/// T3b: corrupt state is NEVER aged out, however long it is observed.
#[test]
fn t3b_corrupt_state_never_ages_out() {
    let fx = Fx::new("t3bc");
    let rid = fx.init_run(1);
    fx.set_status(&rid, "t1", "done");
    std::fs::write(fx.run_file(&rid), "\0\0 garbage").unwrap();
    let id = format!("gate-exec:{rid}:t1");
    fx.seed_finding(&id);
    for dt in [0, 31 * DAY, 400 * DAY] {
        let (o, v) = fx.reconcile(Some(T0 + dt));
        assert_eq!(o.status.code(), Some(3), "dt={dt}: {o:?}");
        assert_eq!(undet_ids(&v), vec![id.clone()], "{v}");
        assert!(fx.disposition_of(&id).is_none(), "dt={dt}");
    }
}

/// T3b (control): an old absence does not close a finding whose run state has
/// REAPPEARED as non-terminal — it is judged on what is observed now.
#[test]
fn t3b_reappeared_pending_state_is_not_closed_by_old_absence() {
    let fx = Fx::new("t3br");
    let rid = fx.init_run(1);
    let id = format!("gate-exec:{rid}:t1");
    fx.seed_finding(&id);
    let path = fx.run_file(&rid);
    let backup = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let (o1, _) = fx.reconcile(Some(T0));
    assert_eq!(o1.status.code(), Some(3), "{o1:?}");
    std::fs::write(&path, backup).unwrap(); // task t1 pending
    let (o2, v2) = fx.reconcile(Some(T0 + 60 * DAY));
    assert_eq!(o2.status.code(), Some(0), "{o2:?}");
    assert!(resolved_ids(&v2).is_empty(), "{v2}");
    assert!(fx.disposition_of(&id).is_none());
}

// ------------------------------------------------------------ scope / hooks

/// Only `gate-exec:` findings are touched: a record-audit / specguard / CA
/// finding is left alone even though it is in the same store.
#[test]
fn only_gate_exec_findings_are_touched() {
    let fx = Fx::new("scope");
    let rid = fx.init_run(1);
    fx.set_status(&rid, "t1", "done");
    fx.seed_finding(&format!("gate-exec:{rid}:t1"));
    for other in [
        "record-audit:freshness:1000",
        "specguard:untested:foo",
        "CA-condukt-1",
    ] {
        fx.seed_finding(other);
    }
    let (out, v) = fx.reconcile(None);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(resolved_ids(&v), vec![format!("gate-exec:{rid}:t1")], "{v}");
    assert_eq!(fx.dispositions().len(), 1, "{:?}", fx.dispositions());
}

/// R5 (RED): the SessionStart `condukt restore` path runs the reconcile.
#[test]
fn restore_hook_runs_the_reconcile() {
    use std::io::Write;
    use std::process::Stdio;
    let fx = Fx::new("restore");
    let rid = fx.init_run(1);
    fx.set_status(&rid, "t1", "done");
    let id = format!("gate-exec:{rid}:t1");
    fx.seed_finding(&id);
    let mut child = Command::new(bin())
        .arg("restore")
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        .env_remove("CONDUKT_NOW_EPOCH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = format!(
        r#"{{"cwd":{}}}"#,
        serde_json::json!(fx.repo.to_str().unwrap())
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "hook must exit 0: {out:?}");
    let d = fx
        .disposition_of(&id)
        .expect("`condukt restore` must auto-close the terminal-task finding");
    assert_eq!(d["verdict"], "resolved", "{d}");
}
