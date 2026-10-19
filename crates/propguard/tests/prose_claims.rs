// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests: one `claim_<id>` per README prose claim, each
//! driving the real `propguard` binary (temp HOME == temp git repo) and
//! asserting the OBSERVED behaviour the sentence states.
//!
//! Isolation: every test owns a `tempfile::TempDir` used as HOME *and* cwd;
//! `PROPGUARD_STATE_DIR` is pinned under it unless the test is about that very
//! variable. Nothing here touches the real `~/.propguard`, `~/.harness` or
//! `~/.overwatch`, and no process-global env var is mutated.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

// ───────────────────────── fixture / process helpers ─────────────────────────

struct Fx {
    _t: tempfile::TempDir,
    dir: PathBuf,
}

impl Fx {
    /// Empty directory (no git repo).
    fn bare() -> Fx {
        let t = tempfile::tempdir().expect("tempdir");
        let dir = t.path().canonicalize().expect("canonicalize");
        Fx { _t: t, dir }
    }
    /// Git repo with one changed (untracked) source file `a.rs`.
    fn repo() -> Fx {
        let fx = Fx::bare();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            let o = Command::new("git")
                .current_dir(&fx.dir)
                .args(&args)
                .output()
                .expect("git runs");
            assert!(o.status.success(), "git {args:?} failed");
        }
        fx.set_source("fn f() { panic!() }\n");
        fx
    }
    fn set_source(&self, body: &str) {
        std::fs::write(self.dir.join("a.rs"), body).expect("write a.rs");
    }
    fn state(&self) -> PathBuf {
        self.dir.join(".pg-state")
    }
    fn write_home_config(&self, body: &str) {
        let d = self.dir.join(".propguard");
        std::fs::create_dir_all(&d).expect("mkdir .propguard");
        std::fs::write(d.join("config.toml"), body).expect("write home config");
    }
    fn write_project_config(&self, body: &str) {
        std::fs::write(self.dir.join("propguard.toml"), body).expect("write propguard.toml");
    }
    /// Seed one session's gate state (the file `harness_core::gate::state` reads).
    fn seed_state(&self, session: &str, attempts: u32, age_secs: i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let d = self.state().join("sessions");
        std::fs::create_dir_all(&d).expect("mkdir sessions");
        let body = serde_json::json!({
            "attempts": attempts,
            "last_hash": "",
            "last_ts": now - age_secs,
        });
        std::fs::write(d.join(format!("{session}.json")), body.to_string()).expect("seed state");
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    /// True iff stdout carries a `{"decision":"block"}` object.
    fn blocked(&self) -> bool {
        let mut blocked = false;
        for l in self.stdout.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(l.trim())
                .unwrap_or_else(|e| panic!("unparseable hook stdout line {l:?}: {e}"));
            if v["decision"] == "block" {
                blocked = true;
            }
        }
        if !blocked {
            // An allow is positively observed: exit 0 and only non-block JSON
            // (the binary prints nothing on allow, or a systemMessage waiver).
            assert_eq!(
                self.code, 0,
                "non-block outcome must be a clean exit 0: stdout={:?} stderr={:?}",
                self.stdout, self.stderr
            );
        }
        blocked
    }
    fn reason(&self) -> String {
        self.stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
            .find(|v| v["decision"] == "block")
            .and_then(|v| v["reason"].as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

fn run_opts(fx: &Fx, args: &[&str], stdin: &str, envs: &[(&str, &str)], pin_state: bool) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_propguard"));
    cmd.args(args)
        .current_dir(&fx.dir)
        .env("HOME", &fx.dir)
        .env_remove("PROPGUARD_STATE_DIR")
        .env_remove("PROPGUARD_CRITERIA")
        .env_remove("PROPGUARD_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    if pin_state {
        cmd.env("PROPGUARD_STATE_DIR", fx.state());
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("propguard spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("propguard runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run(fx: &Fx, args: &[&str], envs: &[(&str, &str)]) -> Out {
    run_opts(fx, args, "", envs, true)
}

fn payload(fx: &Fx, session: &str, stop_hook_active: bool) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": stop_hook_active,
        "cwd": fx.dir.to_string_lossy(),
    })
    .to_string()
}

/// One Stop-hook invocation.
fn stop(fx: &Fx, session: &str, envs: &[(&str, &str)]) -> Out {
    run_opts(fx, &["check"], &payload(fx, session, false), envs, true)
}

fn trust(fx: &Fx) {
    let o = run(fx, &["trust"], &[]);
    assert_eq!(o.code, 0, "trust failed: {} {}", o.stdout, o.stderr);
}

/// Every `log.jsonl` entry of the pinned state dir.
fn log_entries(state: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(state.join("log.jsonl"))
        .unwrap_or_else(|e| panic!("gate log.jsonl missing/unreadable: {e}"));
    let entries: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad log line {l:?}: {e}")))
        .collect();
    assert!(!entries.is_empty(), "gate log.jsonl is empty");
    entries
}

/// The gate verdict tags (`verdict` key) in log order.
fn verdicts(state: &Path) -> Vec<String> {
    log_entries(state)
        .iter()
        .filter_map(|e| e["verdict"].as_str().map(str::to_string))
        .collect()
}

fn last_verdict(state: &Path) -> String {
    verdicts(state)
        .pop()
        .expect("gate log holds no verdict entry")
}

const IDEM: &str = "must be idempotent";

/// `attempts` stored in a session's state file (panics if absent/unparseable).
fn saved_attempts(state: &Path, session: &str) -> u64 {
    let p = state.join("sessions").join(format!("{session}.json"));
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("state {p:?}: {e}"));
    let v: Value = serde_json::from_str(&text).expect("state JSON");
    v["attempts"].as_u64().expect("attempts number")
}

// ───────────────────────────── 1. one_stop_hook ─────────────────────────────

#[test]
fn claim_one_stop_hook() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let text = std::fs::read_to_string(&path).expect("hooks.json readable");
    let v: Value = serde_json::from_str(&text).expect("hooks.json is JSON");
    let hooks = v["hooks"].as_object().expect("hooks is an object");
    let events: Vec<&String> = hooks.keys().collect();
    assert_eq!(events, vec!["Stop"], "exactly one event, Stop: {events:?}");
    let groups = hooks["Stop"].as_array().expect("Stop is an array");
    assert_eq!(groups.len(), 1, "exactly one Stop group");
    let cmds = groups[0]["hooks"].as_array().expect("group.hooks array");
    assert_eq!(cmds.len(), 1, "exactly one hook in the group");
    assert_eq!(cmds[0]["type"], "command");
    assert_eq!(
        cmds[0]["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/propguard check"
    );
}

// ─────────────────────────────── 2. catalog ───────────────────────────────

/// (id, title) pairs exactly as `CATALOG` defines them.
const CATALOG_EXPECTED: [(&str, &str); 6] = [
    (
        "error-path",
        "失敗パスは panic せず Err/エラーを返す (error handling)",
    ),
    (
        "output-schema",
        "出力スキーマ/フォーマットが安定している (schema stability)",
    ),
    (
        "determinism",
        "決定論的: 同一入力は同一出力を返す (determinism)",
    ),
    (
        "idempotence",
        "冪等: 複数回実行しても結果が変わらない (idempotence)",
    ),
    (
        "bounds-monotonicity",
        "境界・単調性・閾値が守られる (bounds/monotonicity)",
    ),
    (
        "no-partial-write",
        "部分書き込みが起きない (atomic / no-partial-write)",
    ),
];

/// Parse `propguard derive` property lines: `  N. [id] title`.
fn parse_derived(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (num, rest) = l.split_once(". ")?;
            if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let rest = rest.strip_prefix('[')?;
            let (id, title) = rest.split_once("] ")?;
            Some((id.to_string(), title.trim().to_string()))
        })
        .collect()
}

/// README table rows: (id, title cell).
fn readme_rows(name: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name))
        .expect("README readable");
    text.lines()
        .filter(|l| l.starts_with("| `"))
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').collect();
            if cells.len() < 4 {
                return None;
            }
            let id = cells[1].trim().trim_matches('`').to_string();
            if !CATALOG_EXPECTED.iter().any(|(i, _)| *i == id) {
                return None;
            }
            let cell = cells[2].trim();
            let cell = cell.split(" [test:").next().unwrap_or(cell).trim();
            Some((id, cell.to_string()))
        })
        .collect()
}

#[test]
fn claim_catalog() {
    let fx = Fx::bare();
    // Union over several derive calls (derive caps at 5, so one call can never
    // show all six).
    let mut seen: std::collections::BTreeMap<String, String> = Default::default();
    for crit in [
        "idempotent",
        "sort order",
        "atomic save",
        "deterministic",
        "zzz qqq",
    ] {
        let o = run(&fx, &["derive", crit], &[]);
        assert_eq!(o.code, 0, "derive {crit}: {}", o.stderr);
        for (id, title) in parse_derived(&o.stdout) {
            if let Some(prev) = seen.insert(id.clone(), title.clone()) {
                assert_eq!(prev, title, "title of {id} unstable across calls");
            }
        }
    }
    let got: Vec<(String, String)> = CATALOG_EXPECTED
        .iter()
        .map(|(i, _)| (i.to_string(), seen.get(*i).cloned().unwrap_or_default()))
        .collect();
    let want: Vec<(String, String)> = CATALOG_EXPECTED
        .iter()
        .map(|(i, t)| (i.to_string(), t.to_string()))
        .collect();
    assert_eq!(got, want, "binary-observed catalog ids/titles");
    assert_eq!(
        seen.len(),
        6,
        "no property outside the six: {:?}",
        seen.keys()
    );

    // Exactly six catalog entries exist in the source (a seventh would be
    // invisible to the keyword probes above).
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/derive.rs"))
        .expect("derive.rs readable");
    let ids_in_src: Vec<&str> = src
        .lines()
        .filter_map(|l| l.trim().strip_prefix("id: \""))
        .filter_map(|r| r.split('"').next())
        .collect();
    let want_ids: Vec<&str> = CATALOG_EXPECTED.iter().map(|(i, _)| *i).collect();
    assert_eq!(ids_in_src, want_ids, "CATALOG ids in source");

    // Baseline: no keywords -> exactly the three universal properties, in order.
    let o = run(&fx, &["derive", "zzz qqq"], &[]);
    let ids: Vec<String> = parse_derived(&o.stdout).into_iter().map(|p| p.0).collect();
    assert_eq!(ids, vec!["error-path", "output-schema", "determinism"]);

    // README tables must show the catalog's ids and titles.
    let mut mismatches = Vec::new();
    for readme in ["README.md", "README.ja.md"] {
        let rows = readme_rows(readme);
        let row_ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(row_ids, want_ids, "{readme} table ids/order");
        for ((id, cell), (_, title)) in rows.iter().zip(CATALOG_EXPECTED.iter()) {
            // README.ja drops the trailing English gloss; compare the Japanese
            // part (everything before the final " (") for it.
            let want_cell = if readme == "README.md" {
                title.to_string()
            } else {
                title
                    .rsplit_once(" (")
                    .map(|x| x.0)
                    .unwrap_or(title)
                    .to_string()
            };
            if *cell != want_cell {
                mismatches.push(format!(
                    "{readme} {id}: table {cell:?} != catalog {want_cell:?}"
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "README rows differ from CATALOG: {mismatches:#?}"
    );
}

// ────────────────────────── 3. criteria_file_default ──────────────────────────

#[test]
fn claim_criteria_file_default() {
    // (a) file alone (no env, no config) drives the block.
    let a = Fx::repo();
    std::fs::write(a.dir.join(".propguard-criteria"), IDEM).unwrap();
    let o = stop(&a, "cf-a", &[]);
    assert!(
        o.blocked(),
        "file alone must block: {} / {}",
        o.stdout,
        o.stderr
    );
    assert!(
        o.reason().contains("[idempotence]"),
        "reason: {}",
        o.reason()
    );
    assert!(!o.reason().contains("[bounds-monotonicity]"));

    // (b) env beats the file.
    let b = Fx::repo();
    std::fs::write(b.dir.join(".propguard-criteria"), IDEM).unwrap();
    let o = stop(&b, "cf-b", &[("PROPGUARD_CRITERIA", "sort order")]);
    assert!(o.blocked(), "{}", o.stdout);
    assert!(
        o.reason().contains("[bounds-monotonicity]"),
        "env text must win: {}",
        o.reason()
    );
    assert!(
        !o.reason().contains("[idempotence]"),
        "file text leaked past env: {}",
        o.reason()
    );

    // (c) file beats inline done_criteria in a trusted propguard.toml.
    let c = Fx::repo();
    c.write_project_config("done_criteria = \"atomic save\"\n");
    trust(&c);
    // control: inline alone is honoured (so its absence below is meaningful)
    let o = stop(&c, "cf-c0", &[]);
    assert!(
        o.reason().contains("[no-partial-write]"),
        "inline control: {}",
        o.reason()
    );
    std::fs::write(c.dir.join(".propguard-criteria"), IDEM).unwrap();
    let o = stop(&c, "cf-c1", &[]);
    assert!(o.blocked(), "{}", o.stdout);
    assert!(
        o.reason().contains("[idempotence]"),
        "file must win over inline: {}",
        o.reason()
    );
    assert!(
        !o.reason().contains("[no-partial-write]"),
        "inline leaked past file: {}",
        o.reason()
    );
}

// ──────────────────────────── 4. default_mode_inject ────────────────────────────

#[test]
fn claim_default_mode_inject() {
    let fx = Fx::repo();
    // A checker_cmd that would leave a marker if (wrongly) executed, with NO
    // `mode` key anywhere.
    let marker = fx.dir.join("checker-ran.marker");
    fx.write_home_config(&format!("checker_cmd = \"touch {}\"\n", marker.display()));
    let o = stop(&fx, "dm", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert_eq!(o.code, 0);
    assert!(o.blocked(), "{} / {}", o.stdout, o.stderr);
    assert!(
        o.reason().contains("未検証"),
        "inject checklist reason: {}",
        o.reason()
    );
    assert!(
        !marker.exists(),
        "inject mode must not run a checker subprocess"
    );
    let entry = log_entries(&fx.state()).pop().expect("a log entry");
    assert_eq!(entry["mode"], "inject");
    assert_eq!(entry["verdict"], "below-threshold");
    let st = run(&fx, &["status"], &[]);
    assert!(
        st.stdout.contains("mode:           inject"),
        "status: {}",
        st.stdout
    );
}

// ────────────────────────── 5/6. hash + already_verified ──────────────────────────

#[test]
fn claim_already_verified() {
    let fx = Fx::repo();
    let env = [("PROPGUARD_CRITERIA", IDEM)];
    let s1 = stop(&fx, "av", &env);
    assert!(s1.blocked(), "first stop must block: {}", s1.stdout);
    // Same (diff, properties): recorded hash -> allowed as already-verified.
    let s2 = stop(&fx, "av", &env);
    assert!(!s2.blocked(), "same hash must be allowed: {}", s2.stdout);
    assert_eq!(last_verdict(&fx.state()), "already-verified");
    // A different diff is not already-verified.
    fx.set_source("fn g() { unreachable!() }\n");
    let s3 = stop(&fx, "av", &env);
    assert!(s3.blocked(), "changed diff must block again: {}", s3.stdout);
    assert_eq!(last_verdict(&fx.state()), "below-threshold");
}

#[test]
fn claim_hash_diff_and_props() {
    // Changing ONLY the criteria (same diff) re-blocks.
    let a = Fx::repo();
    let s1 = stop(&a, "hp", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert!(s1.blocked());
    let s2 = stop(&a, "hp", &[("PROPGUARD_CRITERIA", "sort order")]);
    assert!(
        s2.blocked(),
        "changed property set must block again: {}",
        s2.stdout
    );
    assert_eq!(last_verdict(&a.state()), "below-threshold");
    assert!(s2.reason().contains("[bounds-monotonicity]"));

    // Control: identical criteria + diff would NOT block (so the above is the
    // properties changing the hash, not the gate always blocking).
    let b = Fx::repo();
    assert!(stop(&b, "hp", &[("PROPGUARD_CRITERIA", IDEM)]).blocked());
    assert!(!stop(&b, "hp", &[("PROPGUARD_CRITERIA", IDEM)]).blocked());

    // Changing ONLY the diff re-blocks.
    let c = Fx::repo();
    assert!(stop(&c, "hp", &[("PROPGUARD_CRITERIA", IDEM)]).blocked());
    c.set_source("fn h() { todo!() }\n");
    let s = stop(&c, "hp", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert!(s.blocked(), "changed diff must block again: {}", s.stdout);
    assert_eq!(last_verdict(&c.state()), "below-threshold");

    // Changing the criteria TEXT while the derived property SET is identical
    // (same diff) keeps the hash: the next stop is already-verified. Kills a
    // mutant that hashes the criteria text instead of the property ids.
    let alt = "idempotent please";
    let same_set = |text: &str| -> Vec<String> {
        parse_derived(&run(&Fx::bare(), &["derive", text], &[]).stdout)
            .into_iter()
            .map(|p| p.0)
            .collect()
    };
    assert_eq!(
        same_set(IDEM),
        same_set(alt),
        "precondition: same property set"
    );
    let d = Fx::repo();
    assert!(stop(&d, "hp", &[("PROPGUARD_CRITERIA", IDEM)]).blocked());
    let s = stop(&d, "hp", &[("PROPGUARD_CRITERIA", alt)]);
    assert!(
        !s.blocked(),
        "same property set + same diff must stay already-verified: {}",
        s.stdout
    );
    assert_eq!(last_verdict(&d.state()), "already-verified");
}

// ─────────────────────────── 7. max_attempts_defaults ───────────────────────────

#[test]
fn claim_max_attempts_defaults() {
    let env = [("PROPGUARD_CRITERIA", IDEM)];
    // status shows max_attempts 2 by default.
    let st = run(&Fx::bare(), &["status"], &[]);
    assert!(
        st.stdout.contains("max_attempts:   2"),
        "status: {}",
        st.stdout
    );

    // Counter increments per block (attempt 1, then 2).
    let fx = Fx::repo();
    let attempt = |fx: &Fx| -> u64 {
        log_entries(&fx.state()).pop().expect("log entry")["attempt"]
            .as_u64()
            .expect("attempt number")
    };
    assert!(stop(&fx, "ma", &env).blocked());
    assert_eq!(attempt(&fx), 1);
    fx.set_source("fn g() { a() }\n");
    assert!(stop(&fx, "ma", &env).blocked());
    assert_eq!(attempt(&fx), 2);

    // reset_after_secs default 600: idle 500s keeps the counter, idle 700s resets.
    // Observed through the block's attempt number (seed attempts=1):
    // kept -> this block is attempt 2, reset -> attempt 1.
    let within = Fx::repo();
    within.seed_state("ra", 1, 500);
    let o = stop(&within, "ra", &env);
    assert!(o.blocked(), "{}", o.stdout);
    assert_eq!(
        log_entries(&within.state()).pop().unwrap()["attempt"].as_u64(),
        Some(2),
        "500s idle is inside the 600s window: counter kept"
    );
    assert_eq!(
        saved_attempts(&within.state(), "ra"),
        2,
        "state file must hold the incremented counter"
    );

    let beyond = Fx::repo();
    beyond.seed_state("ra", 1, 700);
    let o = stop(&beyond, "ra", &env);
    assert!(o.blocked(), "{}", o.stdout);
    assert_eq!(
        log_entries(&beyond.state()).pop().unwrap()["attempt"].as_u64(),
        Some(1),
        "700s idle exceeds 600s: counter restarts"
    );
    assert_eq!(saved_attempts(&beyond.state(), "ra"), 1);
}

// ─────────────────────────── 8. no_giveup_config_errors ───────────────────────────

#[test]
fn claim_no_giveup_config_errors() {
    // Control: the seeded state (attempts=5 >= default max 2) is in effect.
    // Proven with a MEASURED subprocess verdict (checker FAILs every property):
    // below-threshold with attempts past max gives up (`giveup`), which an
    // unseeded run (attempts 0 -> block) does not.
    let fail_cfg = "mode = \"subprocess\"\nchecker_cmd = \"echo 'PROP idempotence: FAIL'; \
         echo 'PROP error-path: FAIL'; echo 'PROP output-schema: FAIL'\"\n";
    let unseeded = Fx::repo();
    unseeded.write_home_config(fail_cfg);
    let o = stop(&unseeded, "ng", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert!(o.blocked(), "unseeded measured FAIL blocks: {}", o.stdout);
    let ctl = Fx::repo();
    ctl.write_home_config(fail_cfg);
    ctl.seed_state("ng", 5, 0);
    let _ = stop(&ctl, "ng", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert_eq!(
        last_verdict(&ctl.state()),
        "giveup",
        "seeded attempts>=max must be in effect (measured verdict path)"
    );

    // config-unreadable
    let a = Fx::repo();
    a.write_home_config("this is = = not toml\n");
    a.seed_state("ng", 5, 0);
    let o = stop(&a, "ng", &[]);
    assert!(
        o.blocked(),
        "config-unreadable must block despite attempts>=max: {}",
        o.stdout
    );
    assert_eq!(last_verdict(&a.state()), "config-unreadable");

    // config-invalid
    let b = Fx::repo();
    b.write_home_config("mode = \"bogus\"\n");
    b.seed_state("ng", 5, 0);
    let o = stop(&b, "ng", &[]);
    assert!(
        o.blocked(),
        "config-invalid must block despite attempts>=max: {}",
        o.stdout
    );
    assert_eq!(last_verdict(&b.state()), "config-invalid");

    // criteria-unreadable: the criteria file exists but is not valid UTF-8.
    let c = Fx::repo();
    std::fs::write(c.dir.join(".propguard-criteria"), [0xffu8, 0xfe, 0xfd]).unwrap();
    c.seed_state("ng", 5, 0);
    let o = stop(&c, "ng", &[]);
    assert!(
        o.blocked(),
        "criteria-unreadable must block despite attempts>=max: {}",
        o.stdout
    );
    assert_eq!(last_verdict(&c.state()), "criteria-unreadable");
}

// ─────────────────────────────── 9. outage_ledger_e2e ───────────────────────────────

fn overwatch_ledger(home: &Path) -> Option<PathBuf> {
    let base = home.join(".overwatch");
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path().join("overwatch").join("violations.jsonl");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn ledger_signatures(home: &Path) -> Vec<String> {
    let Some(p) = overwatch_ledger(home) else {
        return vec![];
    };
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["signature"].as_str().map(str::to_string))
        .collect()
}

/// A repo whose checker always errors and which gives up after ONE block.
fn outage_fx() -> Fx {
    let fx = Fx::repo();
    fx.write_home_config("mode = \"subprocess\"\nmax_attempts = 1\nchecker_cmd = \"exit 1\"\n");
    fx
}

/// Two stops in one session: block (checker-unavailable) then give up.
fn drive_giveup(fx: &Fx, session: &str) -> Out {
    let env = [("PROPGUARD_CRITERIA", IDEM)];
    let first = stop(fx, session, &env);
    assert!(
        first.blocked(),
        "first stop blocks: {} / {}",
        first.stdout,
        first.stderr
    );
    stop(fx, session, &env)
}

#[test]
fn claim_outage_ledger_e2e() {
    // Each give-up is recorded in the ledger; the third distinct session makes
    // it systemic and flips to a block (the isolated outcomes are not pinned).
    let fx = outage_fx();
    drive_giveup(&fx, "out-A");
    assert_eq!(last_verdict(&fx.state()), "checker-error-giveup");
    assert!(
        ledger_signatures(&fx.dir)
            .iter()
            .any(|s| s == "propguard:checker-outage"),
        "give-up must be appended to the overwatch ledger: {:?}",
        ledger_signatures(&fx.dir)
    );
    drive_giveup(&fx, "out-B");
    let o = drive_giveup(&fx, "out-C");
    assert!(
        o.blocked(),
        "recurring give-up must escalate to a block: {} / {}",
        o.stdout,
        o.stderr
    );
    assert_eq!(last_verdict(&fx.state()), "checker-outage-systemic");

    // Unreadable ledger (undecodable line) -> block, with its OWN tag.
    let u = outage_fx();
    drive_giveup(&u, "out-A");
    let ledger = overwatch_ledger(&u.dir).expect("ledger exists after first give-up");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&ledger)
        .unwrap();
    writeln!(f, "this line is not a violation event").unwrap();
    drop(f);
    let o = drive_giveup(&u, "out-B");
    assert!(
        o.blocked(),
        "undecodable ledger must block: {} / {}",
        o.stdout,
        o.stderr
    );
    assert_eq!(last_verdict(&u.state()), "checker-outage-undetermined");

    // Unreadable ledger (path is a directory) -> same block.
    let d = outage_fx();
    drive_giveup(&d, "out-A");
    let ledger = overwatch_ledger(&d.dir).expect("ledger exists");
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let o = drive_giveup(&d, "out-B");
    assert!(
        o.blocked(),
        "unreadable ledger must block: {} / {}",
        o.stdout,
        o.stderr
    );
    assert_eq!(last_verdict(&d.state()), "checker-outage-undetermined");
}

// ───────────────────── 10/11/12. install / init / uninstall ─────────────────────

fn settings(fx: &Fx) -> Value {
    let p = fx.dir.join(".claude").join("settings.json");
    serde_json::from_str(&std::fs::read_to_string(p).expect("settings.json exists"))
        .expect("settings.json is JSON")
}

fn stop_commands(v: &Value) -> Vec<String> {
    v["hooks"]["Stop"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn seed_foreign_settings(fx: &Fx) {
    let d = fx.dir.join(".claude");
    std::fs::create_dir_all(&d).unwrap();
    let body = serde_json::json!({
        "theme": "dark",
        "hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "other-stop-hook"}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": "other-pre-hook"}]}]
        }
    });
    std::fs::write(d.join("settings.json"), body.to_string()).unwrap();
}

#[test]
fn claim_install_wires_stop() {
    // Merge into an existing settings.json: foreign keys + hooks preserved.
    let fx = Fx::bare();
    seed_foreign_settings(&fx);
    let o = run(&fx, &["install"], &[]);
    assert_eq!(o.code, 0, "install: {} {}", o.stdout, o.stderr);
    let v = settings(&fx);
    assert_eq!(v["theme"], "dark", "foreign top-level key preserved");
    let cmds = stop_commands(&v);
    assert!(
        cmds.iter()
            .any(|c| c.contains("propguard") && c.ends_with(" check")),
        "a propguard Stop hook invoking `check` must be wired: {cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c == "other-stop-hook"),
        "foreign Stop hook kept: {cmds:?}"
    );
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "other-pre-hook",
        "foreign event preserved"
    );

    // Creates the file when none exists.
    let fresh = Fx::bare();
    let o = run(&fresh, &["install"], &[]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let cmds = stop_commands(&settings(&fresh));
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    assert!(cmds[0].contains("propguard") && cmds[0].ends_with(" check"));
}

#[test]
fn claim_uninstall_removes() {
    let fx = Fx::bare();
    seed_foreign_settings(&fx);
    assert_eq!(run(&fx, &["install"], &[]).code, 0);
    assert!(stop_commands(&settings(&fx))
        .iter()
        .any(|c| c.contains("propguard")));
    let o = run(&fx, &["uninstall"], &[]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let cmds = stop_commands(&settings(&fx));
    assert!(
        !cmds.iter().any(|c| c.contains("propguard")),
        "propguard hook must be gone: {cmds:?}"
    );
    assert_eq!(
        cmds,
        vec!["other-stop-hook".to_string()],
        "foreign Stop hook kept"
    );
    assert_eq!(settings(&fx)["theme"], "dark");
}

#[test]
fn claim_init_starter() {
    let fx = Fx::bare();
    let o = run(&fx, &["init"], &[]);
    assert_eq!(o.code, 0, "init: {} {}", o.stdout, o.stderr);
    let toml_path = fx.dir.join("propguard.toml");
    assert!(toml_path.is_file(), "init must write ./propguard.toml");
    // It parses as a valid config: once trusted, `status` loads it without a
    // config error and reports it as the config source.
    trust(&fx);
    let st = run(&fx, &["status"], &[]);
    assert_eq!(st.code, 0);
    assert!(
        !st.stdout.contains("config error"),
        "starter must parse: {}",
        st.stdout
    );
    assert!(
        st.stdout.contains("propguard.toml"),
        "status must load it: {}",
        st.stdout
    );
    assert!(st.stdout.contains("mode:           inject"));
}

// ─────────────────────────────── 13. state_dir ───────────────────────────────

#[test]
fn claim_state_dir() {
    let env = [("PROPGUARD_CRITERIA", IDEM)];
    // default: ~/.propguard/state
    let a = Fx::repo();
    let o = run_opts(&a, &["check"], &payload(&a, "sd", false), &env, false);
    assert!(o.blocked(), "{} / {}", o.stdout, o.stderr);
    assert!(
        a.dir.join(".propguard/state/sessions/sd.json").is_file(),
        "state must live under ~/.propguard/state"
    );

    // config key state_dir
    let b = Fx::repo();
    let custom = b.dir.join("custom-state");
    b.write_home_config(&format!("state_dir = \"{}\"\n", custom.display()));
    let o = run_opts(&b, &["check"], &payload(&b, "sd", false), &env, false);
    assert!(o.blocked(), "{} / {}", o.stdout, o.stderr);
    assert!(
        custom.join("sessions/sd.json").is_file(),
        "config state_dir must be used"
    );
    assert!(!b.dir.join(".propguard/state/sessions/sd.json").exists());

    // PROPGUARD_STATE_DIR overrides the config key
    let c = Fx::repo();
    let custom = c.dir.join("custom-state");
    let envdir = c.dir.join("env-state");
    c.write_home_config(&format!("state_dir = \"{}\"\n", custom.display()));
    let o = run_opts(
        &c,
        &["check"],
        &payload(&c, "sd", false),
        &[
            ("PROPGUARD_CRITERIA", IDEM),
            ("PROPGUARD_STATE_DIR", envdir.to_str().unwrap()),
        ],
        false,
    );
    assert!(o.blocked(), "{} / {}", o.stdout, o.stderr);
    assert!(
        envdir.join("sessions/sd.json").is_file(),
        "env var must win"
    );
    assert!(
        !custom.join("sessions/sd.json").exists(),
        "config dir must be untouched"
    );
}

// ─────────────────────────────── 14. example_toml ───────────────────────────────

#[test]
fn claim_example_toml() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("propguard.example.toml");
    assert!(ex.is_file(), "README links propguard.example.toml");
    let fx = Fx::repo();
    std::fs::copy(&ex, fx.dir.join("propguard.toml")).unwrap();
    trust(&fx);
    let st = run(&fx, &["status"], &[]);
    assert!(
        !st.stdout.contains("config error"),
        "example must load: {}",
        st.stdout
    );
    assert!(st.stdout.contains("propguard.toml"), "{}", st.stdout);
    let o = stop(&fx, "ex", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert!(o.blocked(), "{} / {}", o.stdout, o.stderr);
    assert_eq!(
        last_verdict(&fx.state()),
        "below-threshold",
        "example config must be valid"
    );
}

// ─────────────────────────────── 15. key_knobs ───────────────────────────────

fn knob_fx(body: &str) -> Fx {
    let fx = Fx::repo();
    fx.write_project_config(body);
    trust(&fx);
    fx
}

fn derived_count(stdout: &str) -> Option<usize> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("derived "))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

#[test]
fn claim_key_knobs() {
    let mut fails: Vec<String> = Vec::new();
    let all_kw = "idempotent error schema determinist sort atomic";

    // (1) knobs that `status` prints reach the resolved config.
    let fx = knob_fx(
        "enabled = true\nmode = \"subprocess\"\nmin_properties = 3\nmax_properties = 4\n\
         threshold = 2\nmax_attempts = 7\ncriteria_file = \"my-criteria.txt\"\n\
         checker_cmd = \"echo knob\"\n",
    );
    std::fs::write(fx.dir.join("my-criteria.txt"), "sort order").unwrap();
    let st = run(&fx, &["status"], &[]).stdout;
    for want in [
        "enabled:        true",
        "mode:           subprocess",
        "checker_cmd:    echo knob",
        "min_properties: 3",
        "max_properties: 4",
        "threshold:      2",
        "max_attempts:   7",
        "criteria_file:  my-criteria.txt",
        "done_criteria:  sort order",
    ] {
        if !st.contains(want) {
            fails.push(format!("status lacks {want:?}: {st}"));
        }
    }

    // (2) max_properties above 5 is clamped to 5.
    let fx = knob_fx("max_properties = 9\n");
    let st = run(&fx, &["status"], &[("PROPGUARD_CRITERIA", all_kw)]).stdout;
    if derived_count(&st) != Some(5) {
        fails.push(format!("max_properties=9 must clamp to 5 derived: {st}"));
    }
    // (2b) OBSERVED: the 3 is only a default, NOT a floor. min_properties=1 /
    // max_properties=2 are honoured as-is.
    let fx = knob_fx("min_properties = 1\nmax_properties = 2\n");
    let st = run(&fx, &["status"], &[("PROPGUARD_CRITERIA", "zzz qqq")]).stdout;
    if derived_count(&st) != Some(1) {
        fails.push(format!("min=1 must derive 1 padded property: {st}"));
    }
    let st = run(&fx, &["status"], &[("PROPGUARD_CRITERIA", all_kw)]).stdout;
    if derived_count(&st) != Some(2) {
        fails.push(format!("max_properties=2 must cap at 2: {st}"));
    }

    // (3) min_changed_files
    let fx = knob_fx("min_changed_files = 2\n");
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if o.blocked() || last_verdict(&fx.state()) != "no-code-changes" {
        fails.push(format!(
            "min_changed_files=2 with 1 file must allow no-code-changes: {}",
            o.stdout
        ));
    }
    // (4) include / exclude
    let fx = knob_fx("include = [\"**/*.py\"]\n");
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if o.blocked() || last_verdict(&fx.state()) != "no-code-changes" {
        fails.push(format!("include=*.py must skip a.rs: {}", o.stdout));
    }
    let fx = knob_fx("exclude = [\"**/*.rs\"]\n");
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if o.blocked() || last_verdict(&fx.state()) != "no-code-changes" {
        fails.push(format!("exclude=*.rs must skip a.rs: {}", o.stdout));
    }
    // (5) max_diff_bytes
    let fx = knob_fx("max_diff_bytes = 10\n");
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if !o.blocked() || last_verdict(&fx.state()) != "diff-truncated" {
        fails.push(format!(
            "max_diff_bytes=10 must truncate: {} / {}",
            o.stdout, o.stderr
        ));
    }
    // (6) reset_after_secs: 100s idle resets under reset_after_secs=10 (default 600 would give up)
    let fx = knob_fx("reset_after_secs = 10\n");
    fx.seed_state("kn", 2, 100);
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if !o.blocked() || last_verdict(&fx.state()) != "below-threshold" {
        fails.push(format!(
            "reset_after_secs=10 must reset a 100s-old counter: {}",
            o.stdout
        ));
    }
    // (7) threshold + checker_cmd + mode: one PASS of three.
    let checker = "checker_cmd = \"echo 'PROP error-path: PASS'; echo 'PROP output-schema: FAIL'; \
                   echo 'PROP determinism: FAIL'\"\n";
    let fx = knob_fx(&format!("mode = \"subprocess\"\nthreshold = 1\n{checker}"));
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", "zzz qqq")]);
    if o.blocked() || last_verdict(&fx.state()) != "properties-satisfied" {
        fails.push(format!(
            "threshold=1, 1 PASS must allow: {} / {}",
            o.stdout, o.stderr
        ));
    }
    let fx = knob_fx(&format!("mode = \"subprocess\"\nthreshold = 3\n{checker}"));
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", "zzz qqq")]);
    if !o.blocked() || last_verdict(&fx.state()) != "below-threshold" {
        fails.push(format!(
            "threshold=3, 1 PASS must block: {} / {}",
            o.stdout, o.stderr
        ));
    }
    // (8) checker_timeout_secs
    let fx =
        knob_fx("mode = \"subprocess\"\nchecker_cmd = \"sleep 30\"\nchecker_timeout_secs = 1\n");
    let t0 = Instant::now();
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if t0.elapsed() > Duration::from_secs(20) || last_verdict(&fx.state()) != "checker-unavailable"
    {
        fails.push(format!(
            "checker_timeout_secs=1 must cut `sleep 30` short (took {:?}): {}",
            t0.elapsed(),
            o.stdout
        ));
    }
    // (9) enabled = false
    let fx = knob_fx("enabled = false\n");
    let o = stop(&fx, "kn", &[("PROPGUARD_CRITERIA", IDEM)]);
    if o.blocked() {
        fails.push(format!("enabled=false must not block: {}", o.stdout));
    }

    assert!(
        fails.is_empty(),
        "key knob failures:\n{}",
        fails.join("\n---\n")
    );
}

// ─────────────────────────────── 16. trust_cmd ───────────────────────────────

#[test]
fn claim_trust_cmd() {
    let fx = Fx::repo();
    fx.write_project_config("mode = \"subprocess\"\nthreshold = 2\nmax_attempts = 9\n");
    let before = run(&fx, &["status"], &[]);
    assert!(
        before.stdout.contains("mode:           inject"),
        "untrusted file ignored: {}",
        before.stdout
    );
    assert!(
        before.stdout.contains("max_attempts:   2"),
        "{}",
        before.stdout
    );
    let o = run(&fx, &["trust"], &[]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let after = run(&fx, &["status"], &[]);
    assert!(
        after.stdout.contains("mode:           subprocess"),
        "trusted file honoured: {}",
        after.stdout
    );
    assert!(after.stdout.contains("threshold:      2"));
    assert!(after.stdout.contains("max_attempts:   9"));
}

// ─────────────────────────────── 17. status_shows ───────────────────────────────

#[test]
fn claim_status_shows() {
    let fx = Fx::bare();
    let o = run(
        &fx,
        &["status"],
        &[("PROPGUARD_CRITERIA", "must be idempotent")],
    );
    assert_eq!(o.code, 0);
    for want in [
        "mode:           inject",
        "threshold:      3",
        "max_attempts:   2",
        "done_criteria:  must be idempotent",
        "[idempotence]",
    ] {
        assert!(
            o.stdout.contains(want),
            "status lacks {want:?}: {}",
            o.stdout
        );
    }
}

// ───────────────────── 18/19. checker timeout (home config) ─────────────────────

#[test]
fn claim_timeout_home_config() {
    let fx = Fx::repo();
    fx.write_home_config(
        "mode = \"subprocess\"\nchecker_cmd = \"sleep 30\"\nchecker_timeout_secs = 1\n",
    );
    let t0 = Instant::now();
    let o = stop(&fx, "th", &[("PROPGUARD_CRITERIA", IDEM)]);
    let took = t0.elapsed();
    assert!(
        o.blocked(),
        "timeout is a checker error -> block: {} / {}",
        o.stdout,
        o.stderr
    );
    assert_eq!(last_verdict(&fx.state()), "checker-unavailable");
    assert!(
        took < Duration::from_secs(20),
        "top-level checker_timeout_secs=1 in ~/.propguard/config.toml was ignored (took {took:?})"
    );
}

#[test]
fn claim_timeout_negative_blocks() {
    for bad in [
        "checker_timeout_secs = -1\n",
        "checker_timeout_secs = \"abc\"\n",
    ] {
        let fx = knob_fx(bad);
        let o = stop(&fx, "tn", &[]);
        assert!(
            o.blocked(),
            "{bad:?} must block: {} / {}",
            o.stdout,
            o.stderr
        );
        assert_eq!(last_verdict(&fx.state()), "config-unreadable", "{bad:?}");
        assert!(
            o.reason().contains("cannot parse"),
            "reason names the parse failure: {}",
            o.reason()
        );
    }
    // Not a silent default: a valid value does not block as config-unreadable.
    let ok = knob_fx("checker_timeout_secs = 45\n");
    let o = stop(&ok, "tn", &[]);
    assert!(
        !o.blocked(),
        "control: valid value is not a config error: {}",
        o.stdout
    );
    assert_ne!(last_verdict(&ok.state()), "config-unreadable");
}

// ─────────────────────────── 20. skip_contract ───────────────────────────

#[test]
fn claim_skip_contract() {
    let fx = Fx::repo();
    let sess = "skip-sess";
    let env = [
        ("PROPGUARD_CRITERIA", IDEM),
        ("CLAUDE_CODE_SESSION_ID", sess),
    ];
    let skip_file = fx.state().join("skips").join(format!("{sess}.skip"));

    // Reason required: missing and blank both fail and arm nothing.
    let o = run(&fx, &["skip"], &env);
    assert_ne!(o.code, 0, "skip without --reason must fail");
    let o = run(&fx, &["skip", "--reason", "   "], &env);
    assert_ne!(
        o.code, 0,
        "skip with a blank reason must fail: {} {}",
        o.stdout, o.stderr
    );
    assert!(!skip_file.exists(), "no skip may be armed without a reason");

    // Issue.
    let o = run(&fx, &["skip", "--reason", "doc-only change"], &env);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(skip_file.is_file());
    assert!(
        log_entries(&fx.state())
            .iter()
            .any(|e| e["event"] == "skip_issued"
                && e["session_id"] == sess
                && e["reason"] == "doc-only change"),
        "issue must be in the gate log: {:?}",
        log_entries(&fx.state())
    );

    // First stop of the issuing session consumes it...
    let first = run_opts(&fx, &["check"], &payload(&fx, sess, false), &env, true);
    assert!(
        !first.blocked(),
        "skip must let the first stop through: {}",
        first.stdout
    );
    assert!(
        log_entries(&fx.state())
            .iter()
            .any(|e| e["event"] == "skip_consumed"
                && e["session_id"] == sess
                && e["reason"] == "doc-only change"),
        "consumption must be in the gate log: {:?}",
        log_entries(&fx.state())
    );
    // ...and the second blocks again (one-shot).
    let second = run_opts(&fx, &["check"], &payload(&fx, sess, false), &env, true);
    assert!(
        second.blocked(),
        "skip is one-shot; 2nd stop must block: {} / {}",
        second.stdout,
        second.stderr
    );
}

// ─────────────────────────────── 21. enabled_false ───────────────────────────────

#[test]
fn claim_enabled_false() {
    let on = knob_fx("enabled = true\n");
    assert!(
        stop(&on, "en", &[("PROPGUARD_CRITERIA", IDEM)]).blocked(),
        "control: enabled=true blocks"
    );
    let off = knob_fx("enabled = false\n");
    let o = stop(&off, "en", &[("PROPGUARD_CRITERIA", IDEM)]);
    assert_eq!(o.code, 0);
    assert!(
        !o.blocked(),
        "enabled=false must disable the gate: {}",
        o.stdout
    );
}

// ─────────────────────────── timeout kills the process tree ───────────────────────────

#[test]
fn claim_timeout_kills_tree() {
    let fx = Fx::repo();
    let marker = fx.dir.join("grandchild.marker");
    // Shell-wrapped checker (`&`, `;`): a backgrounded grandchild would write the
    // marker after ~3s if it outlived the 1s timeout.
    fx.write_home_config(&format!(
        "mode = \"subprocess\"\nchecker_timeout_secs = 1\n\
         checker_cmd = \"(sleep 3; touch {}) & sleep 30\"\n",
        marker.display()
    ));
    let t0 = Instant::now();
    let o = stop(&fx, "kt", &[("PROPGUARD_CRITERIA", IDEM)]);
    let took = t0.elapsed();
    assert!(took < Duration::from_secs(20), "stop took {took:?}");
    assert!(
        o.blocked(),
        "timeout is a checker error -> block: {}",
        o.stdout
    );
    assert_eq!(last_verdict(&fx.state()), "checker-unavailable");
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "the backgrounded grandchild survived the timeout and wrote its marker"
    );
}
