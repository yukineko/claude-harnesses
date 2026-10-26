// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for the prose in `crates/overwatch/README.md`,
//! `README.ja.md` and `skills/overwatch/SKILL.md`.
//!
//! One `claim_<id>` test per prose claim. Every test drives the real
//! `overwatch` binary against an isolated `HOME` + a fresh git repo (never the
//! real `~/.overwatch`), pins `PATH` so the machine's real helper binaries are
//! not consulted, and asserts the specific observable the sentence states.
//! Time-dependent facts are driven by seeding `leases.json` with explicit
//! `heartbeat_at` values (margins of 100s around the 1800s TTL) instead of
//! sleeping.

use overwatch::store::{self, Lease};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const BASE_PATH: &str = "/usr/bin:/bin";

// ───────────────────────── helpers ─────────────────────────

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// A throwaway HOME + git repo.
struct Sb {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    /// Extra dir prepended to PATH (fake helper binaries).
    fakebin: Option<PathBuf>,
}

impl Sb {
    fn new() -> Sb {
        let t = TempDir::new().unwrap();
        let root = t.path().to_path_buf();
        let home = root.join("home");
        let proj = root.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        let st = Command::new("git")
            .current_dir(&proj)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(st.success(), "fixture: git init failed");
        Sb {
            _t: t,
            root,
            home,
            proj,
            fakebin: None,
        }
    }

    fn with_fakebin(mut self) -> Sb {
        let d = self.root.join("fakebin");
        std::fs::create_dir_all(&d).unwrap();
        self.fakebin = Some(d);
        self
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        let path = match &self.fakebin {
            Some(d) => format!("{}:{BASE_PATH}", d.display()),
            None => BASE_PATH.to_string(),
        };
        c.current_dir(&self.proj)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("OVERWATCH_RUN_ID")
            .env_remove("OVERWATCH_DUP_THRESHOLD")
            .stdin(Stdio::null())
            .args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{args:?} failed: {}", dump(&o));
        o
    }

    /// `<HOME>/.overwatch/<basename>-<fnv1a32 of canonical root>/overwatch`.
    fn store_dir(&self) -> PathBuf {
        let canon = self.proj.canonicalize().unwrap();
        let full = canon.to_string_lossy().to_string();
        let base: String = canon
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        self.home
            .join(".overwatch")
            .join(format!("{base}-{:08x}", fnv1a32(&full)))
            .join("overwatch")
    }

    fn leases_file(&self) -> PathBuf {
        self.store_dir().join("leases.json")
    }

    fn events_file(&self) -> PathBuf {
        self.store_dir().join("events.jsonl")
    }

    fn leases(&self) -> Value {
        match std::fs::read_to_string(self.leases_file()) {
            Ok(s) => serde_json::from_str(&s).unwrap(),
            Err(_) => Value::Null,
        }
    }

    fn events(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.events_file()) {
            Ok(s) => s
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Seed leases.json: (key, session, claimed_at, heartbeat_at, scope).
    fn seed(&self, rows: &[(&str, &str, i64, i64, &[&str])]) {
        let mut m = serde_json::Map::new();
        for (key, session, claimed, hb, scope) in rows {
            m.insert(
                (*key).to_string(),
                json!({
                    "key": key,
                    "title": format!("title of {key}"),
                    "session_id": session,
                    "run_id": format!("run-seed-{key}"),
                    "claimed_at": claimed,
                    "heartbeat_at": hb,
                    "scope": scope,
                    "done_criteria": null,
                }),
            );
        }
        std::fs::create_dir_all(self.store_dir()).unwrap();
        std::fs::write(self.leases_file(), serde_json::to_string(&m).unwrap()).unwrap();
    }

    /// Seed one lease with an explicit title / scope (for advisory-summary tests).
    fn seed_peer(
        &self,
        m: &mut serde_json::Map<String, Value>,
        key: &str,
        title: &str,
        scope: &[&str],
        hb: i64,
    ) {
        m.insert(
            key.to_string(),
            json!({
                "key": key, "title": title, "session_id": format!("sess-{key}"),
                "run_id": format!("run-{key}"), "claimed_at": hb, "heartbeat_at": hb,
                "scope": scope, "done_criteria": null,
            }),
        );
    }

    fn write_leases_map(&self, m: serde_json::Map<String, Value>) {
        std::fs::create_dir_all(self.store_dir()).unwrap();
        std::fs::write(self.leases_file(), serde_json::to_string(&m).unwrap()).unwrap();
    }

    /// Write a LIVE pid into the lease lock so the next mutator sees contention.
    fn hold_lock(&self) -> PathBuf {
        let p = self.store_dir().join("leases.lock");
        std::fs::create_dir_all(self.store_dir()).unwrap();
        std::fs::write(
            &p,
            json!({ "pid": std::process::id(), "acquired_at": now() }).to_string(),
        )
        .unwrap();
        p
    }
}

fn dump(o: &Output) -> String {
    format!(
        "status={:?} stdout={:?} stderr={:?}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn json_out(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", dump(o)))
}

#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn begin(sb: &Sb, key: &str, session: &str) -> Output {
    sb.run(&["begin", "--key", key, "--title", "t", "--session", session])
}

fn session_ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap().to_string())
        .collect()
}

/// A fake `condukt` that records its argv and exits 0.
#[cfg(unix)]
fn fake_condukt(sb: &Sb) -> PathBuf {
    let log = sb.root.join("condukt-args.log");
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && exit 0\necho \"$@\" >> '{}'\nexit 0\n",
        log.display()
    );
    write_script(&sb.fakebin.clone().unwrap().join("condukt"), &script);
    log
}

// ───────────────────────── begin / lease lifecycle ─────────────────────────

#[test]
fn claim_rd_begin_held_key_exit1_skip_json() {
    // (a) key held by another live session -> exit 1 + holder JSON.
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let held = sb.leases()["k"].clone();
    let o = begin(&sb, "k", "B");
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    let v = json_out(&o);
    assert_eq!(v["skipped"], json!(true));
    assert_eq!(v["holder"]["session_id"], json!("A"));
    assert_eq!(v["holder"]["run_id"], held["run_id"]);
    assert_eq!(v["holder"]["heartbeat_at"], held["heartbeat_at"]);
    assert!(v["holder"]["heartbeat_at"].is_i64());
    assert!(v.get("reason").is_none(), "held-by-other has no reason key");
    // the loser did not take the lease
    assert_eq!(sb.leases()["k"]["session_id"], json!("A"));

    // (b) lease-registry lock contended -> exit 1 + lock_contended JSON,
    // and the key is NOT claimed ("whether the key is free was not checked").
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "other", "--title", "t", "--session", "A"]);
    let lock = sb.hold_lock();
    let o = begin(&sb, "fresh-key", "B");
    let _ = std::fs::remove_file(&lock);
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    assert_eq!(
        json_out(&o),
        json!({"skipped": true, "reason": "lock_contended"})
    );
    assert!(
        sb.leases().get("fresh-key").is_none(),
        "a contended begin must not claim the key"
    );
}

#[test]
fn claim_rd_begin_exit0_refresh_keeps_claimed_at() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[("k", "A", t - 1000, t - 500, &[])]);
    let o = begin(&sb, "k", "A");
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    let l = sb.leases()["k"].clone();
    assert_eq!(l["claimed_at"], json!(t - 1000), "claimed_at must be kept");
    assert!(
        l["heartbeat_at"].as_i64().unwrap() >= t - 5,
        "heartbeat_at must be refreshed, got {}",
        l["heartbeat_at"]
    );
}

#[test]
fn claim_rd_begin_internal_error_exit1_no_json() {
    // (a) corrupt leases.json -> exit 1, error on stderr, no skip JSON.
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k0", "--title", "t", "--session", "A"]);
    std::fs::write(sb.leases_file(), "{ this is not json").unwrap();
    let o = begin(&sb, "k", "B");
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    assert!(err(&o).contains("corrupt"), "{}", dump(&o));
    assert!(!out(&o).contains("skipped"), "{}", dump(&o));

    // (b) an UNRESOLVABLE store root: exit 1, stderr names the root as
    // undetermined, and no store is created. The stdout reason it prints
    // (`lock_contended`) is deliberately NOT pinned: it misreports the cause
    // and is an open finding, not spec.
    let sb = Sb::new();
    std::fs::remove_dir_all(sb.proj.join(".git")).unwrap();
    std::fs::write(sb.proj.join(".git"), "garbage\n").unwrap();
    let o = begin(&sb, "k", "B");
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    assert!(err(&o).contains("undetermined"), "{}", dump(&o));
    assert!(
        !sb.home.join(".overwatch").exists(),
        "no store may be created"
    );
}

#[test]
fn claim_rd_lease_scope_done_criteria_fields() {
    let sb = Sb::new();
    sb.ok(&[
        "begin",
        "--key",
        "k",
        "--title",
        "t",
        "--session",
        "A",
        "--scope",
        "A,B",
        "--done-criteria",
        "T",
    ]);
    let l = sb.leases()["k"].clone();
    assert_eq!(l["scope"], json!(["A", "B"]));
    assert_eq!(l["done_criteria"], json!("T"));
    assert!(l["scope"].as_array().unwrap().iter().all(Value::is_string));
    assert!(l["done_criteria"].is_string());
    // optional: omitted flags -> [] and null
    sb.ok(&["begin", "--key", "k2", "--title", "t", "--session", "A"]);
    let l2 = sb.leases()["k2"].clone();
    assert_eq!(l2["scope"], json!([]));
    assert!(l2["done_criteria"].is_null());
}

#[test]
fn claim_rd_legacy_leases_json_loads() {
    let legacy = r#"{"k":{"key":"k","title":"t","session_id":"A","run_id":"r","claimed_at":5,"heartbeat_at":6}}"#;
    let reg: store::LeaseRegistry = serde_json::from_str(legacy).unwrap();
    assert!(reg["k"].scope.is_empty());
    assert_eq!(reg["k"].done_criteria, None);
    // and through the CLI: a live legacy lease is readable by `lease --json`
    let sb = Sb::new();
    let t = now();
    let legacy = json!({"k": {"key":"k","title":"t","session_id":"A","run_id":"r","claimed_at":t,"heartbeat_at":t}});
    std::fs::create_dir_all(sb.store_dir()).unwrap();
    std::fs::write(sb.leases_file(), legacy.to_string()).unwrap();
    let o = sb.ok(&["lease", "--session", "A", "--json"]);
    let v = json_out(&o);
    assert_eq!(v["scope"], json!([]));
    assert!(v["done_criteria"].is_null());
}

#[test]
fn claim_rd_lease_has_heartbeat_and_claimed_at() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k1", "--title", "t", "--session", "A"]);
    sb.ok(&["begin", "--key", "k2", "--title", "t", "--session", "B"]);
    let leases = sb.leases();
    let m = leases.as_object().unwrap();
    assert_eq!(m.len(), 2);
    for (k, l) in m {
        assert!(
            l["heartbeat_at"].is_i64(),
            "{k}: heartbeat_at not numeric: {l}"
        );
        assert!(l["claimed_at"].is_i64(), "{k}: claimed_at not numeric: {l}");
    }
    // the `lease --json` anchor carries both too
    let v = json_out(&sb.ok(&["lease", "--session", "A", "--json"]));
    assert!(v["heartbeat_at"].is_i64() && v["claimed_at"].is_i64());
}

// ───────────────────────── staleness / TTL ─────────────────────────

#[test]
fn claim_rd_lease_ttl_1800_stale() {
    // library boundary: exactly 1800 is NOT stale, 1801 IS.
    assert_eq!(store::LEASE_TTL_SECS, 1800);
    let lease = Lease {
        key: "k".into(),
        title: "t".into(),
        session_id: "A".into(),
        run_id: "r".into(),
        claimed_at: 100,
        heartbeat_at: 100,
        scope: vec![],
        done_criteria: None,
    };
    assert!(!store::is_stale(&lease, 100 + 1800));
    assert!(store::is_stale(&lease, 100 + 1801));

    // CLI: aged 1700s -> still live (other session's begin is skipped);
    // aged 1900s -> stale (other session's begin succeeds).
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("live", "A", t - 1700, t - 1700, &[]),
        ("dead", "A", t - 1900, t - 1900, &[]),
    ]);
    assert_eq!(begin(&sb, "live", "B").status.code(), Some(1));
    let o = begin(&sb, "dead", "B");
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    assert_eq!(sb.leases()["dead"]["session_id"], json!("B"));
}

#[test]
fn claim_rd_stale_dropped_by_every_mutation() {
    let mutations: Vec<(&str, Vec<&str>)> = vec![
        (
            "begin",
            vec!["begin", "--key", "newkey", "--title", "t", "--session", "S"],
        ),
        ("run", vec!["run", "--key", "fresh"]),
        ("end", vec!["end", "--key", "fresh", "--status", "done"]),
        ("heartbeat", vec!["heartbeat", "--key", "fresh"]),
        ("reassign", vec!["reassign", "--key", "fresh", "--to", "B"]),
        ("reap", vec!["reap"]),
    ];
    for (name, args) in mutations {
        let sb = Sb::new();
        let t = now();
        sb.seed(&[
            ("stale", "dead", t - 1900, t - 1900, &[]),
            ("fresh", "F", t - 10, t - 10, &[]),
        ]);
        let o = sb.run(&args);
        assert!(o.status.success(), "{name}: {}", dump(&o));
        assert!(
            sb.leases().get("stale").is_none(),
            "{name} did not drop the stale lease: {}",
            sb.leases()
        );
    }

    // readers ignore it
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("stale", "dead", t - 1900, t - 1900, &[]),
        ("fresh", "F", t - 10, t - 10, &[]),
    ]);
    let sessions = json_out(&sb.ok(&["sessions", "--json"]));
    assert_eq!(session_ids(&sessions), vec!["F".to_string()]);
    let status = json_out(&sb.ok(&["status", "--json"]));
    assert_eq!(session_ids(&status["sessions"]), vec!["F".to_string()]);
    let l = sb.run(&["lease", "--session", "dead", "--json"]);
    assert_eq!(l.status.code(), Some(1), "{}", dump(&l));
    assert!(l.stdout.is_empty() && l.stderr.is_empty(), "{}", dump(&l));

    // and a dead holder never blocks a begin by another session
    let o = begin(&sb, "stale", "other");
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    assert_eq!(sb.leases()["stale"]["session_id"], json!("other"));
}

#[test]
fn claim_rd_reap_drops_stale_prints_count() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("s1", "d1", t - 1900, t - 1900, &[]),
        ("s2", "d2", t - 5000, t - 5000, &[]),
        ("s3", "d3", t - 2000, t - 2000, &[]),
        ("live", "L", t - 20, t - 20, &[]),
    ]);
    let live_before = sb.leases()["live"].clone();
    let o = sb.ok(&["reap"]);
    assert_eq!(out(&o).trim(), r#"{"reaped": 3}"#);
    let after = sb.leases();
    assert_eq!(after.as_object().unwrap().len(), 1);
    assert_eq!(after["live"], live_before, "live lease must be untouched");

    let o = sb.ok(&["reap"]);
    assert_eq!(out(&o).trim(), r#"{"reaped": 0}"#);
    assert_eq!(sb.leases()["live"], live_before);
}

#[test]
fn claim_rd_ttl_not_configurable() {
    // behavioural: plausible TTL env overrides do not change staleness
    let sb = Sb::new();
    let t = now();
    sb.seed(&[("k", "A", t - 1700, t - 1700, &[])]);
    let mut c = sb.cmd(&["begin", "--key", "k", "--title", "t", "--session", "B"]);
    for v in [
        "OVERWATCH_LEASE_TTL_SECS",
        "OVERWATCH_LEASE_TTL",
        "OVERWATCH_TTL_SECS",
        "OVERWATCH_TTL",
        "OVERWATCH_STALE_SECS",
        "OVERWATCH_HEARTBEAT_TTL",
        "LEASE_TTL_SECS",
    ] {
        c.env(v, "1");
    }
    let o = c.output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(1),
        "TTL override changed staleness: {}",
        dump(&o)
    );

    // structural: no env var read anywhere in src/ is TTL-ish
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut names = Vec::new();
    for e in std::fs::read_dir(&src).unwrap().flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        for chunk in text.split("env::var(").skip(1) {
            if let Some(rest) = chunk.trim_start().strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    names.push(rest[..end].to_string());
                }
            }
        }
    }
    assert!(!names.is_empty(), "scanner found no env::var reads at all");
    for n in &names {
        let u = n.to_uppercase();
        assert!(
            !(u.contains("TTL") || u.contains("STALE") || u.contains("HEARTBEAT")),
            "env var {n} looks like a TTL override"
        );
    }
}

#[test]
fn claim_rd_lease_persists_until_end_reassign_stale() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    // separate processes later: lease is still there
    sb.ok(&["heartbeat", "--key", "k"]);
    sb.ok(&["status", "--json"]);
    sb.ok(&["sessions"]);
    assert!(sb.leases().get("k").is_some());
    let l = sb.ok(&["lease", "--session", "A", "--json"]);
    assert_eq!(json_out(&l)["key"], json!("k"));
    // no overwatch hook ends a session
    let hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap(),
    )
    .unwrap();
    assert!(hooks["hooks"].get("SessionEnd").is_none());
    // end releases it
    sb.ok(&["end", "--key", "k", "--status", "done"]);
    assert!(sb.leases().get("k").is_none());
    // reassign releases it
    sb.ok(&["begin", "--key", "k2", "--title", "t", "--session", "A"]);
    sb.ok(&["reassign", "--key", "k2", "--to", "B"]);
    assert!(sb.leases().get("k2").is_none());
}

// ───────────────────────── end / heartbeat / run / reassign ─────────────────────────

#[test]
fn claim_rd_run_records_heartbeat_event() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[("K", "A", t - 900, t - 600, &[])]);
    sb.ok(&["run", "--key", "K", "--note", "doing-things"]);
    let hb = sb.leases()["K"]["heartbeat_at"].as_i64().unwrap();
    assert!(hb >= t - 5, "heartbeat_at not refreshed: {hb} vs {t}");
    let ev = sb.events();
    let last = ev.last().expect("events.jsonl must have a run event");
    assert_eq!(last["kind"], json!("running"));
    assert_eq!(last["key"], json!("K"));
    assert_eq!(last["status"], json!("doing-things"));
    assert_eq!(last["session_id"], json!("A"));
}

#[test]
fn claim_rd_end_releases_lease_records_status() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "K", "--title", "t", "--session", "A"]);
    let o = sb.ok(&["end", "--key", "K", "--status", "abandoned"]);
    assert!(sb.leases().get("K").is_none());
    assert!(out(&o).contains("released lease K"));
    let ev = sb.events();
    let last = ev.last().unwrap();
    assert_eq!(last["kind"], json!("ended"));
    assert_eq!(last["key"], json!("K"));
    assert_eq!(last["status"], json!("abandoned"));
}

#[test]
fn claim_rd_end_unheld_key_nonzero() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "other", "--title", "t", "--session", "A"]);
    let o = sb.run(&["end", "--key", "never-begun", "--status", "done"]);
    assert_ne!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(err(&o).contains("nothing was released"), "{}", dump(&o));
    assert!(!out(&o).contains("released lease"));
    assert!(sb.leases().get("other").is_some());
}

#[test]
fn claim_rd_heartbeat_refreshes_prints_count() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[("K", "A", t - 900, t - 600, &[])]);
    let o = sb.ok(&["heartbeat", "--key", "K"]);
    assert_eq!(out(&o).trim(), r#"{"refreshed": 1}"#);
    assert!(sb.leases()["K"]["heartbeat_at"].as_i64().unwrap() >= t - 5);
    let o = sb.ok(&["heartbeat", "--key", "nope"]);
    assert_eq!(out(&o).trim(), r#"{"refreshed": 0}"#);
}

#[test]
fn claim_rd_reassign_releases_lease_no_new_lease() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "K", "--title", "t", "--session", "A"]);
    sb.ok(&["begin", "--key", "other", "--title", "t", "--session", "C"]);
    sb.ok(&["reassign", "--key", "K", "--to", "S"]);
    let l = sb.leases();
    assert!(l.get("K").is_none(), "K must be released: {l}");
    assert!(l.get("other").is_some());
    let any_s = l
        .as_object()
        .unwrap()
        .values()
        .any(|x| x["session_id"] == json!("S"));
    assert!(!any_s, "reassign must not create a lease for S: {l}");
    let ev = sb.events();
    let last = ev.last().unwrap();
    assert_eq!(last["key"], json!("K"));
    assert_eq!(last["status"], json!("reassign to=S"));
}

#[test]
fn claim_rd_session_id_resolution() {
    let holder = |sb: &Sb| sb.leases()["k"]["session_id"].as_str().unwrap().to_string();
    // --session wins over the env var
    let sb = Sb::new();
    let mut c = sb.cmd(&[
        "begin",
        "--key",
        "k",
        "--title",
        "t",
        "--session",
        "explicit",
    ]);
    c.env("CLAUDE_CODE_SESSION_ID", "from-env");
    assert!(c.output().unwrap().status.success());
    assert_eq!(holder(&sb), "explicit");
    // env var when no --session
    let sb = Sb::new();
    let mut c = sb.cmd(&["begin", "--key", "k", "--title", "t"]);
    c.env("CLAUDE_CODE_SESSION_ID", "from-env");
    assert!(c.output().unwrap().status.success());
    assert_eq!(holder(&sb), "from-env");
    // neither -> pid-<pid>
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t"]);
    let h = holder(&sb);
    let digits = h
        .strip_prefix("pid-")
        .unwrap_or_else(|| panic!("not pid-<pid>: {h}"));
    assert!(
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
        "{h}"
    );
}

#[test]
fn claim_rd_run_id_resolution() {
    let sb = Sb::new();
    let mut c = sb.cmd(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    c.env("OVERWATCH_RUN_ID", "my-run-77");
    assert!(c.output().unwrap().status.success());
    assert_eq!(sb.leases()["k"]["run_id"], json!("my-run-77"));
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let r = sb.leases()["k"]["run_id"].as_str().unwrap().to_string();
    let ts = r
        .strip_prefix("run-")
        .unwrap_or_else(|| panic!("not run-…: {r}"));
    assert!(ts.parse::<i64>().is_ok(), "run id not timestamp-based: {r}");
}

#[test]
fn claim_rd_binary_no_approval_prompt() {
    // pause/resume/reassign/end/reap complete immediately with stdin closed.
    let sb = Sb::new().with_fakebin();
    let log = fake_condukt(&sb);
    let t = now();
    sb.seed(&[
        ("e", "A", t - 10, t - 10, &[]),
        ("r", "A", t - 10, t - 10, &[]),
        ("dead", "D", t - 2000, t - 2000, &[]),
    ]);
    for (args, label) in [
        (vec!["reap"], "reap"),
        (vec!["end", "--key", "e", "--status", "done"], "end"),
        (vec!["reassign", "--key", "r", "--to", "B"], "reassign"),
        (vec!["pause", "--run", "r1"], "pause"),
        (vec!["resume", "--run", "r1"], "resume"),
    ] {
        let o = sb.run(&args);
        assert!(o.status.success(), "{label}: {}", dump(&o));
        assert!(
            !out(&o).to_lowercase().contains("confirm"),
            "{label}: {}",
            dump(&o)
        );
    }
    let l = sb.leases();
    assert!(
        l.as_object().unwrap().is_empty(),
        "all three leases gone: {l}"
    );
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.contains("state pause --run r1") && calls.contains("state resume --run r1"));
}

#[test]
fn claim_rd_skill_confirms_side_effects_with_askuserquestion() {
    // ONLY the clause "calling the binary directly performs the action immediately".
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("e", "A", t - 10, t - 10, &[]),
        ("r", "A", t - 10, t - 10, &[]),
        ("dead", "D", t - 2000, t - 2000, &[]),
    ]);
    let o = sb.run(&["reap"]);
    assert_eq!(out(&o).trim(), r#"{"reaped": 1}"#, "reap acted immediately");
    assert!(sb.leases().get("dead").is_none());
    let o = sb.run(&["end", "--key", "e", "--status", "done"]);
    assert!(o.status.success(), "{}", dump(&o));
    assert!(sb.leases().get("e").is_none(), "end acted immediately");
    let o = sb.run(&["reassign", "--key", "r", "--to", "B"]);
    assert!(o.status.success(), "{}", dump(&o));
    assert!(sb.leases().get("r").is_none(), "reassign acted immediately");
}

// ───────────────────────── lease / sessions / status views ─────────────────────────

#[test]
fn claim_rd_lease_cmd_prints_live_lease_exit1_silent() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("old", "A", t - 300, t - 10, &[]),
        ("newest", "A", t - 100, t - 10, &[]),
        ("mid", "A", t - 200, t - 10, &[]),
        ("theirs", "Z", t - 5, t - 5, &[]),
    ]);
    let o = sb.ok(&["lease", "--session", "A", "--json"]);
    assert_eq!(json_out(&o)["key"], json!("newest"));
    let o = sb.ok(&["lease", "--session", "A"]);
    assert!(out(&o).starts_with("newest"), "{}", dump(&o));
    // holds none -> exit 1 and empty stdout/stderr
    let o = sb.run(&["lease", "--session", "nobody", "--json"]);
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{}", dump(&o));
    // also with no store at all
    let sb2 = Sb::new();
    let o = sb2.run(&["lease", "--session", "nobody"]);
    assert_eq!(o.status.code(), Some(1), "{}", dump(&o));
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{}", dump(&o));
}

#[test]
fn claim_rd_sessions_roster_live_only() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("a", "live-sess", t - 10, t - 10, &[]),
        ("b", "dead-sess", t - 4000, t - 4000, &[]),
    ]);
    let txt = out(&sb.ok(&["sessions"]));
    assert!(
        txt.contains("live-sess") && !txt.contains("dead-sess"),
        "{txt}"
    );
    let v = json_out(&sb.ok(&["sessions", "--json"]));
    assert_eq!(session_ids(&v), vec!["live-sess".to_string()]);
}

#[test]
fn claim_sk_sessions_list() {
    let sb = Sb::new();
    sb.ok(&[
        "begin",
        "--key",
        "k",
        "--title",
        "t",
        "--session",
        "s-listed",
    ]);
    let txt = out(&sb.ok(&["sessions"]));
    assert!(txt.contains("s-listed"), "{txt}");
    let v = json_out(&sb.ok(&["sessions", "--json"]));
    assert!(session_ids(&v).contains(&"s-listed".to_string()));
}

/// Write the four helper fakes. `origin` is recorded per call (not for --version probes).
#[cfg(unix)]
fn fake_helper(path: &Path, name: &str, origin: &str, stdout: &str) {
    let body = format!(
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && exit 0\necho {origin} >> \"$OW_MARK/{name}.origin\"\nprintf '%s' '{stdout}'\n"
    );
    write_script(path, &body);
}

#[cfg(unix)]
fn cache_bin(home: &Path, name: &str) -> PathBuf {
    home.join(".claude/plugins/cache/yukineko")
        .join(name)
        .join("0.1.0/bin")
        .join(name)
}

#[cfg(unix)]
#[test]
fn claim_rd_sessionstart_injects_status() {
    // The SessionStart hook runs `overwatch status`; that output carries the five views.
    let hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        json!("${CLAUDE_PLUGIN_ROOT}/bin/overwatch status")
    );
    let sb = Sb::new().with_fakebin();
    let fb = sb.fakebin.clone().unwrap();
    std::fs::create_dir_all(sb.root.join("mark")).unwrap();
    fake_helper(
        &fb.join("backlog"),
        "backlog",
        "path",
        r#"[{"status":"pending","priority":"p1"},{"status":"done"}]"#,
    );
    fake_helper(
        &fb.join("hypothesis"),
        "hypothesis",
        "path",
        r#"[{"status":"open"}]"#,
    );
    write_script(
        &fb.join("condukt"),
        "#!/bin/sh\n[ \"$1\" = \"--version\" ] && exit 0\nprintf 'run-7\\t1/3\\tship it\\n'\n",
    );
    fake_helper(&fb.join("compass"), "compass", "path", "THE-GAP-TEXT");
    sb.ok(&[
        "begin",
        "--key",
        "k",
        "--title",
        "work item",
        "--session",
        "sess-live",
    ]);
    let mut c = sb.cmd(&["status"]);
    c.env("OW_MARK", sb.root.join("mark"));
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", dump(&o));
    let txt = out(&o);
    let order = [
        "== Sessions ==",
        "== PDO hypotheses ==",
        "== Backlog ==",
        "== Condukt runs ==",
        "== Compass gap ==",
    ];
    let mut at = 0;
    for h in order {
        let p = txt[at..]
            .find(h)
            .unwrap_or_else(|| panic!("missing/ordered {h} in {txt}"));
        at += p + h.len();
    }
    assert!(txt.contains("session sess-live"), "{txt}");
    assert!(
        txt.contains("pending:  1") && txt.contains("done:     1"),
        "{txt}"
    );
    assert!(txt.contains("open:                 1"), "{txt}");
    assert!(txt.contains("run-7  1/3  ship it"), "{txt}");
    assert!(txt.contains("THE-GAP-TEXT"), "{txt}");
    assert!(
        err(&o).is_empty(),
        "everything observed -> no warning: {}",
        dump(&o)
    );
}

#[cfg(unix)]
#[test]
fn claim_rd_helper_binary_resolution_order() {
    let sb = Sb::new().with_fakebin();
    let mark = sb.root.join("mark");
    std::fs::create_dir_all(&mark).unwrap();
    let fb = sb.fakebin.clone().unwrap();
    // PATH has all four ("path"); the plugin cache has three ("cache").
    for (n, out_) in [
        ("backlog", "[]"),
        ("hypothesis", "[]"),
        ("condukt", ""),
        ("compass", "gap-from-path"),
    ] {
        fake_helper(&fb.join(n), n, "path", out_);
    }
    fake_helper(&cache_bin(&sb.home, "backlog"), "backlog", "cache", "[]");
    fake_helper(
        &cache_bin(&sb.home, "hypothesis"),
        "hypothesis",
        "cache",
        "[]",
    );
    fake_helper(
        &cache_bin(&sb.home, "compass"),
        "compass",
        "cache",
        "gap-from-cache",
    );
    let mut c = sb.cmd(&["status"]);
    c.env("OW_MARK", &mark);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", dump(&o));
    let origin =
        |n: &str| std::fs::read_to_string(mark.join(format!("{n}.origin"))).unwrap_or_default();
    for n in ["backlog", "hypothesis", "compass"] {
        let got = origin(n);
        assert!(
            got.contains("cache") && !got.contains("path"),
            "{n}: cache must win, got {got:?}"
        );
    }
    let got = origin("condukt");
    assert!(
        got.contains("path") && !got.contains("cache"),
        "condukt: PATH fallback, got {got:?}"
    );
    assert!(out(&o).contains("gap-from-cache") && !out(&o).contains("gap-from-path"));
}

#[test]
fn claim_rd_status_warning_on_unreadable_source() {
    let mk = || {
        let sb = Sb::new();
        sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
        std::fs::write(sb.leases_file(), "{ not json").unwrap();
        sb
    };
    let sb = mk();
    let o = sb.run(&["status"]);
    assert!(o.status.success(), "{}", dump(&o));
    let e = err(&o);
    assert!(e.contains("WARNING"), "{}", dump(&o));
    // every undetermined source (from the machine-readable list) is named on stderr
    let sources: Vec<String> = json_out(&mk().ok(&["status", "--json"]))["undetermined"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["source"].as_str().unwrap().to_string())
        .collect();
    assert!(sources.contains(&"sessions".to_string()), "{sources:?}");
    assert!(
        sources.len() >= 2,
        "expected helpers absent too: {sources:?}"
    );
    for s in &sources {
        assert!(e.contains(&format!("[{s}]")), "stderr must name [{s}]: {e}");
    }
    assert!(
        e.contains(&format!("{} status source(s)", sources.len())),
        "{e}"
    );
    assert!(out(&o).contains("(unknown:"), "{}", dump(&o));
}

#[cfg(unix)]
#[test]
fn claim_rd_status_json_undetermined_array() {
    // unreadable source -> top-level array of {source, reason}
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    std::fs::write(sb.leases_file(), "{ not json").unwrap();
    let v = json_out(&sb.ok(&["status", "--json"]));
    let arr = v["undetermined"]
        .as_array()
        .expect("undetermined must be an array");
    assert!(!arr.is_empty());
    for u in arr {
        assert!(u["source"].is_string() && u["reason"].is_string(), "{u}");
    }
    assert!(arr.iter().any(|u| u["source"] == json!("sessions")));
    // all sources observed -> the key is absent
    let sb = Sb::new().with_fakebin();
    let fb = sb.fakebin.clone().unwrap();
    std::fs::create_dir_all(sb.root.join("mark")).unwrap();
    for (n, o_) in [
        ("backlog", "[]"),
        ("hypothesis", "[]"),
        ("condukt", ""),
        ("compass", "gap"),
    ] {
        fake_helper(&fb.join(n), n, "path", o_);
    }
    let mut c = sb.cmd(&["status", "--json"]);
    c.env("OW_MARK", sb.root.join("mark"));
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", dump(&o));
    let v = json_out(&o);
    assert!(
        v.get("undetermined").is_none(),
        "must be omitted entirely: {v}"
    );
}

#[test]
fn claim_rd_hooks_json_sessionstart_only_status_hook() {
    let hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap(),
    )
    .unwrap();
    let events = hooks["hooks"].as_object().unwrap();
    let mut names: Vec<&String> = events.keys().collect();
    names.sort();
    assert!(
        !events.contains_key("Stop"),
        "`overwatch status` must NOT be registered on Stop (25KB dump per Stop is noise); got events {names:?}"
    );
    assert_eq!(names, ["SessionStart"]);
    {
        let ev = "SessionStart";
        let groups = events[ev].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{ev}");
        let hs = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(hs.len(), 1, "{ev}");
        assert_eq!(hs[0]["type"], json!("command"));
        assert_eq!(
            hs[0]["command"],
            json!("${CLAUDE_PLUGIN_ROOT}/bin/overwatch status")
        );
        assert_eq!(hs[0]["timeout"], json!(5));
    }
}

#[test]
fn claim_rd_no_install_subcommand() {
    let sb = Sb::new();
    let o = sb.run(&["install"]);
    assert_eq!(o.status.code(), Some(2), "{}", dump(&o));
    assert!(
        err(&o).contains("unrecognized subcommand 'install'"),
        "{}",
        dump(&o)
    );
    // control: the same invocation shape works for a real subcommand
    assert!(sb.run(&["reap"]).status.success());
}

// ───────────────────────── advisory begin summary ─────────────────────────

#[test]
fn claim_rd_begin_prints_advisory_summary() {
    let sb = Sb::new();
    let o = sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let v = json_out(&o);
    let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "possible_duplicate",
            "scope_declared",
            "scope_overlap",
            "scope_undeclared"
        ]
    );
    assert!(v["scope_overlap"].is_array());
    assert!(v["scope_undeclared"].is_array());
    assert!(v["scope_declared"].is_boolean());
    assert!(v["possible_duplicate"].is_array());
}

#[test]
fn claim_rd_scope_overlap_lists_live_overlapping_peers() {
    let sb = Sb::new();
    let t = now();
    let mut m = serde_json::Map::new();
    sb.seed_peer(&mut m, "overlapping", "alpha one", &["crates/a/**"], t - 10);
    sb.seed_peer(&mut m, "disjoint", "bravo two", &["docs/**"], t - 10);
    sb.seed_peer(
        &mut m,
        "stale-overlap",
        "charlie three",
        &["crates/a/**"],
        t - 3000,
    );
    sb.write_leases_map(m);
    let o = sb.ok(&[
        "begin",
        "--key",
        "me",
        "--title",
        "zulu",
        "--session",
        "ME",
        "--scope",
        "crates/a/src/**",
    ]);
    let v = json_out(&o);
    let keys: Vec<&str> = v["scope_overlap"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["overlapping"], "{v}");
    assert_eq!(v["scope_overlap"][0]["scope"], json!(["crates/a/**"]));
    // disjoint declared scope: in neither list
    assert!(
        !v["scope_undeclared"].to_string().contains("disjoint"),
        "{v}"
    );
    assert!(!v.to_string().contains("stale-overlap"), "{v}");
}

#[test]
fn claim_rd_scope_undeclared_lists_undetermined_peers() {
    let sb = Sb::new();
    let t = now();
    let mut m = serde_json::Map::new();
    sb.seed_peer(&mut m, "no-scope-peer", "alpha one", &[], t - 10);
    sb.seed_peer(
        &mut m,
        "declared-disjoint",
        "bravo two",
        &["docs/**"],
        t - 10,
    );
    sb.write_leases_map(m);
    // this lease declares a scope; the peer without one is undetermined
    let o = sb.ok(&[
        "begin",
        "--key",
        "me",
        "--title",
        "zulu",
        "--session",
        "ME",
        "--scope",
        "crates/a/**",
    ]);
    let v = json_out(&o);
    let und: Vec<&str> = v["scope_undeclared"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["key"].as_str().unwrap())
        .collect();
    assert_eq!(und, vec!["no-scope-peer"], "{v}");
    assert!(v["scope_overlap"].as_array().unwrap().is_empty(), "{v}");
    assert!(
        !v.to_string().contains("declared-disjoint"),
        "peer in neither list: {v}"
    );
    // self lacking scope -> declared-scope peer also lands in scope_undeclared (not overlap)
    let sb = Sb::new();
    let mut m = serde_json::Map::new();
    sb.seed_peer(&mut m, "declared-peer", "bravo two", &["docs/**"], t - 10);
    sb.write_leases_map(m);
    let v = json_out(&sb.ok(&["begin", "--key", "me", "--title", "zulu", "--session", "ME"]));
    assert_eq!(v["scope_undeclared"][0]["key"], json!("declared-peer"));
    assert!(v["scope_overlap"].as_array().unwrap().is_empty());
}

#[test]
fn claim_rd_scope_declared_false_lists_all_peers_undeclared() {
    let sb = Sb::new();
    let t = now();
    let mut m = serde_json::Map::new();
    sb.seed_peer(&mut m, "p-with-scope", "alpha one", &["docs/**"], t - 10);
    sb.seed_peer(&mut m, "p-without", "bravo two", &[], t - 10);
    sb.write_leases_map(m);
    let v = json_out(&sb.ok(&["begin", "--key", "me", "--title", "zulu", "--session", "ME"]));
    assert_eq!(v["scope_declared"], json!(false));
    let mut und: Vec<&str> = v["scope_undeclared"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["key"].as_str().unwrap())
        .collect();
    und.sort();
    assert_eq!(und, vec!["p-with-scope", "p-without"], "{v}");
    assert!(v["scope_overlap"].as_array().unwrap().is_empty());
    // control: with --scope the flag flips to true
    let sb2 = Sb::new();
    let v2 = json_out(&sb2.ok(&[
        "begin",
        "--key",
        "me",
        "--title",
        "zulu",
        "--session",
        "ME",
        "--scope",
        "x/**",
    ]));
    assert_eq!(v2["scope_declared"], json!(true));
}

#[test]
fn claim_rd_possible_duplicate_threshold() {
    let seed = |sb: &Sb| {
        let t = now();
        let mut m = serde_json::Map::new();
        sb.seed_peer(
            &mut m,
            "peer",
            "alpha beta gamma delta epsilon",
            &[],
            t - 10,
        );
        sb.write_leases_map(m);
    };
    let me = |sb: &Sb, thr: Option<&str>| {
        let mut c = sb.cmd(&[
            "begin",
            "--key",
            "me",
            "--title",
            "alpha beta gamma delta zeta",
            "--session",
            "ME",
        ]);
        if let Some(t) = thr {
            c.env("OVERWATCH_DUP_THRESHOLD", t);
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", dump(&o));
        json_out(&o)
    };
    // default 0.6: Jaccard of the two titles is 4/6 ≈ 0.667 -> listed with similarity
    let sb = Sb::new();
    seed(&sb);
    let v = me(&sb, None);
    let dup = v["possible_duplicate"].as_array().unwrap();
    assert_eq!(dup.len(), 1, "{v}");
    assert_eq!(dup[0]["key"], json!("peer"));
    let sim = dup[0]["similarity"].as_f64().unwrap();
    assert!((sim - 4.0 / 6.0).abs() < 1e-9, "similarity {sim}");
    // override above the similarity -> not listed
    let sb = Sb::new();
    seed(&sb);
    assert!(me(&sb, Some("0.9"))["possible_duplicate"]
        .as_array()
        .unwrap()
        .is_empty());
    // override below -> listed
    let sb = Sb::new();
    seed(&sb);
    assert_eq!(
        me(&sb, Some("0.5"))["possible_duplicate"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // dissimilar title is never listed at the default
    let sb = Sb::new();
    seed(&sb);
    let o = sb.ok(&[
        "begin",
        "--key",
        "me",
        "--title",
        "theta iota kappa lambda",
        "--session",
        "ME",
    ]);
    assert!(json_out(&o)["possible_duplicate"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn claim_rd_begin_lists_never_change_exit_code() {
    let sb = Sb::new();
    let o = sb.run(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_out(&o);
    assert_eq!(v["scope_overlap"], json!([]));
    assert_eq!(v["scope_undeclared"], json!([]));
    assert_eq!(v["possible_duplicate"], json!([]));
    // all three lists populated at once -> still exit 0
    let t = now();
    let mut m = serde_json::Map::new();
    sb.seed_peer(&mut m, "overlap", "same title words here", &["a/**"], t - 5);
    sb.seed_peer(&mut m, "undecl", "same title words here", &[], t - 5);
    sb.write_leases_map(m);
    let o = sb.run(&[
        "begin",
        "--key",
        "me",
        "--title",
        "same title words here",
        "--session",
        "ME",
        "--scope",
        "a/x/**",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    let v = json_out(&o);
    assert!(!v["scope_overlap"].as_array().unwrap().is_empty());
    assert!(!v["scope_undeclared"].as_array().unwrap().is_empty());
    assert!(!v["possible_duplicate"].as_array().unwrap().is_empty());
}

// ───────────────────────── storage layout ─────────────────────────

fn walk(dir: &Path, out_: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out_);
            } else {
                out_.push(p);
            }
        }
    }
}

#[test]
fn claim_rd_state_dir_layout() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let dir = sb.store_dir();
    assert!(
        dir.join("leases.json").is_file(),
        "missing {}",
        dir.display()
    );
    assert!(dir.join("events.jsonl").is_file());
    // exactly one project dir under ~/.overwatch
    let n = std::fs::read_dir(sb.home.join(".overwatch"))
        .unwrap()
        .count();
    assert_eq!(n, 1);
    // the key is <basename>-<8 hex>
    let key = dir
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        key.starts_with("proj-") && key.len() == "proj-".len() + 8,
        "{key}"
    );
}

#[test]
fn claim_sk_store_path() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let mut files = Vec::new();
    walk(&sb.root, &mut files);
    let hits = |name: &str| -> Vec<PathBuf> {
        files
            .iter()
            .filter(|p| p.file_name().is_some_and(|n| n == name))
            .cloned()
            .collect()
    };
    let dir = sb.store_dir();
    assert_eq!(hits("leases.json"), vec![dir.join("leases.json")]);
    assert_eq!(hits("events.jsonl"), vec![dir.join("events.jsonl")]);
    assert!(dir.starts_with(sb.home.join(".overwatch")));
}

#[test]
fn claim_sk_events_append_only() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k1", "--title", "t", "--session", "A"]);
    let b1 = std::fs::read(sb.events_file()).unwrap();
    sb.ok(&["begin", "--key", "k2", "--title", "t", "--session", "A"]);
    let b2 = std::fs::read(sb.events_file()).unwrap();
    assert!(b2.len() > b1.len());
    assert_eq!(
        &b2[..b1.len()],
        &b1[..],
        "prior lines must be byte-identical"
    );
    sb.ok(&["run", "--key", "k1", "--note", "n"]);
    sb.ok(&["end", "--key", "k2", "--status", "done"]);
    let b3 = std::fs::read(sb.events_file()).unwrap();
    assert!(b3.len() > b2.len());
    assert_eq!(&b3[..b2.len()], &b2[..]);
    assert_eq!(b3.iter().filter(|b| **b == b'\n').count(), 4);
}

#[test]
fn claim_rd_main_worktree_unresolvable_fails() {
    // NOTE: only begin/lease/end are asserted to fail. heartbeat/run/reap/reassign
    // exit 0 on this condition (e.g. `{"refreshed": 0}`), which is NOT pinned here
    // (it would be pinning an undetermined-reported-as-zero); see the report.
    let sb = Sb::new();
    std::fs::remove_dir_all(sb.proj.join(".git")).unwrap();
    std::fs::write(sb.proj.join(".git"), "not a gitdir line\n").unwrap();
    for args in [
        vec!["begin", "--key", "k", "--title", "t", "--session", "A"],
        vec!["lease", "--session", "A"],
        vec!["end", "--key", "k", "--status", "done"],
    ] {
        let o = sb.run(&args);
        assert_ne!(o.status.code(), Some(0), "{args:?} must fail: {}", dump(&o));
        assert!(
            err(&o).contains("undetermined") || err(&o).contains("unparseable"),
            "{args:?}: {}",
            dump(&o)
        );
    }
    assert!(
        !sb.home.join(".overwatch").exists(),
        "no per-worktree store may be created"
    );
}

// ───────────────────────── contention (≈10s each: bounded lock wait) ─────────────────────────

#[test]
fn claim_sk_end_nonzero_when_not_released() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "held", "--title", "t", "--session", "A"]);
    // never-begun
    let o = sb.run(&["end", "--key", "never", "--status", "done"]);
    assert_ne!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(!out(&o).contains("released lease"));
    // already-ended
    sb.ok(&["end", "--key", "held", "--status", "done"]);
    let o = sb.run(&["end", "--key", "held", "--status", "done"]);
    assert_ne!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(!out(&o).contains("released lease"));
    // reaped-as-stale
    let t = now();
    sb.seed(&[("old", "D", t - 3000, t - 3000, &[])]);
    let o = sb.run(&["end", "--key", "old", "--status", "done"]);
    assert_ne!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(!out(&o).contains("released lease"));
    // lock unobtainable -> non-zero, lease untouched
    sb.seed(&[("locked", "A", t - 10, t - 10, &[])]);
    let lock = sb.hold_lock();
    let o = sb.run(&["end", "--key", "locked", "--status", "done"]);
    let _ = std::fs::remove_file(&lock);
    assert_ne!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(!out(&o).contains("released lease"), "{}", dump(&o));
    assert!(sb.leases().get("locked").is_some());
}

#[test]
fn claim_sk_end_release_message() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "K", "--title", "t", "--session", "A"]);
    let o = sb.run(&["end", "--key", "K", "--status", "done"]);
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    assert!(out(&o).contains("released lease K"), "{}", dump(&o));
}

#[test]
fn claim_sk_reap_removes_stale() {
    let sb = Sb::new();
    let t = now();
    sb.seed(&[
        ("old", "D", t - 1900, t - 1900, &[]),
        ("fresh", "F", t - 100, t - 100, &[]),
    ]);
    let fresh_before = sb.leases()["fresh"].clone();
    sb.ok(&["reap"]);
    let l = sb.leases();
    assert!(l.get("old").is_none());
    assert_eq!(l["fresh"], fresh_before);
}

#[test]
fn claim_sk_reassign_changes_holder() {
    // SKILL.md: reassign RELEASES the lease, records a `reassign to=<sid>`
    // event, and creates no lease for <sid>. (An earlier SKILL.md sentence
    // said it "changes the holder"; that was observed false and corrected.)
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "K", "--title", "t", "--session", "A"]);
    let o = sb.run(&["reassign", "--key", "K", "--to", "B"]);
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    let l = sb.leases();
    assert!(
        l.get("K").is_none(),
        "actual: K is released, not handed to B: {l}"
    );
    assert!(
        !l.as_object()
            .unwrap()
            .values()
            .any(|x| x["session_id"] == json!("B")),
        "actual: no lease for B: {l}"
    );
    assert_eq!(
        sb.events().last().unwrap()["status"],
        json!("reassign to=B")
    );
}

#[cfg(unix)]
#[test]
fn claim_rd_pause_resume_delegate_condukt() {
    let sb = Sb::new().with_fakebin();
    let log = fake_condukt(&sb);
    sb.ok(&["pause", "--run", "run-abc"]);
    sb.ok(&["resume", "--run", "run-abc"]);
    let calls: Vec<String> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        calls,
        ["state pause --run run-abc", "state resume --run run-abc"]
    );
    let ev = sb.events();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0]["key"], json!("run-abc"));
    assert_eq!(ev[0]["status"], json!("pause ok=true"));
    assert_eq!(ev[1]["status"], json!("resume ok=true"));
}

// ───────────────────────── violations ─────────────────────────

fn rec(sb: &Sb, disc: &str, task: &str, session: &str) -> Output {
    sb.ok(&[
        "record-violation",
        "--source",
        "blastguard",
        "--discriminator",
        disc,
        "--task",
        task,
        "--session",
        session,
    ])
}

fn violation_line(sig: &str, task: &str, session: &str, ts: i64) -> String {
    json!({"source":"blastguard","signature":sig,"task_key":task,"session_id":session,"ts":ts})
        .to_string()
}

fn seed_violations(sb: &Sb, lines: &[String]) {
    std::fs::create_dir_all(sb.store_dir()).unwrap();
    std::fs::write(
        sb.store_dir().join("violations.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn claim_rd_record_violation_and_systemic_escalation() {
    let sb = Sb::new();
    // normalized signature: trimmed, lower-cased, spaces -> '-'
    let o = rec(&sb, "  Rule X ", "t1", "s1");
    assert_eq!(json_out(&o)["signature"], json!("blastguard:rule-x"));
    rec(&sb, "rule x", "t2", "s2");
    // below threshold (2 < 3): not systemic
    let e = out(&sb.ok(&["escalations"]));
    assert!(e.contains("(no systemic issues)"), "{e}");
    rec(&sb, "RULE X", "t3", "s3");
    // 3 occurrences across >1 task/session: escalated as systemic
    let v = json_out(&sb.ok(&["escalations", "--json"]));
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "{v}");
    assert_eq!(arr[0]["signature"], json!("blastguard:rule-x"));
    assert_eq!(arr[0]["occurrences"], json!(3));
    assert_eq!(arr[0]["is_systemic"], json!(true));
    // one task retrying the same gate is NOT systemic
    let sb2 = Sb::new();
    for _ in 0..4 {
        rec(&sb2, "retry-loop", "only-task", "only-session");
    }
    assert!(json_out(&sb2.ok(&["escalations", "--json"]))
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn claim_rd_violations_cli_prints_all_signatures() {
    let sb = Sb::new();
    for i in 0..3 {
        rec(&sb, "systemic-one", &format!("t{i}"), &format!("s{i}"));
    }
    rec(&sb, "lonely", "t0", "s0");
    let v = json_out(&sb.ok(&["violations", "--json"]));
    let mut sigs: Vec<(String, bool)> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["signature"].as_str().unwrap().to_string(),
                r["is_systemic"].as_bool().unwrap(),
            )
        })
        .collect();
    sigs.sort();
    assert_eq!(
        sigs,
        vec![
            ("blastguard:lonely".to_string(), false),
            ("blastguard:systemic-one".to_string(), true)
        ]
    );
    let txt = out(&sb.ok(&["violations"]));
    assert!(
        txt.contains("blastguard:lonely") && txt.contains("blastguard:systemic-one"),
        "{txt}"
    );
}

#[test]
fn claim_rd_escalations_cli_only_systemic() {
    let sb = Sb::new();
    for i in 0..3 {
        rec(&sb, "systemic-one", &format!("t{i}"), &format!("s{i}"));
    }
    rec(&sb, "lonely", "t0", "s0");
    let v = json_out(&sb.ok(&["escalations", "--json"]));
    let sigs: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["signature"].as_str().unwrap())
        .collect();
    assert_eq!(sigs, vec!["blastguard:systemic-one"]);
    let txt = out(&sb.ok(&["escalations"]));
    assert!(
        txt.contains("blastguard:systemic-one") && !txt.contains("lonely"),
        "{txt}"
    );
}

#[test]
fn claim_rd_recurrence_default_policy_and_overrides() {
    let t = now();
    let three = |ts: i64| -> Vec<String> {
        (0..3)
            .map(|i| violation_line("blastguard:sig", &format!("t{i}"), &format!("s{i}"), ts))
            .collect()
    };
    let two = |ts: i64| -> Vec<String> { three(ts).into_iter().take(2).collect() };
    let systemic = |o: &Output| -> Vec<bool> {
        json_out(o)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["is_systemic"].as_bool().unwrap())
            .collect()
    };
    // default threshold 3: 3 events systemic, 2 not
    let sb = Sb::new();
    seed_violations(&sb, &three(t - 100));
    assert_eq!(systemic(&sb.ok(&["violations", "--json"])), vec![true]);
    seed_violations(&sb, &two(t - 100));
    assert_eq!(systemic(&sb.ok(&["violations", "--json"])), vec![false]);
    // --threshold overrides per call (both directions)
    assert_eq!(
        systemic(&sb.ok(&["violations", "--json", "--threshold", "2"])),
        vec![true]
    );
    seed_violations(&sb, &three(t - 100));
    assert_eq!(
        systemic(&sb.ok(&["violations", "--json", "--threshold", "4"])),
        vec![false]
    );
    // default window 86400: inside (80000s old) counted, outside (90000s old) dropped
    seed_violations(&sb, &three(t - 80_000));
    assert_eq!(systemic(&sb.ok(&["violations", "--json"])), vec![true]);
    seed_violations(&sb, &three(t - 90_000));
    assert!(json_out(&sb.ok(&["violations", "--json"]))
        .as_array()
        .unwrap()
        .is_empty());
    // --window-secs overrides: widen brings them back, narrow drops the 80000s ones
    assert_eq!(
        systemic(&sb.ok(&["violations", "--json", "--window-secs", "100000"])),
        vec![true]
    );
    seed_violations(&sb, &three(t - 80_000));
    assert!(
        json_out(&sb.ok(&["violations", "--json", "--window-secs", "3600"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claim_rd_violations_jsonl_separate_stream() {
    let sb = Sb::new();
    sb.ok(&["begin", "--key", "k", "--title", "t", "--session", "A"]);
    let ev_before = std::fs::read(sb.events_file()).unwrap();
    rec(&sb, "some-rule", "t1", "s1");
    let vio = std::fs::read_to_string(sb.store_dir().join("violations.jsonl")).unwrap();
    assert_eq!(vio.lines().count(), 1);
    assert!(vio.contains("blastguard:some-rule"));
    assert_eq!(
        std::fs::read(sb.events_file()).unwrap(),
        ev_before,
        "events.jsonl must be untouched"
    );
    // and with no events.jsonl yet, recording does not create one
    let sb2 = Sb::new();
    rec(&sb2, "some-rule", "t1", "s1");
    assert!(sb2.store_dir().join("violations.jsonl").is_file());
    assert!(!sb2.events_file().exists());
}

// ───────────────────────── CLI surface ─────────────────────────

fn variants_in_main_rs() -> Vec<String> {
    let src =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
    let mut inside = false;
    let mut names = Vec::new();
    for line in src.lines() {
        if line.starts_with("enum Command {") {
            inside = true;
            continue;
        }
        if inside && line == "}" {
            break;
        }
        if inside && line.starts_with("    ") && !line.starts_with("     ") {
            let ident: String = line
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if ident.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                let mut k = String::new();
                for (i, c) in ident.chars().enumerate() {
                    if c.is_ascii_uppercase() && i > 0 {
                        k.push('-');
                    }
                    k.push(c.to_ascii_lowercase());
                }
                names.push(k);
            }
        }
    }
    names
}

#[test]
fn claim_rd_help_lists_every_subcommand() {
    let sb = Sb::new();
    let o = sb.ok(&["--help"]);
    let help = out(&o);
    let names = variants_in_main_rs();
    assert!(names.len() >= 30, "variant scanner broken: {names:?}");
    let cmds: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .collect();
    for n in &names {
        let line = cmds
            .iter()
            .find(|l| l.split_whitespace().next() == Some(n.as_str()))
            .unwrap_or_else(|| panic!("--help does not list `{n}`:\n{help}"));
        let desc = line.trim_start().strip_prefix(n.as_str()).unwrap().trim();
        assert!(!desc.is_empty(), "`{n}` has no description line: {line:?}");
    }
}

#[test]
fn claim_rd_canary_commands_behaviour() {
    let sb = Sb::new();
    // canary-plan: pure stage split (by size, by count), executes nothing
    let v = json_out(&sb.ok(&["canary-plan", "--plugins", "a,b,c,d", "--stage-size", "2"]));
    assert_eq!(
        v["stages"],
        json!([{"index":0,"plugins":["a","b"]},{"index":1,"plugins":["c","d"]}])
    );
    let v = json_out(&sb.ok(&["canary-plan", "--plugins", "a,b,c,d", "--stage-count", "3"]));
    let sizes: Vec<usize> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["plugins"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, vec![2, 1, 1]);
    // canary-gate: exit 3 + rollback JSON, exit 0 + proceed JSON
    let o = sb.run(&["canary-gate", "--observed-violations", "5"]);
    assert_eq!(o.status.code(), Some(3), "{}", dump(&o));
    assert_eq!(json_out(&o)["decision"], json!("rollback"));
    let o = sb.run(&["canary-gate", "--observed-violations", "0"]);
    assert_eq!(o.status.code(), Some(0), "{}", dump(&o));
    assert_eq!(json_out(&o)["decision"], json!("proceed"));
    // pure modes touched no store
    assert!(!sb.home.join(".overwatch").exists());
    // canary-rollback-plan prints the restore plan
    let o = sb.ok(&[
        "canary-rollback-plan",
        "--prior",
        r#"[{"name":"a","prior_version":"1","prior_install_path":"/p"}]"#,
        "--canary-targets",
        r#"[{"name":"a","canary_version":"2","canary_install_path":"/q"}]"#,
    ]);
    let t = &json_out(&o)["targets"][0];
    assert_eq!(t["name"], json!("a"));
    assert_eq!(t["restore_install_path"], json!("/p"));
    assert_eq!(t["canary_install_path"], json!("/q"));
    // record-rollback appends a rollback event
    sb.ok(&[
        "record-rollback",
        "--plugin",
        "a",
        "--from-version",
        "1",
        "--to-version",
        "2",
        "--stage",
        "0",
    ]);
    let log = std::fs::read_to_string(sb.store_dir().join("rollbacks.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);
    let ev: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(ev["plugin"], json!("a"));
    assert_eq!(ev["to_version"], json!("2"));
    assert_eq!(ev["from_version"], json!("1"));
}

#[test]
fn claim_rd_continuous_audit_commands() {
    let sb = Sb::new();
    let o = sb.ok(&[
        "audit-round",
        "record",
        "--round",
        "r1",
        "--target",
        "x",
        "--new-findings",
        "3",
        "--confirmed",
        "2",
        "--regression-tests-added",
        "2",
    ]);
    assert_eq!(json_out(&o)["recorded"], json!(true));
    let o = sb.ok(&["audit-round", "close", "--round", "r1", "--tests", "1"]);
    assert_eq!(json_out(&o)["closed"], json!(true));
    let m = json_out(&sb.ok(&["audit-metrics", "--json"]));
    assert_eq!(m["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(
        m["rounds"][0]["regression_tests_added"],
        json!(1),
        "close SETs the count"
    );
    // default window is 3; --window overrides
    assert_eq!(m["convergence_window"], json!(3));
    let m5 = json_out(&sb.ok(&["audit-metrics", "--json", "--window", "5"]));
    assert_eq!(m5["convergence_window"], json!(5));
}

#[test]
fn claim_rd_telemetry_commands() {
    let sb = Sb::new();
    // undetermined-metrics --window-days N
    let v = json_out(&sb.ok(&["undetermined-metrics", "--json", "--window-days", "7"]));
    assert_eq!(v["window_days"], json!(7));
    assert!(json_out(&sb.ok(&["undetermined-metrics", "--json"]))["window_days"].is_null());
    // gate-outcomes aggregates ~/.<gate>/state/log.jsonl for the three gates
    let seed = |gate: &str, lines: &[&str]| {
        let p = sb.home.join(format!(".{gate}/state/log.jsonl"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    };
    seed(
        "donegate",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"green","ts":2}"#,
        ],
    );
    seed(
        "tdd",
        &[
            r#"{"session":"s1","verdict":"blocked","ts":1}"#,
            r#"{"session":"s1","verdict":"ok","ts":2}"#,
            r#"{"session":"s2","verdict":"blocked","ts":3}"#,
            r#"{"session":"s2","verdict":"ok","ts":4}"#,
        ],
    );
    let v = json_out(&sb.ok(&["gate-outcomes", "--json"]));
    let gates = v["gates"].as_array().unwrap();
    let names: Vec<&str> = gates.iter().map(|g| g["gate"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["donegate", "tdd", "reviewgate"]);
    let firings: Vec<i64> = gates
        .iter()
        .map(|g| g["firings"].as_i64().unwrap())
        .collect();
    assert_eq!(firings, vec![1, 2, 0]);
    for g in gates {
        let name = g["gate"].as_str().unwrap();
        assert!(
            g["log"]
                .as_str()
                .unwrap()
                .ends_with(&format!(".{name}/state/log.jsonl")),
            "{g}"
        );
    }
}

#[test]
fn claim_rd_review_surface_defaults() {
    let sb = Sb::new();
    // every listed subcommand exists and accepts the listed flags
    let table: &[(&str, &[&str])] = &[
        ("review-queue", &["--json", "--since", "--limit"]),
        ("record-finding", &[]),
        ("record-disposition", &[]),
        ("review-metrics", &[]),
        ("compact-findings", &[]),
        ("resolve-merge-conflict", &[]),
        (
            "reconcile-fixed",
            &["--since-ref", "--range", "--last-n", "--dry-run"],
        ),
        ("auto-approved", &["--since", "--sample", "--seed"]),
    ];
    for (sub, flags) in table {
        let o = sb.run(&[sub, "--help"]);
        assert_eq!(o.status.code(), Some(0), "{sub} --help: {}", dump(&o));
        for f in *flags {
            assert!(out(&o).contains(f), "{sub} --help lacks {f}:\n{}", out(&o));
        }
    }

    // reconcile-fixed with no range flag scans the last 50 commits
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .current_dir(&sb.proj)
            .env("GIT_AUTHOR_NAME", "a")
            .env("GIT_AUTHOR_EMAIL", "a@a")
            .env("GIT_COMMITTER_NAME", "a")
            .env("GIT_COMMITTER_EMAIL", "a@a")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    for f in ["CA-foo-001", "CA-foo-002"] {
        sb.ok(&[
            "record-finding",
            "--finding-id",
            f,
            "--source",
            "t",
            "--summary",
            "s",
            "--verdict",
            "confirmed",
        ]);
    }
    git(&["commit", "-q", "--allow-empty", "-m", "fix CA-foo-001"]);
    for i in 0..50 {
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("filler {i}"),
        ]);
    }
    git(&["commit", "-q", "--allow-empty", "-m", "fix CA-foo-002"]);
    // 52 commits total: the default window (50) sees CA-foo-002 but not CA-foo-001
    let v = json_out(&sb.ok(&["reconcile-fixed", "--json", "--dry-run"]));
    assert_eq!(v["commits_scanned"], json!(50), "{v}");
    assert_eq!(v["reconciled"], json!(["CA-foo-002"]), "{v}");
    // control: an explicit wider window reaches the 52nd commit
    let v = json_out(&sb.ok(&["reconcile-fixed", "--json", "--dry-run", "--last-n", "100"]));
    assert_eq!(v["commits_scanned"], json!(52), "{v}");
    let mut r: Vec<&str> = v["reconciled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    r.sort();
    assert_eq!(r, vec!["CA-foo-001", "CA-foo-002"]);

    // auto-approved: default sample 5, seed 0
    let sb2 = Sb::new();
    let p = sb2.home.join(".condukt/state/gate-decisions.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let rows: Vec<String> = (0..9)
        .map(|i| format!(r#"{{"question":"Q{i}","options":["a","b"],"recommend_index":0,"chosen":"a","policy":"auto","created_at":{i}}}"#))
        .collect();
    std::fs::write(&p, rows.join("\n") + "\n").unwrap();
    let v = json_out(&sb2.ok(&["auto-approved", "--json"]));
    assert_eq!(v["count"], json!(9));
    assert_eq!(v["sample_size"], json!(5), "{v}");
    assert_eq!(v["seed"], json!(0), "{v}");
    let v = json_out(&sb2.ok(&["auto-approved", "--json", "--sample", "2", "--seed", "7"]));
    assert_eq!(v["sample_size"], json!(2));
    assert_eq!(v["seed"], json!(7));
}

#[test]
fn claim_rd_license_mit() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let own: toml::Value =
        toml::from_str(&std::fs::read_to_string(manifest.join("Cargo.toml")).unwrap()).unwrap();
    assert_eq!(
        own["package"]["license"]["workspace"],
        toml::Value::Boolean(true)
    );
    let ws: toml::Value =
        toml::from_str(&std::fs::read_to_string(manifest.join("../../Cargo.toml")).unwrap())
            .unwrap();
    assert_eq!(ws["workspace"]["package"]["license"].as_str(), Some("MIT"));
}
