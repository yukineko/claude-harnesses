// Integration tests: unwrap/expect are the idiom here (workspace lints are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! One focused reproduction per prose claim in README.md / README.ja.md /
//! Makefile / commands/record.md. Every test drives the real binary (or reads
//! the shipped plugin file) in a fresh tempdir with an isolated HOME, and
//! clears the environment variables the outer shell could leak in.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_session-insights");
const NUM_START: &str = "<!-- si:numeric:start -->";
const NUM_END: &str = "<!-- si:numeric:end -->";
const COST_START: &str = "<!-- si:cost:start -->";
const COST_END: &str = "<!-- si:cost:end -->";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    _td: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
    vault: PathBuf,
}

impl Env {
    /// Fresh HOME + project dir (`proj`, so project name is `proj`). The vault
    /// path is chosen but NOT created; call `mk_vault`.
    fn new() -> Env {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let home = root.join("home");
        let proj = root.join("proj");
        let vault = root.join("vault");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        Env {
            _td: td,
            home,
            proj,
            vault,
        }
    }

    fn mk_vault(&self) {
        std::fs::create_dir_all(&self.vault).unwrap();
    }

    fn toml(&self, extra: &str) {
        let body = format!("obsidian_vault = \"{}\"\n{extra}\n", self.vault.display());
        std::fs::write(self.proj.join("session-insights.toml"), body).unwrap();
    }

    fn run(&self, args: &[&str], stdin: &str, envs: &[(&str, &str)]) -> Out {
        let mut cmd = Command::new(BIN);
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.proj)
            .env("HOME", &self.home)
            .env_remove("SESSION_INSIGHTS_DISABLE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CONTEXT_GOVERNOR_STATE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn binary");
        {
            let mut si = child.stdin.take().unwrap();
            let _ = si.write_all(stdin.as_bytes());
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn tool_payload(sid: &str, tool: &str, file: Option<&str>) -> String {
        let input = match file {
            Some(f) => serde_json::json!({ "file_path": f }),
            None => serde_json::json!({}),
        };
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": sid,
            "tool_name": tool,
            "tool_input": input,
        })
        .to_string()
    }

    fn record(&self, sid: &str, tool: &str, file: Option<&str>) -> Out {
        self.run(&["record"], &Self::tool_payload(sid, tool, file), &[])
    }

    fn stop(&self, sid: &str) -> Out {
        let p = serde_json::json!({"hook_event_name":"Stop","session_id":sid}).to_string();
        self.run(&["stop"], &p, &[])
    }

    fn report(&self, sid: &str) -> String {
        let o = self.run(&["report", "--session", sid], "", &[]);
        assert_eq!(o.code, 0, "report failed: {}", o.stderr);
        o.stdout
    }

    fn record_now(&self, sid: &str) -> Out {
        self.run(&["record-now"], "", &[("CLAUDE_CODE_SESSION_ID", sid)])
    }

    fn note_path(&self, o: &Out) -> PathBuf {
        let p = PathBuf::from(o.stdout.trim());
        assert!(
            p.is_file(),
            "record-now did not print an existing note path: stdout={:?} stderr={:?}",
            o.stdout,
            o.stderr
        );
        p
    }
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    v.sort();
    v
}

fn between<'a>(text: &'a str, a: &str, b: &str) -> &'a str {
    let s = text.find(a).unwrap_or_else(|| panic!("no {a} in {text}")) + a.len();
    let e = s + text[s..]
        .find(b)
        .unwrap_or_else(|| panic!("no {b} in {text}"));
    &text[s..e]
}

fn headings(note: &str) -> Vec<&str> {
    note.lines().filter(|l| l.starts_with("## ")).collect()
}

// ---------------------------------------------------------------- record note

#[test]
fn claim_machine_block_edits_overwritten() {
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    let o = e.record_now("mblock-1111");
    let p = e.note_path(&o);
    let note = std::fs::read_to_string(&p).unwrap();
    let edited = note
        .replace(NUM_START, &format!("{NUM_START}\nUSER-EDIT-NUM"))
        .replace(COST_START, &format!("{COST_START}\nUSER-EDIT-COST"))
        + "\nUSER-OUTSIDE-KEEP\n";
    std::fs::write(&p, edited).unwrap();

    let o2 = e.record_now("mblock-1111");
    assert_eq!(PathBuf::from(o2.stdout.trim()), p);
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(
        !after.contains("USER-EDIT-NUM"),
        "edit inside si:numeric survived: {after}"
    );
    assert!(
        !after.contains("USER-EDIT-COST"),
        "edit inside si:cost survived: {after}"
    );
    assert!(
        after.contains("USER-OUTSIDE-KEEP"),
        "text outside the blocks was lost: {after}"
    );
}

fn full_skeleton_without_markers(e: &Env, sid: &str) -> (PathBuf, String) {
    let o = e.record_now(sid);
    let p = e.note_path(&o);
    let note = std::fs::read_to_string(&p).unwrap();
    let stripped: String = note
        .lines()
        .filter(|l| ![NUM_START, NUM_END, COST_START, COST_END].contains(l))
        .map(|l| format!("{l}\n"))
        .collect();
    (p, stripped)
}

#[test]
fn claim_merge_leaves_markerless_note() {
    // Markers absent, every heading present: byte-identical after a write.
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    let (p, markerless) = full_skeleton_without_markers(&e, "mless-2222");
    assert!(markerless.contains("## 数値サマリ") && markerless.contains("## コスト"));
    assert!(!markerless.contains("si:numeric") && !markerless.contains("si:cost"));
    std::fs::write(&p, &markerless).unwrap();
    let o = e.record_now("mless-2222");
    assert_eq!(PathBuf::from(o.stdout.trim()), p);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        markerless,
        "a note without si:* markers was rewritten"
    );
}

#[test]
fn claim_merge_condition_is_markers_not_headings() {
    // Markers present but NO ## 数値サマリ / ## コスト headings: still refreshed.
    // So the operative condition is "markers absent", not "headings absent".
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    let o = e.record_now("mcond-3333");
    let p = e.note_path(&o);
    let custom =
        format!("# custom\n{NUM_START}\nOLD-NUM\n{NUM_END}\n{COST_START}\nOLD-COST\n{COST_END}\n");
    std::fs::write(&p, &custom).unwrap();
    e.record_now("mcond-3333");
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(
        !after.contains("OLD-NUM") && !after.contains("OLD-COST"),
        "{after}"
    );
    assert!(after.contains("- turns:"), "{after}");
    assert!(
        !after.contains("## 数値サマリ"),
        "no heading should be added: {after}"
    );
}

// ---------------------------------------------------------------- hooks

#[test]
fn claim_posttooluse_records_tool_and_files() {
    let e = Env::new();
    let sid = "ptu-aaaa1111";
    e.record(sid, "Edit", Some("a.rs"));
    e.record(sid, "Edit", Some("a.rs"));
    e.record(sid, "Read", Some("b.rs"));
    let r = e.report(sid);
    assert!(r.contains("top tools: Edit 2, Read 1"), "{r}");
    assert!(r.contains("files: 2"), "{r}");
    assert!(r.contains("tool events: 3"), "{r}");
}

#[test]
fn claim_stop_counts_turn() {
    let e = Env::new();
    let sid = "stopturn-aa";
    e.stop(sid);
    assert!(e.report(sid).contains("turns: 1"));
    e.stop(sid);
    assert!(e.report(sid).contains("turns: 2"));
}

#[test]
fn claim_stop_writes_session_note_when_obsidian_log() {
    let e = Env::new();
    e.mk_vault();
    e.toml("obsidian_log = true");
    let sid = "snote-bbbb2222";
    e.record(sid, "Edit", Some("a.rs"));
    let o = e.stop(sid);
    assert_eq!(o.code, 0);
    let dir = e.vault.join("sessions");
    let files = md_files(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.ends_with("-snote-bb.md"), "{name}");
    let body = std::fs::read_to_string(&files[0]).unwrap();
    assert!(body.starts_with("---\n"), "{body}");
    for l in [
        "type: session",
        "size: XS",
        "category: coding",
        "turns: 1",
        "tool_events: 1",
    ] {
        assert!(body.lines().any(|x| x == l), "missing {l:?} in {body}");
    }
    // Second stop overwrites in place.
    e.stop(sid);
    let files2 = md_files(&dir);
    assert_eq!(files2, files);
    let body2 = std::fs::read_to_string(&files2[0]).unwrap();
    assert!(body2.lines().any(|x| x == "turns: 2"), "{body2}");
    assert!(!body2.lines().any(|x| x == "turns: 1"), "{body2}");
}

#[test]
fn claim_stop_session_note_never_creates_vault_and_needs_opt_in() {
    // Missing vault: nothing created.
    let e = Env::new();
    e.toml("obsidian_log = true");
    e.stop("snote-cccc3333");
    assert!(!e.vault.exists(), "vault was created");
    // obsidian_log = false: no sessions/ note even though the vault exists.
    let e2 = Env::new();
    e2.mk_vault();
    e2.toml("obsidian_log = false");
    e2.stop("snote-dddd4444");
    assert!(!e2.vault.join("sessions").exists());
}

#[test]
fn claim_stop_refreshes_record_when_record_true() {
    let e = Env::new();
    e.mk_vault();
    e.toml("record = true");
    let sid = "srec-eeee5555";
    e.stop(sid);
    let files = md_files(&e.vault.join("records"));
    assert_eq!(files.len(), 1, "{files:?}");
    let note = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        between(&note, NUM_START, NUM_END).contains("- turns: 1"),
        "{note}"
    );
    // Every turn refreshes it.
    e.stop(sid);
    let note2 = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        between(&note2, NUM_START, NUM_END).contains("- turns: 2"),
        "{note2}"
    );

    let e2 = Env::new();
    e2.mk_vault();
    e2.toml("record = false");
    e2.stop("srec-ffff6666");
    assert!(!e2.vault.join("records").exists());
}

#[test]
fn claim_sessionend_writes_record_only_when_record_true() {
    let payload = |sid: &str| {
        serde_json::json!({"hook_event_name":"SessionEnd","session_id":sid}).to_string()
    };
    let e = Env::new();
    e.mk_vault();
    e.toml("record = true");
    e.record("send-aaaa7777", "Read", Some("a.rs"));
    e.run(&["sessionend"], &payload("send-aaaa7777"), &[]);
    let files = md_files(&e.vault.join("records"));
    assert_eq!(files.len(), 1, "{files:?}");
    let note = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        between(&note, NUM_START, NUM_END).contains("- tool events: 1"),
        "{note}"
    );

    let e2 = Env::new();
    e2.mk_vault();
    e2.toml("record = false");
    e2.run(&["sessionend"], &payload("send-bbbb8888"), &[]);
    assert!(!e2.vault.join("records").exists());
}

// ---------------------------------------------------------------- metrics

#[test]
fn claim_size_thresholds_default_boundaries() {
    let e = Env::new();
    let sid = "size-default";
    let size_after = |n: usize| -> String {
        let r = e.report(sid);
        let line = r.lines().find(|l| l.contains("size:")).unwrap().to_string();
        assert!(r.contains(&format!("tool events: {n}")), "{r}");
        line.split_whitespace().nth(1).unwrap().to_string()
    };
    let expect: &[(usize, &str)] = &[
        (4, "XS"),
        (5, "S"),
        (14, "S"),
        (15, "M"),
        (39, "M"),
        (40, "L"),
        (99, "L"),
        (100, "XL"),
    ];
    let mut done = 0;
    for (n, want) in expect {
        while done < *n {
            e.record(sid, "Edit", Some("a.rs"));
            done += 1;
        }
        assert_eq!(size_after(*n), *want, "at {n} events");
    }
}

#[test]
fn claim_size_thresholds_configurable() {
    let e = Env::new();
    e.toml("size_thresholds = [1, 2, 3, 4]");
    let sid = "size-config";
    for _ in 0..3 {
        e.record(sid, "Edit", Some("a.rs"));
    }
    // 3 events: default thresholds would say XS; [1,2,3,4] says L.
    let r = e.report(sid);
    assert!(r.contains("size: L"), "{r}");
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(st.contains("[1, 2, 3, 4]"), "{st}");
}

fn category_of(tools: &[&str]) -> String {
    let e = Env::new();
    let sid = "category-x1";
    for t in tools {
        e.record(sid, t, Some("f.rs"));
    }
    let r = e.report(sid);
    let line = r.lines().find(|l| l.contains("category:")).unwrap();
    line.split("category:").nth(1).unwrap().trim().to_string()
}

#[test]
fn claim_category_rule() {
    assert_eq!(category_of(&["Bash", "Bash", "Bash", "Read"]), "ops");
    assert_eq!(category_of(&["Read", "Grep", "Glob", "Edit"]), "research");
    assert_eq!(category_of(&["WebFetch", "WebSearch", "Bash"]), "research");
    assert_eq!(
        category_of(&["MultiEdit", "NotebookEdit", "Bash"]),
        "coding"
    );
    assert_eq!(category_of(&["Write", "Edit", "Bash"]), "coding");
    // exactly half -> that group
    assert_eq!(category_of(&["Edit", "Edit", "Bash", "Read"]), "coding");
    // below half -> mixed
    assert_eq!(category_of(&["Edit", "Bash", "Read"]), "mixed");
    // no categorized events -> empty
    assert_eq!(category_of(&["Agent"]), "empty");
}

#[test]
fn claim_report_shows_counts() {
    let e = Env::new();
    let sid = "counts-1234";
    e.record(sid, "Edit", Some("a.rs"));
    e.record(sid, "Read", Some("b.rs"));
    e.record(sid, "Read", Some("c.rs"));
    e.stop(sid);
    e.stop(sid);
    let r = e.report(sid);
    assert!(r.contains("turns: 2"), "{r}");
    assert!(r.contains("tool events: 3"), "{r}");
    assert!(r.contains("files: 3"), "{r}");
    assert!(r.contains("top tools: Read 2, Edit 1"), "{r}");
}

#[test]
fn claim_report_latest_default() {
    let e = Env::new();
    e.record("aaaaaaaa-first", "Read", Some("a.rs"));
    e.record("bbbbbbbb-second", "Read", Some("a.rs"));
    let o = e.run(&["report"], "", &[]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("session bbbbbbbb"), "{}", o.stdout);
    assert!(!o.stdout.contains("aaaaaaaa"), "{}", o.stdout);
}

#[test]
fn claim_report_all_one_line_each() {
    let e = Env::new();
    for sid in ["aaaaaaaa-1", "bbbbbbbb-2", "cccccccc-3"] {
        e.record(sid, "Read", Some("a.rs"));
    }
    let o = e.run(&["report", "--all"], "", &[]);
    assert_eq!(o.code, 0);
    let lines: Vec<&str> = o.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", o.stdout);
    for id in ["aaaaaaaa", "bbbbbbbb", "cccccccc"] {
        assert_eq!(lines.iter().filter(|l| l.starts_with(id)).count(), 1);
    }
}

#[test]
#[cfg(unix)]
fn claim_report_unreadable_store_exits_1() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.record("unread-1111", "Read", Some("a.rs"));
    let state = e.home.join(".session-insights/state");
    assert!(state.is_dir());
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&state).is_ok() {
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        panic!("cannot observe: chmod 000 does not deny this user (root?)");
    }
    let single = e.run(&["report"], "", &[]);
    let all = e.run(&["report", "--all"], "", &[]);
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    for o in [single, all] {
        assert_eq!(o.code, 1, "stdout={:?} stderr={:?}", o.stdout, o.stderr);
        assert!(
            o.stderr
                .contains("unknown — session store could not be read:"),
            "{}",
            o.stderr
        );
        assert!(!o.stdout.contains("no sessions"), "{}", o.stdout);
        assert!(!o.stdout.contains("no matching session"), "{}", o.stdout);
    }
}

// ---------------------------------------------------------------- record-now

fn assert_dated_name(p: &Path, tail: &str) {
    let n = p.file_name().unwrap().to_str().unwrap();
    let b = n.as_bytes();
    assert!(
        n.len() > 10
            && b[4] == b'-'
            && b[7] == b'-'
            && n[..10].chars().all(|c| c.is_ascii_digit() || c == '-')
            && n[10..] == *tail,
        "{n} != <date>{tail}"
    );
}

#[test]
fn claim_record_note_path_and_vault_required() {
    // record=false (default) + existing vault: record-now writes anyway, at <vault>/records.
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    let o = e.record_now("path-abcdef12345");
    let p = e.note_path(&o);
    assert_eq!(p.parent().unwrap(), e.vault.join("records"));
    assert_dated_name(&p, "-proj-path-abc.md");

    // record_dir override.
    let e2 = Env::new();
    e2.mk_vault();
    e2.toml("record_dir = \"x\"");
    let p2 = e2.note_path(&e2.record_now("path-abcdef12345"));
    assert_eq!(p2.parent().unwrap(), e2.vault.join("x"));

    // Missing vault: not created, no note, stderr names the vault.
    let e3 = Env::new();
    e3.toml("");
    let o3 = e3.record_now("path-abcdef12345");
    assert!(!e3.vault.exists(), "vault was created");
    assert!(o3.stdout.trim().is_empty(), "{}", o3.stdout);
    assert!(
        o3.stderr.contains(&e3.vault.display().to_string()),
        "stderr does not name the vault: {}",
        o3.stderr
    );
}

#[test]
fn claim_record_now_prints_path() {
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    e.record("recnowxx-1", "Read", Some("a.rs"));
    e.record("recnowyy-2", "Read", Some("a.rs"));
    e.record("recnowyy-2", "Read", Some("b.rs"));
    let o = e.record_now("recnowyy-2");
    assert_eq!(o.code, 0);
    let p = PathBuf::from(o.stdout.trim());
    assert!(p.is_absolute(), "{p:?}");
    assert!(p.is_file());
    assert_dated_name(&p, "-proj-recnowyy.md");
    let note = std::fs::read_to_string(&p).unwrap();
    assert!(
        between(&note, NUM_START, NUM_END).contains("- tool events: 2"),
        "{note}"
    );
    assert!(!between(&note, COST_START, COST_END).trim().is_empty());
}

#[test]
fn claim_skeleton_prose_sections_placeholders() {
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    e.record("skel-aaaa1111", "Edit", Some("a.rs"));
    let note = std::fs::read_to_string(e.note_path(&e.record_now("skel-aaaa1111"))).unwrap();
    let lines: Vec<&str> = note.lines().collect();
    for h in [
        "完了サマリ",
        "認知の変化 / 修正された理解",
        "つまずき / 学び",
        "振り返り / 確立した方針",
        "自己批判 / 確信度",
        "注意点 / 落とし穴",
        "残課題",
        "要追跡 / あとで確認",
        "関連",
    ] {
        let i = lines
            .iter()
            .position(|l| *l == format!("## {h}"))
            .unwrap_or_else(|| panic!("heading {h} missing:\n{note}"));
        assert_eq!(
            lines[i + 1],
            format!("<!-- fill: {h} -->"),
            "placeholder after {h}"
        );
    }
    // Numeric/cost blocks are filled, not placeholders.
    assert!(between(&note, NUM_START, NUM_END).contains("- tool events: 1"));
    assert!(!between(&note, COST_START, COST_END).contains("fill:"));
    assert!(!between(&note, COST_START, COST_END).trim().is_empty());
}

#[test]
fn claim_cognition_sections_adjacent() {
    let e = Env::new();
    e.mk_vault();
    e.toml("");
    let note = std::fs::read_to_string(e.note_path(&e.record_now("adj-aaaa1111"))).unwrap();
    let hs = headings(&note);
    let at = |h: &str| {
        hs.iter()
            .position(|x| *x == h)
            .unwrap_or_else(|| panic!("{h}: {hs:?}"))
    };
    assert_eq!(
        at("## 認知の変化 / 修正された理解"),
        at("## 完了サマリ") + 1,
        "{hs:?}"
    );
    assert_eq!(
        at("## 自己批判 / 確信度"),
        at("## 振り返り / 確立した方針") + 1,
        "{hs:?}"
    );
}

// ---------------------------------------------------------------- backlog

#[test]
fn claim_no_backlog_subcommand() {
    let e = Env::new();
    let o = e.run(&["backlog", "list"], "", &[]);
    assert_ne!(o.code, 0, "stdout={:?}", o.stdout);
    assert!(o.stderr.contains("unrecognized subcommand"), "{}", o.stderr);

    e.mk_vault();
    e.toml("record = true\nobsidian_log = true");
    let sid = "nobl-aaaa1111";
    e.record(sid, "Edit", Some("a.rs"));
    e.stop(sid);
    e.run(
        &["sessionend"],
        &serde_json::json!({"hook_event_name":"SessionEnd","session_id":sid}).to_string(),
        &[],
    );
    e.record_now(sid);
    for f in ["backlog.md", "backlog.json"] {
        assert!(!e.vault.join(f).exists(), "{f} created in vault");
    }
}

// ---------------------------------------------------------------- packaging

#[test]
fn claim_hooks_json_three_hooks() {
    let raw =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["PostToolUse", "SessionEnd", "Stop"]);
    assert_eq!(
        hooks["PostToolUse"][0]["matcher"],
        "Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit"
    );
    for (ev, sub) in [
        ("PostToolUse", "record"),
        ("Stop", "stop"),
        ("SessionEnd", "sessionend"),
    ] {
        assert_eq!(hooks[ev].as_array().unwrap().len(), 1, "{ev}");
        let inner = hooks[ev][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1, "{ev}");
        assert_eq!(
            inner[0]["command"],
            format!("${{CLAUDE_PLUGIN_ROOT}}/bin/session-insights {sub}")
        );
    }
}

#[test]
fn claim_project_toml_tunes() {
    let e = Env::new();
    e.toml("obsidian_log = true\nsize_thresholds = [2, 4, 6, 8]");
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(st.contains("obsidian_log:    true"), "{st}");
    assert!(st.contains("size_thresholds: [2, 4, 6, 8]"), "{st}");
    assert!(
        st.contains(&e.proj.join("session-insights.toml").display().to_string()),
        "{st}"
    );
}

fn settings_of(e: &Env) -> serde_json::Value {
    let p = e.home.join(".claude/settings.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn claim_install_merges_three_hooks() {
    let e = Env::new();
    let o = e.run(&["install"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let s = settings_of(&e);
    let hooks = s["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["PostToolUse", "SessionEnd", "Stop"]);
    for (ev, sub) in [
        ("PostToolUse", " record"),
        ("Stop", " stop"),
        ("SessionEnd", " sessionend"),
    ] {
        let cmd = hooks[ev][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("session-insights") && cmd.ends_with(sub),
            "{ev}: {cmd}"
        );
    }
}

#[test]
fn claim_status_resolved_config() {
    let e = Env::new();
    e.toml("record_dir = \"zzz\"");
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(st.contains("record_dir:      zzz"), "{st}");
}

#[test]
fn claim_init_creates_state_dir() {
    let e = Env::new();
    let state = e.home.join(".session-insights/state");
    assert!(!state.exists());
    let o = e.run(&["init"], "", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(state.is_dir());
}

#[test]
fn claim_install_idempotent_backup_foreign() {
    let e = Env::new();
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let sp = claude.join("settings.json");
    std::fs::write(
        &sp,
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"foreign-tool stop"}]}],
            "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"other-check"}]}]}}"#,
    )
    .unwrap();
    assert_eq!(e.run(&["install"], "", &[]).code, 0);
    let after1 = std::fs::read(&sp).unwrap();
    assert_eq!(e.run(&["install"], "", &[]).code, 0);
    let after2 = std::fs::read(&sp).unwrap();
    assert_eq!(after1, after2, "second install changed settings.json");
    let t1 = String::from_utf8(after1).unwrap();
    assert!(t1.contains("foreign-tool stop") && t1.contains("other-check"));
    // Exactly one group of ours per event (the path itself may contain the name
    // more than once, so count the command entries rather than substrings).
    assert_eq!(t1.matches("/session-insights ").count(), 3, "{t1}");
    let backups: Vec<_> = std::fs::read_dir(&claude)
        .unwrap()
        .flatten()
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .collect();
    assert!(!backups.is_empty(), "no settings.json backup created");
    let backup_text = std::fs::read_to_string(backups[0].path()).unwrap();
    assert!(backup_text.contains("foreign-tool stop"));

    assert_eq!(e.run(&["uninstall"], "", &[]).code, 0);
    let un1 = std::fs::read(&sp).unwrap();
    let t = String::from_utf8(un1.clone()).unwrap();
    assert!(!t.contains("session-insights"), "{t}");
    assert!(
        t.contains("foreign-tool stop") && t.contains("other-check"),
        "{t}"
    );
    assert_eq!(e.run(&["uninstall"], "", &[]).code, 0);
    assert_eq!(
        un1,
        std::fs::read(&sp).unwrap(),
        "second uninstall changed file"
    );
}

#[test]
fn claim_config_lookup_order() {
    let e = Env::new();
    let gdir = e.home.join(".session-insights");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::write(gdir.join("config.toml"), "record_dir = \"globaldir\"\n").unwrap();
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(
        st.contains("record_dir:      globaldir"),
        "only global: {st}"
    );
    std::fs::write(
        e.proj.join("session-insights.toml"),
        "record_dir = \"projdir\"\n",
    )
    .unwrap();
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(
        st.contains("record_dir:      projdir"),
        "both present: {st}"
    );
    // First that exists wins: the project file does not inherit from the global one.
    std::fs::write(gdir.join("config.toml"), "enabled = false\n").unwrap();
    let st = e.run(&["status"], "", &[]).stdout;
    assert!(st.contains("enabled:         true"), "{st}");
}

#[test]
fn claim_config_defaults_values() {
    let e = Env::new();
    let st = e.run(&["status"], "", &[]).stdout;
    let h = e.home.display();
    for l in [
        "enabled:         true".to_string(),
        "ignore_tools:    TodoWrite".to_string(),
        "size_thresholds: [5, 15, 40, 100] (S,M,L,XL)".to_string(),
        "obsidian_log:    false".to_string(),
        format!("obsidian_vault:  {h}/Documents/vault/yukineko"),
        "record:          false".to_string(),
        "record_dir:      records".to_string(),
        format!("state_dir:       {h}/.session-insights/state"),
    ] {
        assert!(st.lines().any(|x| x == l), "missing {l:?} in:\n{st}");
    }
}

#[test]
fn claim_config_defaults_todowrite_ignored() {
    let e = Env::new();
    let sid = "todo-aaaa1111";
    e.record(sid, "TodoWrite", None);
    let o = e.run(&["report", "--session", sid], "", &[]);
    assert!(o.stdout.contains("no matching session"), "{}", o.stdout);
    e.record(sid, "Read", Some("a.rs"));
    e.record(sid, "TodoWrite", None);
    assert!(e.report(sid).contains("tool events: 1"));
}

#[test]
fn claim_config_defaults_enabled_false_is_noop() {
    let e = Env::new();
    e.toml("enabled = false");
    e.record("off-aaaa1111", "Read", Some("a.rs"));
    let o = e.run(&["report", "--session", "off-aaaa1111"], "", &[]);
    assert!(o.stdout.contains("no matching session"), "{}", o.stdout);
}

fn note_with_transcript(overrides: &str) -> String {
    let e = Env::new();
    e.mk_vault();
    e.toml(overrides);
    let sid = "price-aaaa1111";
    let tdir = e.home.join(".claude/projects/p");
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(
        tdir.join(format!("{sid}.jsonl")),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-06-22T10:00:01Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":1000000,"output_tokens":1000000,"cache_read_input_tokens":0}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let note = std::fs::read_to_string(e.note_path(&e.record_now(sid))).unwrap();
    between(&note, COST_START, COST_END).to_string()
}

#[test]
fn claim_config_defaults_price_overrides_change_cost() {
    let builtin = note_with_transcript("");
    assert!(builtin.contains("- total: $30.00 USD"), "{builtin}");
    let overridden = note_with_transcript(
        "[[price_overrides]]\npattern = \"opus\"\ninput = 1.0\noutput = 2.0\n",
    );
    assert!(overridden.contains("- total: $3.00 USD"), "{overridden}");
}

#[test]
fn claim_kill_switch_env() {
    for (val, counts) in [("1", false), ("true", false), ("0", true), ("", true)] {
        let e = Env::new();
        let sid = "kill-aaaa1111";
        let o = e.run(
            &["record"],
            &Env::tool_payload(sid, "Read", Some("a.rs")),
            &[("SESSION_INSIGHTS_DISABLE", val)],
        );
        assert_eq!(o.code, 0);
        let r = e.run(&["report", "--session", sid], "", &[]).stdout;
        if counts {
            assert!(r.contains("tool events: 1"), "DISABLE={val:?}: {r}");
        } else {
            assert!(r.contains("no matching session"), "DISABLE={val:?}: {r}");
        }
    }
}

// ---------------------------------------------------------------- git / launcher

fn git(args: &[&str]) -> (i32, String) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("git runs");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn claim_bins_gitignored() {
    let (rc, out) = git(&["ls-files", "bin"]);
    assert_eq!(rc, 0, "git ls-files failed");
    assert_eq!(
        out.trim(),
        "bin/session-insights",
        "tracked bin files: {out}"
    );
    for f in [
        "bin/session-insights-darwin-arm64",
        "bin/session-insights-darwin-x86_64",
        "bin/session-insights-linux-arm64",
        "bin/session-insights-linux-x86_64",
    ] {
        let (rc, _) = git(&["check-ignore", "-q", f]);
        assert_eq!(rc, 0, "{f} is not git-ignored");
    }
    let (rc, _) = git(&["check-ignore", "-q", "bin/session-insights"]);
    assert_eq!(rc, 1, "the launcher must NOT be ignored");
}

#[test]
#[cfg(unix)]
fn claim_launcher_missing_binary() {
    let td = tempfile::tempdir().unwrap();
    let launcher = td.path().join("session-insights");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/session-insights"),
        &launcher,
    )
    .unwrap();
    let run = |arg: &str| -> Out {
        let o = harness_core::git_env::scrub(&mut Command::new(&launcher))
            .arg(arg)
            .stdin(Stdio::null())
            .output()
            .expect("launcher runs");
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    };
    let rec = run("record");
    assert!(rec.stderr.contains("record did NOT run"), "{}", rec.stderr);
    // record's exit code is deliberately NOT pinned (README claims only the stderr line).
    let stop = run("stop");
    assert_eq!(stop.code, 0);
    let v: serde_json::Value = serde_json::from_str(stop.stdout.trim()).expect("stop emits JSON");
    assert!(v["systemMessage"].as_str().unwrap().contains("did NOT run"));
    let end = run("sessionend");
    assert_eq!(end.code, 1);
    assert!(end.stderr.contains("did NOT run"), "{}", end.stderr);
    for verb in ["report", "status", "install"] {
        let o = run(verb);
        assert_eq!(o.code, 127, "{verb}: {}", o.stderr);
        assert!(o.stderr.contains("did NOT run"), "{}", o.stderr);
    }
}
