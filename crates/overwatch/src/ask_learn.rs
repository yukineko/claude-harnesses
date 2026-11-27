//! `overwatch ask-learn` — the learner behind blastguard's Ask learning
//! (spec `docs/specs/blastguard-ask-learning.md`, user ruling 2026-11-28).
//!
//! It turns the interactive Asks a human really answered into
//! `~/.blastguard/learned/table.json`, which blastguard READS. The learner is
//! not a verdict, but its output feeds one, so it fails closed and visibly:
//! on any failure the previous table is kept untouched, the error is written
//! to `status.json`, and the run exits non-zero (in `--hook` mode: exit 0 and
//! a `systemMessage`, because a Stop hook must never block the stop).
//!
//! Pipeline: `blocks.jsonl` interactive Ask records (UUID sessions only) ->
//! the shape recomputed by `blastguard shape` with the CURRENT analyser (an
//! Ask the analyser no longer asks is skipped) -> the outcome joined from the
//! session transcript (`tool_use` with equal tool name + input; its
//! `tool_result` containing Claude Code's rejection text is a decline, any
//! other result an approval, no match yet = pending) -> `asklearn::learn`.
//!
//! Incremental: `state.json` keeps the byte offset into the block log, the
//! unresolved (pending) records, the resolved outcomes and the claimed
//! `tool_use` ids. `--full` (or a different blastguard version, a shrunk log)
//! rebuilds from nothing; both give a byte-identical table.

use harness_core::asklearn::{self, learn, Outcome, OutcomeRecord, Shape};
use harness_core::boundary;
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DECLINE_TEXT: &str = "The user doesn't want to proceed with this tool use";
/// An Ask with no matching transcript result for this long is dropped.
const PENDING_MAX_AGE_SECS: u64 = 30 * 24 * 3600;
const STATE_VERSION: u32 = 1;
const SHAPE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    session_id: String,
    ts: u64,
    tool_name: String,
    tool_input: Value,
    shape: String,
    system_harm: bool,
    sample: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resolved {
    session_id: String,
    ts: u64,
    shape: String,
    approved: bool,
    sample: String,
    system_harm: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    version: u32,
    blastguard_version: String,
    offset: u64,
    claimed: BTreeSet<String>,
    pending: Vec<Pending>,
    resolved: Vec<Resolved>,
}

#[derive(Debug, Default)]
struct Opts {
    blocks: Option<PathBuf>,
    projects: Option<PathBuf>,
    table: Option<PathBuf>,
    blastguard: Option<PathBuf>,
    full: bool,
    hook: bool,
    budget: Option<Duration>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--blocks" => o.blocks = Some(PathBuf::from(val(a)?)),
            "--projects" => o.projects = Some(PathBuf::from(val(a)?)),
            "--table" => o.table = Some(PathBuf::from(val(a)?)),
            "--blastguard" => o.blastguard = Some(PathBuf::from(val(a)?)),
            "--budget-secs" => {
                let v = val(a)?;
                let n: u64 = v.parse().map_err(|_| format!("bad --budget-secs {v:?}"))?;
                o.budget = Some(Duration::from_secs(n));
            }
            "--full" => o.full = true,
            "--hook" => o.hook = true,
            other => return Err(format!("unknown option `{other}`")),
        }
        i += 1;
    }
    Ok(o)
}

/// Entry point for `overwatch ask-learn [...]`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("file-reds") {
        return match file_reds(&args[1..]) {
            Ok(n) => {
                println!("ask-learn file-reds: filed {n} promotion(s)");
                0
            }
            Err(e) => {
                eprintln!("overwatch ask-learn file-reds: {e}");
                1
            }
        };
    }
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("overwatch ask-learn: {e}");
            return 2;
        }
    };
    let status_path = asklearn::learned_dir().join("status.json");
    let result = learn_run(&opts);
    let (status, code, message) = match &result {
        Ok(Run::Done(s)) => (
            json!({"ok": true, "ts": now(), "allow": s.allow, "deny": s.deny,
                   "new_promotions": s.promoted, "partial": s.partial}),
            0,
            None,
        ),
        Ok(Run::Skipped) => {
            eprintln!("overwatch ask-learn: another run holds the lock; nothing done");
            return 0;
        }
        Err(e) => (
            json!({"ok": false, "ts": now(), "error": e}),
            1,
            Some(format!(
                "overwatch ask-learn FAILED: {e}. The previous learned table is kept; details in {}",
                status_path.display()
            )),
        ),
    };
    let mut status_err = None;
    if let Err(e) = write_atomic(&status_path, format!("{status:#}\n").as_bytes()) {
        status_err = Some(format!("could not write {}: {e}", status_path.display()));
    }
    if let Ok(Run::Done(s)) = &result {
        println!(
            "ask-learn: allow: {}, deny: {}, new allow promotions: {}{}",
            s.allow,
            s.deny,
            s.promoted,
            if s.partial {
                " (partial: time budget)"
            } else {
                ""
            }
        );
    }
    if let Some(m) = &message {
        eprintln!("{m}");
    }
    if let Some(e) = &status_err {
        eprintln!("overwatch ask-learn: {e}");
    }
    if opts.hook {
        // A Stop hook never blocks the stop (it is not a gate), but a failure
        // is never silent: it is a systemMessage.
        let text = match (message, status_err) {
            (Some(m), Some(e)) => Some(format!("{m} ({e})")),
            (Some(m), None) => Some(m),
            (None, Some(e)) => Some(format!("overwatch ask-learn: {e}")),
            (None, None) => None,
        };
        if let Some(t) = text {
            println!("{}", json!({ "systemMessage": t }));
        }
        return 0;
    }
    if status_err.is_some() && code == 0 {
        return 1;
    }
    code
}

struct Summary {
    allow: usize,
    deny: usize,
    promoted: usize,
    partial: bool,
}

enum Run {
    Done(Summary),
    Skipped,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
    let r = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

struct LockGuard(PathBuf);
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn take_lock(path: &Path) -> Result<Option<LockGuard>, String> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => return Ok(Some(LockGuard(path.to_path_buf()))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(300));
                if !stale {
                    return Ok(None);
                }
                let _ = std::fs::remove_file(path);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(None)
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn find_blastguard(explicit: &Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let c = d.join("blastguard");
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    let cache = harness_core::config::home().join(".claude/plugins/cache/yukineko/blastguard");
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for entry in det(boundary::read_dir_entries(&cache))? {
        let ver: Vec<u64> = entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
        let bin = entry.join("bin/blastguard");
        if bin.is_file() && best.as_ref().is_none_or(|(v, _)| ver > *v) {
            best = Some((ver, bin));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| "no blastguard binary found (pass --blastguard)".to_string())
}

/// `Determination` -> `Result<_, String>`: an undetermined observation is an error.
fn det<T>(d: Determination<T>) -> Result<T, String> {
    match d {
        Determination::Known(v) => Ok(v),
        Determination::Undetermined(why) => Err(why.as_str().to_string()),
    }
}

/// The text of `path`, `None` when it does not exist; any other failure is an error.
fn read_opt(path: &Path) -> Result<Option<String>, String> {
    det(boundary::read_to_string(path))
}

/// Run `cmd` (stdin = `stdin`), require exit 0, return stdout.
fn run_ok(cmd: &mut Command, stdin: Option<Vec<u8>>) -> Result<String, String> {
    cmd.stdin(Stdio::piped());
    det(boundary::run_with_timeout_and_stdin(
        cmd,
        SHAPE_TIMEOUT,
        stdin,
    ))
    .and_then(|o| det(o.stdout_on_success()))
}

fn blastguard_version(bin: &Path) -> Result<String, String> {
    let mut c = Command::new(bin);
    c.arg("--version");
    Ok(run_ok(&mut c, None)?.trim().to_string())
}

/// `Ok(None)` = the current analyser no longer Asks (skip).
fn recompute_shape(bin: &Path, rec: &Value) -> Result<Option<(String, bool)>, String> {
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": rec["session_id"],
        "cwd": rec["cwd"],
        "tool_name": rec["tool_name"],
        "tool_input": rec["tool_input"],
    });
    let mut c = Command::new(bin);
    c.arg("shape");
    for var in ["CLAUDE_PROJECT_DIR", "TMPDIR"] {
        match rec.get("env").and_then(|e| e.get(var)) {
            Some(Value::String(s)) => {
                c.env(var, s);
            }
            Some(Value::Null) => {
                c.env_remove(var);
            }
            _ => {}
        }
    }
    let so = run_ok(&mut c, Some(payload.to_string().into_bytes()))
        .map_err(|e| format!("`blastguard shape` ({}): {e}", bin.display()))?;
    let v: Value = serde_json::from_str(&so)
        .map_err(|e| format!("`blastguard shape` printed non-JSON ({e})"))?;
    if v["decision"] != "ask" {
        return Ok(None);
    }
    let key = v["shape"]
        .as_str()
        .ok_or("`blastguard shape` printed no shape")?;
    Ok(Some((
        key.to_string(),
        v["system_harm"].as_bool().unwrap_or(true),
    )))
}

/// `(tool_use id, name, input, result outcome)` in transcript order.
type ToolUse = (String, String, Value, Option<bool>);

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn read_transcript(path: &Path) -> Result<Vec<ToolUse>, String> {
    let text = read_opt(path)?.unwrap_or_default();
    let mut uses: Vec<ToolUse> = Vec::new();
    let mut results: BTreeMap<String, bool> = BTreeMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(items) = v.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for it in items {
            match it.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (
                        it.get("id").and_then(Value::as_str),
                        it.get("name").and_then(Value::as_str),
                    ) {
                        let input = it.get("input").cloned().unwrap_or(Value::Null);
                        uses.push((id.to_string(), name.to_string(), input, None));
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = it.get("tool_use_id").and_then(Value::as_str) {
                        let declined = result_text(&it["content"]).contains(DECLINE_TEXT);
                        results.insert(id.to_string(), !declined);
                    }
                }
                _ => {}
            }
        }
    }
    for u in &mut uses {
        u.3 = results.get(&u.0).copied();
    }
    Ok(uses)
}

fn transcript_files(projects: &Path, sid: &str) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for dir in det(boundary::read_dir_entries(projects))? {
        let p = dir.join(format!("{sid}.jsonl"));
        if p.is_file() {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

fn learn_run(o: &Opts) -> Result<Run, String> {
    let dir = asklearn::learned_dir();
    let Some(_lock) = take_lock(&dir.join("ask-learn.lock"))? else {
        return Ok(Run::Skipped);
    };
    let started = Instant::now();
    let blocks = o
        .blocks
        .clone()
        .unwrap_or_else(|| harness_core::blocklog::log_path(&harness_core::blocklog::log_dir()));
    let projects = o
        .projects
        .clone()
        .unwrap_or_else(|| harness_core::config::home().join(".claude/projects"));
    let table_path = o.table.clone().unwrap_or_else(asklearn::table_path);
    let bin = find_blastguard(&o.blastguard)?;
    let version = blastguard_version(&bin)?;

    let state_path = dir.join("state.json");
    let mut state = State::default();
    if !o.full {
        // An absent state is a first run; an unreadable one is an error; one
        // that does not parse is rebuilt from scratch (it is only a cache).
        if let Some(t) = read_opt(&state_path)? {
            if let Ok(s) = serde_json::from_str::<State>(&t) {
                if s.version == STATE_VERSION && s.blastguard_version == version {
                    state = s;
                }
            }
        }
    }
    state.version = STATE_VERSION;
    state.blastguard_version = version;

    // --- read new block-log lines
    let mut f = match std::fs::File::open(&blocks) {
        Ok(f) => Some(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", blocks.display())),
    };
    let mut partial = false;
    if let Some(f) = f.as_mut() {
        let len = f
            .metadata()
            .map_err(|e| format!("{}: {e}", blocks.display()))?
            .len();
        if len < state.offset {
            state = State {
                version: STATE_VERSION,
                blastguard_version: state.blastguard_version.clone(),
                ..State::default()
            };
        }
        f.seek(SeekFrom::Start(state.offset))
            .map_err(|e| format!("{}: {e}", blocks.display()))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| format!("{}: {e}", blocks.display()))?;
        let mut pos = 0usize;
        while let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') {
            if o.budget.is_some_and(|b| started.elapsed() > b) {
                partial = true;
                break;
            }
            let line = &buf[pos..pos + nl];
            pos += nl + 1;
            state.offset += (nl + 1) as u64;
            let Ok(rec) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let sid = rec["session_id"].as_str().unwrap_or("");
            if rec["decision"] != "ask"
                || rec["decision_kind"] != "ask"
                || rec["actor"] != "interactive"
                || !is_uuid(sid)
                || rec["tool_name"].as_str().is_none()
                || rec["tool_input"].is_null()
            {
                continue;
            }
            let Some((shape, harm)) = recompute_shape(&bin, &rec)? else {
                continue;
            };
            let sample = rec["command"]
                .as_str()
                .map(str::to_string)
                .or_else(|| rec["file_path"].as_str().map(str::to_string))
                .unwrap_or_default();
            state.pending.push(Pending {
                session_id: sid.to_string(),
                ts: rec["ts"].as_u64().unwrap_or(0),
                tool_name: rec["tool_name"].as_str().unwrap_or("").to_string(),
                tool_input: rec["tool_input"].clone(),
                shape,
                system_harm: harm,
                sample,
            });
        }
    }

    // --- resolve pending against transcripts
    let mut cache: BTreeMap<String, Vec<ToolUse>> = BTreeMap::new();
    let mut still = Vec::new();
    let t_now = now();
    for p in std::mem::take(&mut state.pending) {
        if !cache.contains_key(&p.session_id) {
            let mut all = Vec::new();
            for file in transcript_files(&projects, &p.session_id)? {
                all.extend(read_transcript(&file)?);
            }
            cache.insert(p.session_id.clone(), all);
        }
        let uses = cache.get(&p.session_id).map(Vec::as_slice).unwrap_or(&[]);
        let hit = uses.iter().find(|(id, name, input, res)| {
            res.is_some()
                && *name == p.tool_name
                && *input == p.tool_input
                && !state.claimed.contains(id)
        });
        match hit {
            Some((id, _, _, Some(approved))) => {
                state.claimed.insert(id.clone());
                state.resolved.push(Resolved {
                    session_id: p.session_id,
                    ts: p.ts,
                    shape: p.shape,
                    approved: *approved,
                    sample: p.sample,
                    system_harm: p.system_harm,
                });
            }
            _ if t_now.saturating_sub(p.ts) > PENDING_MAX_AGE_SECS => {}
            _ => still.push(p),
        }
    }
    state.pending = still;

    // --- learn
    let mut recs = Vec::new();
    for r in &state.resolved {
        let shape = Shape::from_key(&r.shape)
            .ok_or_else(|| format!("unparsable shape key {:?}", r.shape))?;
        recs.push(OutcomeRecord {
            shape,
            session_id: r.session_id.clone(),
            outcome: if r.approved {
                Outcome::Approved
            } else {
                Outcome::Declined
            },
            ts: r.ts,
            sample_command: r.sample.clone(),
            system_harm: r.system_harm,
        });
    }
    let table = learn(&recs);
    let mut text = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    text.push('\n');
    let unchanged = std::fs::read(&table_path).is_ok_and(|b| b == text.as_bytes());
    if !unchanged {
        write_atomic(&table_path, text.as_bytes())
            .map_err(|e| format!("{}: {e}", table_path.display()))?;
    }

    // --- promotions
    let promo_path = dir.join("promotions.jsonl");
    let existing: BTreeSet<String> = read_opt(&promo_path)?
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["shape"].as_str().map(str::to_string))
        .collect();
    let mut promoted = 0;
    for e in &table.entries {
        if e.decision == asklearn::Verdict::Allow && !existing.contains(&e.shape) {
            let line = json!({"shape": e.shape, "approvals": e.approvals,
                "sessions": e.sessions, "samples": e.samples, "ts": now(), "filed": false});
            harness_core::append::append_line(&promo_path, &line.to_string())
                .map_err(|er| format!("{}: {er}", promo_path.display()))?;
            promoted += 1;
        }
    }

    // --- persist the cursor last: a failure above re-does the work next time
    let st = serde_json::to_string(&state).map_err(|e| e.to_string())?;
    write_atomic(&state_path, st.as_bytes())
        .map_err(|e| format!("{}: {e}", state_path.display()))?;

    Ok(Run::Done(Summary {
        allow: table
            .entries
            .iter()
            .filter(|e| e.decision == asklearn::Verdict::Allow)
            .count(),
        deny: table
            .entries
            .iter()
            .filter(|e| e.decision == asklearn::Verdict::Deny)
            .count(),
        promoted,
        partial,
    }))
}

// ------------------------------------------------------------ file-reds

fn git_out(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c)
        .current_dir(dir)
        .args(args);
    let o = det(boundary::run(&mut c))?;
    Ok(det(o.stdout_on_success())?.trim().to_string())
}

/// True only for a LINKED worktree: its git dir differs from the common dir.
/// Anything undeterminable (not a repo, git failure) is refused.
fn is_linked_worktree(w: &Path) -> Result<bool, String> {
    let gd = git_out(w, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let cd = git_out(
        w,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let canon = |s: &str| std::fs::canonicalize(s).map_err(|e| format!("{s}: {e}"));
    Ok(canon(&gd)? != canon(&cd)?)
}

fn red_test_source(key: &str, samples: &[String]) -> String {
    let list: String = samples.iter().map(|s| format!("    {s:?},\n")).collect();
    include_str!("ask_learn_red.tmpl")
        .replace("@KEY@", &key.replace('"', "'"))
        .replace("@LIST@", &list)
}

fn file_reds(args: &[String]) -> Result<usize, String> {
    let mut worktree: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--worktree" => {
                i += 1;
                worktree = Some(PathBuf::from(
                    args.get(i).ok_or("--worktree needs a value")?,
                ));
            }
            other => return Err(format!("unknown option `{other}`")),
        }
        i += 1;
    }
    let w = worktree.ok_or("--worktree <linked worktree> is required")?;
    if !is_linked_worktree(&w)? {
        return Err(format!(
            "{} is not a linked git worktree (the main working tree is merge-only, CLAUDE.md §8); refusing",
            w.display()
        ));
    }
    let promo_path = asklearn::learned_dir().join("promotions.jsonl");
    let Some(text) = read_opt(&promo_path)? else {
        return Ok(0);
    };
    let mut rows: Vec<Value> = Vec::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        rows.push(serde_json::from_str(l).map_err(|e| format!("promotions.jsonl: {e}"))?);
    }
    let mut filed = 0;
    for idx in 0..rows.len() {
        if rows[idx]["filed"] != false {
            continue;
        }
        let key = rows[idx]["shape"]
            .as_str()
            .ok_or("promotion without shape")?
            .to_string();
        let samples: Vec<String> = rows[idx]["samples"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if samples.is_empty() {
            return Err(format!(
                "promotion {key} has no sample command; nothing to assert"
            ));
        }
        let hex = harness_core::hash::fnv1a32_hex(&key);
        let rel = format!("crates/blastguard/tests/learned_shape_{hex}.rs");
        let abs = w.join(&rel);
        write_atomic(&abs, red_test_source(&key, &samples).as_bytes())
            .map_err(|e| format!("{}: {e}", abs.display()))?;
        git_out(&w, &["add", "--", &rel])?;
        git_out(
            &w,
            &[
                "commit",
                "-q",
                "-m",
                &format!("test(learned): RED for learned Allow shape {key}"),
                "--",
                &rel,
            ],
        )?;
        let mut bl = Command::new("backlog");
        bl.current_dir(&w)
            .args(["add", "--title"])
            .arg(format!(
                "blastguard: promote learned Allow shape {key} to a permanent rule"
            ))
            .arg("--project")
            .arg(&w)
            .args(["--tag", "blastguard", "--repro-test"])
            .arg(format!(
                "cargo test -p blastguard --test learned_shape_{hex} -- --ignored"
            ))
            .arg("--notes")
            .arg(format!(
                "Learned by `overwatch ask-learn`; samples: {samples:?}"
            ));
        // A refused `backlog add` (repro not reproduced, gate) is an error:
        // the promotion stays unfiled.
        det(boundary::run(&mut bl))
            .and_then(|o| det(o.stdout_on_success()))
            .map_err(|e| format!("backlog add failed for {key}: {e}"))?;
        rows[idx]["filed"] = json!(true);
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        write_atomic(&promo_path, body.as_bytes())
            .map_err(|e| format!("{}: {e}", promo_path.display()))?;
        filed += 1;
    }
    Ok(filed)
}
