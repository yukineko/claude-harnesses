//! Prose-claim reproduction tests for the backlog README / SKILL.md.
//!
//! One `claim_<id>` test per prose claim, written by an independent test
//! writer: each drives the REAL built binary (or reads the real packaged
//! file) and asserts the outcome the prose states. Nothing here pins a
//! fail-open behaviour: every "cannot determine" case asserts a refusal.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fixture, Out};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ---------------------------------------------------------------- helpers

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn session_start(f: &Fixture) -> Out {
    let payload = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"s","cwd":"{}"}}"#,
        f.repo.display()
    );
    f.run_stdin(&["session-start"], &[], payload.as_bytes())
}

/// The `additionalContext` string of the SessionStart hook JSON on stdout.
fn additional_context(o: &Out) -> String {
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout is not JSON ({e}): {:?}", o.stdout));
    v["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no additionalContext in {v}"))
        .to_string()
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Append a hand-written `[[task]]` block to `rel` (relative to the repo).
fn plant(f: &Fixture, rel: &str, body: &str) {
    let p = f.repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(body);
    std::fs::write(&p, cur).unwrap();
}

fn task_block(id: &str, title: &str, project: &str, status: &str, extra: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{project}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n{extra}\n"
    )
}

// ------------------------------------------------- SessionStart hook claims

/// README.md:1 / README.ja.md:1 -- the SessionStart hook injects pending work
/// as context when a session opens.
#[test]
fn claim_r_hook_injects_pending_work() {
    let f = Fixture::new("hook-inject");
    let pending = f.add("alpha pending item");
    let done = f.add("beta item that gets closed");
    let o = f.run(&["done", &done, "--duplicate-of", &pending]);
    assert_eq!(o.code, 0, "{}", o.both());

    let o = session_start(&f);
    assert_eq!(o.code, 0, "{}", o.both());
    let ctx = additional_context(&o);
    assert!(
        ctx.contains(&pending) && ctx.contains("alpha pending item"),
        "pending task must be injected into additionalContext: {ctx}"
    );
    assert!(
        !ctx.contains(&done) && !ctx.contains("beta item that gets closed"),
        "a done task must not be injected as pending work: {ctx}"
    );
}

/// README.md:149-151 -- the SessionStart hook injects a "store UNREADABLE"
/// notice instead of an empty queue when the done file is unreadable.
#[test]
fn claim_r_store_unreadable() {
    let f = Fixture::new("hook-unreadable");
    let _ = f.add("some pending item");
    f.write(".backlog/tasks.done.toml", "this is [[ not valid toml");
    let o = session_start(&f);
    assert_eq!(o.code, 0, "hook must still emit its notice: {}", o.both());
    let ctx = additional_context(&o);
    assert!(
        ctx.contains("store UNREADABLE"),
        "unparseable done file must inject the UNREADABLE notice: {ctx}"
    );
    assert!(
        !ctx.contains("some pending item"),
        "the notice replaces the queue; it must not pretend to list it: {ctx}"
    );
}

/// README.md:26 / README.ja.md:35 -- the registered hook's timeout is 10s.
#[test]
fn claim_r_timeout_is_10s() {
    let raw = std::fs::read_to_string(manifest_dir().join("hooks/hooks.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let groups = v["hooks"]["SessionStart"].as_array().expect("SessionStart");
    let hooks: Vec<&serde_json::Value> = groups
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap().iter())
        .collect();
    assert_eq!(hooks.len(), 1, "exactly one registered hook: {raw}");
    assert!(hooks[0]["command"]
        .as_str()
        .unwrap()
        .ends_with("backlog session-start"));
    assert_eq!(hooks[0]["timeout"], 10, "timeout must be 10: {raw}");
}

// ----------------------------------------------------------- done / fail

/// README.md:8 / README.ja.md:8 -- exactly one of the three evidence forms is
/// required; two at once are refused with the status unchanged.
#[test]
fn claim_r_exactly_one_of_those_forms() {
    let f = Fixture::new("one-form");
    let (red, _head) = f.bug_then_fix();
    f.write("docs/guide.md", "# doc\n");
    let doc_commit = f.commit_paths(&["docs/guide.md"], "docs only");
    let id = f.add("target");
    let other = f.add("other");

    let combos: [&[&str]; 3] = [
        &["--doc-only", &doc_commit, "--duplicate-of", &other],
        &[
            "--test",
            "bash tests/check.sh",
            "--red-rev",
            &red,
            "--doc-only",
            &doc_commit,
        ],
        &[
            "--test",
            "bash tests/check.sh",
            "--red-rev",
            &red,
            "--duplicate-of",
            &other,
        ],
    ];
    for combo in combos {
        let mut args = vec!["done", id.as_str()];
        args.extend_from_slice(combo);
        let o = f.run(&args);
        assert_ne!(o.code, 0, "two evidence forms must be refused: {args:?}");
        assert!(
            o.both().contains("exactly one"),
            "refusal must say exactly one is required: {}",
            o.both()
        );
        assert_eq!(f.status(&id), "pending", "status must be unchanged");
    }

    // Each single form is accepted.
    let o = f.run(&["done", &id, "--doc-only", &doc_commit]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "done");
    let o = f.run(&[
        "done",
        &other,
        "--test",
        "bash tests/check.sh",
        "--red-rev",
        &red,
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&other), "done");
}

/// SKILL.md:16 -- `--duplicate-of` accepts a pending target, a target shown as
/// `claimed` (leased), and a done target.
#[test]
fn claim_s_pending_claimed_done() {
    let f = Fixture::new("dup-targets");
    let pending_t = f.add("pending target");
    let leased_t = f.add("leased target");
    let done_t = f.add("done target");
    let src_for_done = f.add("source that closes done target");
    // Close `done_t` as a duplicate of the pending target, so it is `done`.
    let o = f.run(&["done", &done_t, "--duplicate-of", &pending_t]);
    assert_eq!(o.code, 0, "{}", o.both());
    let _ = src_for_done;

    // Lease `leased_t` through the real ledger: claim until we hold it.
    let mut held = Vec::new();
    for _ in 0..4 {
        let o = f.run(&["next", "--claim"]);
        if o.code != 0 || o.stdout.contains("no pending") {
            break;
        }
        let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap();
        held.push(v["id"].as_str().unwrap().to_string());
    }
    assert!(held.contains(&leased_t), "claimed ids: {held:?}");
    assert_eq!(f.status(&leased_t), "claimed", "target must show claimed");

    // A fresh source for each target kind.
    for (target, label) in [
        (&pending_t, "pending"),
        (&leased_t, "claimed"),
        (&done_t, "done"),
    ] {
        let src = f.add(&format!("duplicate source for {label}"));
        let o = f.run(&["done", &src, "--duplicate-of", target]);
        assert_eq!(
            o.code,
            0,
            "--duplicate-of a {label} target must be accepted: {}",
            o.both()
        );
        assert_eq!(f.status(&src), "done");
        assert_eq!(f.row(&src)["closure"]["duplicate_of"], target.as_str());
    }
}

/// README.md:9,50 / SKILL.md:20 -- `fail` defers the re-run by exactly 2 days
/// and the row comes back as pending afterwards.
#[test]
fn claim_r_defers_the_re_run_by_2_days() {
    let f = Fixture::new("defer");
    let id = f.add("will fail");
    let o = f.run(&["fail", &id, "--reason", "blocked"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let row = f.row(&id);
    let defer = row["defer_until"].as_i64().expect("defer_until set");
    let upd = row["updated_at"].as_i64().unwrap();
    assert_eq!(
        defer,
        upd + 172_800,
        "exactly 2 days after updated_at: {row}"
    );
    assert_eq!(row["status"], "deferred", "inside the window: {row}");

    // `next` does not hand out a deferred row.
    let o = f.run(&["next"]);
    assert!(
        !o.stdout.contains(&id),
        "deferred row must not be handed out by next: {}",
        o.both()
    );

    // Once the deferral is in the past, the SessionStart requeue returns it
    // to pending.
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    let past = format!("defer_until = {}", now_unix() - 10);
    let rewritten: String = text
        .lines()
        .map(|l| {
            if l.starts_with("defer_until") {
                past.clone()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(f.tasks_path(), rewritten + "\n").unwrap();
    let o = session_start(&f);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending", "requeued as pending");
}

/// SKILL.md:20 -- `fail` marks the item failed and it is re-queued after 2 days.
#[test]
fn claim_s_skill_20() {
    let f = Fixture::new("skill20");
    let id = f.add("item to fail");
    let before = now_unix();
    let o = f.run(&["fail", &id, "--reason", "x"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after = now_unix();
    let defer = f.row(&id)["defer_until"].as_i64().expect("defer_until");
    assert!(
        defer >= before + 172_800 && defer <= after + 172_800,
        "defer_until must be fail time + 172800: {defer} not in [{}, {}]",
        before + 172_800,
        after + 172_800
    );
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    assert!(
        text.contains("status = \"failed\""),
        "stored failed: {text}"
    );
}

// ---------------------------------------------------------- overlap / edit

/// README.md:12 -- `overlap` groups queued tasks by declared `touched_files`
/// scope; undeclared ones are listed separately and never inside a group.
#[test]
fn claim_r_overlap() {
    let f = Fixture::new("overlap");
    let label = f.add_with_label("seed").1;
    let mk = |id: &str, title: &str, files: &str| {
        task_block(
            id,
            title,
            &label,
            "pending",
            &format!("touched_files = {files}"),
        )
        .replace("touched_files = []\n", "")
    };
    plant(
        &f,
        ".backlog/tasks.toml",
        &mk(
            "aaaa0001",
            "overlap one",
            r#"["src/a.rs", "src/shared.rs"]"#,
        ),
    );
    plant(
        &f,
        ".backlog/tasks.toml",
        &mk("aaaa0002", "overlap two", r#"["src/shared.rs"]"#),
    );
    plant(
        &f,
        ".backlog/tasks.toml",
        &mk("aaaa0003", "disjoint three", r#"["docs/other.md"]"#),
    );
    // aaaa0004 declares nothing (touched_files = []).
    plant(
        &f,
        ".backlog/tasks.toml",
        &task_block("aaaa0004", "undeclared four", &label, "pending", ""),
    );

    let o = f.run(&["overlap"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let group_lines: Vec<&str> = o.stdout.lines().filter(|l| l.contains("group:")).collect();
    let g = group_lines
        .iter()
        .find(|l| l.contains("aaaa0001"))
        .unwrap_or_else(|| panic!("no group holds aaaa0001: {}", o.stdout));
    assert!(
        g.contains("aaaa0002"),
        "overlapping tasks share a group: {g}"
    );
    assert!(
        !g.contains("aaaa0003"),
        "a disjoint task must not share the overlap group: {g}"
    );
    for l in &group_lines {
        assert!(
            !l.contains("aaaa0004"),
            "an undeclared task must never be inside a group: {l}"
        );
    }
    // Undeclared tasks are listed separately (after the `undeclared:` line).
    let tail = o
        .stdout
        .split("undeclared:")
        .nth(1)
        .expect("undeclared section");
    assert!(tail.contains("aaaa0004"), "undeclared list: {tail}");
    assert!(
        !tail.contains("aaaa0001") && !tail.contains("aaaa0002"),
        "declared tasks must not be listed as undeclared: {tail}"
    );
}

/// README.md:13 -- `edit <id>` updates title / tags / notes / status, other
/// fields unchanged.
#[test]
fn claim_r_edit_id() {
    let f = Fixture::new("edit");
    let id = f.add("original title");
    let before = f.row(&id);
    let o = f.run(&[
        "edit",
        &id,
        "--title",
        "new title",
        "--tag",
        "alpha",
        "--tag",
        "beta",
        "--notes",
        "new notes",
        "--status",
        "failed",
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after = f.row(&id);
    assert_eq!(after["title"], "new title");
    assert_eq!(after["tags"], serde_json::json!(["alpha", "beta"]));
    assert_eq!(after["notes"], "new notes");
    assert_eq!(f.status_stored(&id), "failed");
    for k in ["id", "project", "created_at", "weight", "repro"] {
        assert_eq!(before[k], after[k], "field {k} must be unchanged");
    }

    // Each field alone leaves the others alone.
    let o = f.run(&["edit", &id, "--title", "only title"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let again = f.row(&id);
    assert_eq!(again["title"], "only title");
    assert_eq!(again["notes"], "new notes");
    assert_eq!(again["tags"], serde_json::json!(["alpha", "beta"]));
}

trait StoredStatus {
    fn status_stored(&self, id: &str) -> String;
}
impl StoredStatus for Fixture {
    /// Status as stored in tasks.toml (list --json may show derived `deferred`).
    fn status_stored(&self, id: &str) -> String {
        let text = std::fs::read_to_string(self.tasks_path()).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        v["task"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("{id} not in tasks.toml"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

// ------------------------------------------------------ install / uninstall

fn settings_path(f: &Fixture) -> PathBuf {
    f.home.join(".claude").join("settings.json")
}

fn backlog_groups(settings: &serde_json::Value) -> Vec<serde_json::Value> {
    settings["hooks"]["SessionStart"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| {
            g["hooks"].as_array().is_some_and(|hs| {
                hs.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.ends_with("backlog session-start"))
                })
            })
        })
        .collect()
}

/// README.md:16,27,53 -- `install` merges exactly one ownership-marked
/// SessionStart group running `backlog session-start` (idempotent);
/// `uninstall` removes it and nothing else.
#[test]
fn claim_r_wire_remove_the_sessionstart_hook() {
    let f = Fixture::new("install");
    std::fs::create_dir_all(f.home.join(".claude")).unwrap();
    let other = serde_json::json!({
        "theme": "dark",
        "hooks": { "SessionStart": [
            { "hooks": [ { "type": "command", "command": "echo unrelated-hook", "timeout": 3 } ] }
        ] }
    });
    std::fs::write(
        settings_path(&f),
        serde_json::to_string_pretty(&other).unwrap(),
    )
    .unwrap();

    let o = f.run(&["install"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after1 = std::fs::read_to_string(settings_path(&f)).unwrap();
    let v1: serde_json::Value = serde_json::from_str(&after1).unwrap();
    assert_eq!(
        backlog_groups(&v1).len(),
        1,
        "exactly one backlog group: {after1}"
    );
    assert_eq!(v1["theme"], "dark", "unrelated keys kept");
    assert_eq!(
        v1["hooks"]["SessionStart"].as_array().unwrap().len(),
        2,
        "unrelated hook group kept next to ours: {after1}"
    );

    // Idempotent: a second install changes nothing.
    let o = f.run(&["install"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after2 = std::fs::read_to_string(settings_path(&f)).unwrap();
    assert_eq!(after1, after2, "second install must be a no-op");

    // uninstall removes the backlog group and nothing else.
    let o = f.run(&["uninstall"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v3: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings_path(&f)).unwrap()).unwrap();
    assert!(
        backlog_groups(&v3).is_empty(),
        "backlog group removed: {v3}"
    );
    assert_eq!(v3, other, "everything else is exactly as it was: {v3}");
}

/// README.md:57 / README.ja.md:35 -- `install` / `uninstall` accept `--dry-run`:
/// the resulting settings are printed and nothing is written.
#[test]
fn claim_r_accept_dry_run() {
    let f = Fixture::new("dryrun");
    // Absent settings.json: dry-run install prints the result, writes nothing.
    let o = f.run(&["install", "--dry-run"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let printed: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert_eq!(backlog_groups(&printed).len(), 1, "printed: {}", o.stdout);
    assert!(
        !settings_path(&f).exists(),
        "dry-run install must not create settings.json"
    );

    // Present settings.json: both dry runs leave the bytes identical.
    let o = f.run(&["install"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let before = std::fs::read(settings_path(&f)).unwrap();
    let o = f.run(&["uninstall", "--dry-run"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let printed: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert!(backlog_groups(&printed).is_empty(), "printed: {}", o.stdout);
    assert_eq!(std::fs::read(settings_path(&f)).unwrap(), before);
    let o = f.run(&["install", "--dry-run"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(std::fs::read(settings_path(&f)).unwrap(), before);
}

// ------------------------------------------------------------- list claims

/// README.md:99 -- every element of `list --json` carries a `hashkey`
/// (title + project, FNV-1a); it is computed, not stored.
#[test]
fn claim_r_carries_a_hashkey_field() {
    let f = Fixture::new("hashkey");
    let a = f.add("Hello   World");
    let b = f.add("another thing");
    let rows = f.list_all();
    assert_eq!(rows.len(), 2);
    for (id, title) in [(&a, "hello world"), (&b, "another thing")] {
        let row = rows.iter().find(|r| r["id"] == id.as_str()).unwrap();
        let project = row["project"].as_str().unwrap();
        let expected = format!(
            "{:016x}",
            harness_core::hash::fnv1a64(format!("{title}\u{1f}{project}").as_bytes())
        );
        assert_eq!(row["hashkey"], expected.as_str(), "row: {row}");
    }
    let stored = std::fs::read_to_string(f.tasks_path()).unwrap();
    assert!(
        !stored
            .lines()
            .any(|l| l.trim_start().starts_with("hashkey")),
        "hashkey must not be stored in tasks.toml: {stored}"
    );
}

// ------------------------------------------------------------- sync claims

fn sync_fixture(tag: &str) -> (Fixture, PathBuf) {
    let f = Fixture::new(tag);
    f.git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
    let log = f.shim.join("gh.log");
    let stub = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then echo \"https://github.com/o/r/issues/900\"; fi\nexit 0\n",
        log = log.display()
    );
    f.write_exec_shim("gh", &stub);
    (f, log)
}

fn ghlog(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// A done row whose issue was never confirmed closed -> one `close` action.
fn done_with_issue(id: &str, title: &str, project: &str, n: u32) -> String {
    task_block(
        id,
        title,
        project,
        "done",
        &format!("issue_number = {n}\nissue_url = \"https://github.com/o/r/issues/{n}\""),
    )
}

/// README.md:14 -- `sync` is one-way, local wins: `--apply` changes the remote
/// issue to match local state; remote state is never read back over a local row.
#[test]
fn claim_r_reconcile_the_store_against() {
    let (f, log) = sync_fixture("sync-oneway");
    let label = f.add_with_label("seed").1;
    // Local says done; the issue (41) is presumed open remotely -> remote is changed.
    plant(
        &f,
        ".backlog/tasks.done.toml",
        &done_with_issue("dd000001", "local done", &label, 41),
    );
    // Local says pending and mirrored (issue 42); whatever the remote says,
    // the local row must not change and no remote call is made for it.
    plant(
        &f,
        ".backlog/tasks.toml",
        &task_block(
            "pp000001",
            "local pending mirrored",
            &label,
            "pending",
            "issue_number = 42\nissue_url = \"https://github.com/o/r/issues/42\"",
        ),
    );
    let _ = std::fs::remove_file(&log);
    let o = f.run(&["sync", "--apply", "--only", "close"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let g = ghlog(&log);
    assert_eq!(
        g.matches("issue close 41 ").count(),
        1,
        "remote issue 41 is closed to match local: {g}"
    );
    for forbidden in [
        "close 42",
        "issue view",
        "issue list",
        "issue edit",
        "issue create",
        "api ",
    ] {
        assert!(
            !g.contains(forbidden),
            "no remote read or other write ({forbidden}): {g}"
        );
    }
    let row = f.row("pp000001");
    assert_eq!(row["status"], "pending", "local row not overwritten: {row}");
    assert_eq!(row["issue_number"], 42);
    assert!(
        row.get("issue_closed_at").is_none_or(|v| v.is_null()),
        "{row}"
    );
}

/// README.md:14 -- `--limit` truncates the plan to that many tasks (0 = no cap).
#[test]
fn claim_r_limit() {
    let (f, _log) = sync_fixture("sync-limit");
    let label = f.add_with_label("seed").1;
    for (i, n) in [(1, 51), (2, 52), (3, 53)] {
        plant(
            &f,
            ".backlog/tasks.done.toml",
            &done_with_issue(&format!("dd00000{i}"), &format!("done {i}"), &label, n),
        );
    }
    let unlimited = f.run(&["sync", "--only", "close"]);
    assert_eq!(unlimited.code, 0, "{}", unlimited.both());
    assert!(
        unlimited.stdout.contains("3 issue(s) to close"),
        "baseline plan has 3 closes: {}",
        unlimited.stdout
    );
    let limited = f.run(&["sync", "--only", "close", "--limit", "2"]);
    assert_eq!(limited.code, 0, "{}", limited.both());
    assert!(
        limited.stdout.contains("2 issue(s) to close"),
        "--limit 2 truncates the plan to 2: {}",
        limited.stdout
    );
    assert_eq!(
        limited
            .stdout
            .lines()
            .filter(|l| l.contains("  close   #"))
            .count(),
        2,
        "only 2 actions printed: {}",
        limited.stdout
    );
    let zero = f.run(&["sync", "--only", "close", "--limit", "0"]);
    assert!(
        zero.stdout.contains("3 issue(s) to close"),
        "--limit 0 means unlimited: {}",
        zero.stdout
    );

    // And with --apply, only the first N actions are performed.
    let o = f.run(&["sync", "--only", "close", "--limit", "1", "--apply"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(
        ghlog(&_log).matches("issue close").count(),
        1,
        "exactly one close performed: {}",
        ghlog(&_log)
    );
}

// ----------------------------------------------------- lock probe (progress)

fn lock_cmd(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Out {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    c.args(args)
        .env("HOME", home)
        .env("PATH", common::path_with_condukt_shim())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.spawn().unwrap().wait_with_output().unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn plain_git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn probe_verdict(home: &Path, project: &str) -> String {
    let o = lock_cmd(
        home,
        &["lock", "probe", "--project", project, "--json"],
        &[("HARNESS_PROGRESS_WINDOW_SECS", "0")],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap();
    v["verdict"].as_str().unwrap().to_string()
}

/// README.md:18 -- `probe` judges the holder's progress (git HEAD + transcript
/// growth): moved/grew -> progressing, frozen across the window -> stalled,
/// unreadable signals -> undetermined.
#[test]
fn claim_r_probe_judges() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let repo = td.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    plain_git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "1").unwrap();
    plain_git(&repo, &["add", "."]);
    plain_git(&repo, &["commit", "-qm", "one"]);
    let project = repo.to_string_lossy().to_string();
    let transcripts = home.join(".claude").join("projects").join("-fake");
    std::fs::create_dir_all(&transcripts).unwrap();
    let transcript = transcripts.join("holder.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();

    let o = lock_cmd(
        &home,
        &[
            "lock",
            "acquire",
            "--session-id",
            "holder",
            "--project",
            &project,
        ],
        &[],
    );
    assert_eq!(o.code, 0, "{}", o.both());

    // First sample anchors the fingerprint: single sample => undetermined.
    assert_eq!(probe_verdict(&home, &project), "undetermined");
    // Second sample, both signals frozen across the (0s) window => stalled.
    assert_eq!(probe_verdict(&home, &project), "stalled");
    // git HEAD moved => progressing.
    std::fs::write(repo.join("a.txt"), "2").unwrap();
    plain_git(&repo, &["commit", "-qam", "two"]);
    assert_eq!(probe_verdict(&home, &project), "progressing");
    assert_eq!(probe_verdict(&home, &project), "stalled", "frozen again");
    // transcript grew => progressing.
    std::fs::write(
        &transcript,
        "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n",
    )
    .unwrap();
    assert_eq!(probe_verdict(&home, &project), "progressing");

    // Unreadable signals: a holder whose project is not a git repo and that has
    // no transcript can never be judged -- undetermined, not stalled.
    let home2 = td.path().join("home2");
    std::fs::create_dir_all(&home2).unwrap();
    let nogit = td.path().join("nogit");
    std::fs::create_dir_all(&nogit).unwrap();
    let nogit_s = nogit.to_string_lossy().to_string();
    let o = lock_cmd(
        &home2,
        &[
            "lock",
            "acquire",
            "--session-id",
            "ghost",
            "--project",
            &nogit_s,
        ],
        &[],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    for _ in 0..3 {
        assert_eq!(probe_verdict(&home2, &nogit_s), "undetermined");
    }
}

// ------------------------------------------------- snapshots / small helpers

/// Every regular file under `root` (skipping `.git`), path -> bytes.
fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd {
            let e = e.unwrap();
            let p = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                walk(&p, out);
            } else if ft.is_file() {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            } else {
                out.insert(p, b"<non-regular>".to_vec());
            }
        }
    }
    let mut m = std::collections::BTreeMap::new();
    walk(root, &mut m);
    m
}

fn claims_dir(f: &Fixture) -> PathBuf {
    f.home.join(".backlog").join("claims")
}

/// The single `*.json` ledger under `$HOME/.backlog/claims`.
fn ledger_file(f: &Fixture) -> PathBuf {
    let mut v: Vec<PathBuf> = std::fs::read_dir(claims_dir(f))
        .expect("claims dir exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(v.len(), 1, "expected exactly one ledger file: {v:?}");
    v.pop().unwrap()
}

/// `next --claim`, asserting it handed out a task; returns its id.
fn claim_id(f: &Fixture) -> String {
    let o = f.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "claim must succeed: {}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("claim stdout not JSON ({e}): {:?}", o.stdout));
    v["id"].as_str().unwrap().to_string()
}

/// macOS `chflags uchg`: the file stays readable but cannot be replaced or
/// written. Used to make exactly one later write fail (fault injection).
struct Immutable(PathBuf);
impl Immutable {
    fn set(path: &Path) -> Immutable {
        let st = Command::new("chflags").arg("uchg").arg(path).status();
        assert!(
            st.is_ok_and(|s| s.success()),
            "chflags uchg {} failed: this fault-injection needs macOS BSD file flags",
            path.display()
        );
        Immutable(path.to_path_buf())
    }
}
impl Drop for Immutable {
    fn drop(&mut self) {
        let _ = Command::new("chflags").arg("nouchg").arg(&self.0).status();
    }
}

fn stored_rows(path: &Path) -> Vec<toml::Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let v: toml::Value = toml::from_str(&text).unwrap();
    v.get("task")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default()
}

fn orphan_dir(td: &tempfile::TempDir, name: &str) -> PathBuf {
    let d = td.path().join(name);
    std::fs::create_dir_all(&d).unwrap();
    let d = d.canonicalize().unwrap();
    let mut cur = Some(d.as_path());
    while let Some(c) = cur {
        assert!(
            !c.join(".git").exists(),
            "{} is inside a git repo; the test would be vacuous",
            d.display()
        );
        cur = c.parent();
    }
    d
}

// --------------------------------------------- next outside any repository

fn pending_block_with_repro(id: &str, title: &str, project: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{project}\"\n\
         tags = [\"p1\"]\nstatus = \"pending\"\nnotes = \"\"\n\
         created_at = 1000\nupdated_at = 1000\nweight = 0.0\n[task.repro]\noutcome = \"reproduced\"\ncmd = \"cargo test -p fixture\"\nobserved_at = 1000\ndetail = \"fixture\"\n\n"
    )
}

fn run_in(home: &Path, cwd: &Path, args: &[&str]) -> Out {
    let mut c = Command::new(env!("CARGO_BIN_EXE_backlog"));
    c.args(args)
        .env("HOME", home)
        .env("PATH", common::path_with_condukt_shim())
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.spawn().unwrap().wait_with_output().unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn next_outside_repo(claim: bool) {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    let cwd = orphan_dir(&td, "orphan");
    // A planted pending row in the cross-project store `next` must not read.
    std::fs::write(
        home.join(".backlog").join("tasks.toml"),
        pending_block_with_repro("shared001", "must not be returned", &cwd.to_string_lossy()),
    )
    .unwrap();
    let before = snapshot(&home);
    let args: &[&str] = if claim {
        &["next", "--claim"]
    } else {
        &["next"]
    };
    let o = run_in(&home, &cwd, args);
    assert_ne!(
        o.code,
        0,
        "next outside any repo must be refused: {}",
        o.both()
    );
    assert!(
        !o.stderr.trim().is_empty(),
        "refusal must carry a reason on stderr"
    );
    assert!(
        !o.stdout.contains("shared001") && !o.stdout.contains("no pending"),
        "~/.backlog/tasks.toml must not be read (nor reported empty): {:?}",
        o.stdout
    );
    assert_eq!(
        snapshot(&home),
        before,
        "~/.backlog must be neither read-modified nor written"
    );
}

/// README.md:124 -- outside any repo, `next` refuses the same way as `add` /
/// `list` (non-zero, reason on stderr, shared store untouched).
#[test]
fn claim_r_next_refuses_the_same_way() {
    next_outside_repo(false);
    next_outside_repo(true);
}

/// SKILL.md:43 -- `next` is refused from a cwd with no repo root above it and
/// does not read `~/.backlog/tasks.toml`.
#[test]
fn claim_s_repo_root_cwd_next() {
    next_outside_repo(false);
}

// -------------------------------------------------------- done-file claims

/// README.md:136 -- the done file of `<dir>/<stem>.toml` is
/// `<dir>/<stem>.done.toml`. ONLY the default stem is reachable through the
/// binary (the store file name is fixed to `tasks.toml`); the general rule in
/// `store::done_path` is not observable from outside. This pins the reachable
/// instance: terminal rows land in `tasks.done.toml`, the sibling of
/// `tasks.toml` in the same directory.
#[test]
fn claim_r_dir_stem_done_toml() {
    let f = Fixture::new("stem");
    let a = f.add("keeper");
    let b = f.add("closer");
    let o = f.run(&["done", &b, "--duplicate-of", &a]);
    assert_eq!(o.code, 0, "{}", o.both());
    let done = f.tasks_path().with_file_name("tasks.done.toml");
    assert_eq!(done, f.done_path());
    let rows = stored_rows(&done);
    assert_eq!(rows.len(), 1, "terminal row in the sibling done file");
    assert_eq!(rows[0]["id"].as_str(), Some(b.as_str()));
    let live = stored_rows(&f.tasks_path());
    assert!(live.iter().all(|r| r["id"].as_str() != Some(b.as_str())));
    assert!(
        f.repo.join(".backlog").read_dir().unwrap().all(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            !n.ends_with(".done.toml") || n == "tasks.done.toml"
        }),
        "no other done file name"
    );
}

/// README.md:138 -- `sync`, the duplicate guard and the near-duplicate scan
/// see done-file rows too (`sync` plans against them; a done row does not
/// block a re-add; terminal rows are never reported as near-duplicate peers).
#[test]
fn claim_r_sync_the_duplicate_guard_and_the_near_du() {
    let (f, log) = sync_fixture("donefile-readers");
    let label = f.add_with_label("seed").1;
    f.write(
        ".backlog/tasks.done.toml",
        &done_with_issue("dd000009", "Already shipped feature alpha", &label, 77),
    );
    // sync sees the done-file-only row: it plans a close for it.
    let o = f.run(&["sync", "--only", "close"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains("1 issue(s) to close") && o.stdout.contains("dd000009"),
        "sync must plan against the done-file row: {}",
        o.stdout
    );
    let _ = std::fs::remove_file(&log);
    // The duplicate guard decides as before: a done row does not block a re-add,
    // and the near-duplicate scan does not offer a terminal row as a peer.
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "Already shipped feature alpha",
        "--project",
        &p,
        "--repro-test",
        "bash tests/repro_yes.sh",
    ]);
    assert_eq!(o.code, 0, "done row must not block a re-add: {}", o.both());
    assert!(
        !o.stderr.contains("dd000009"),
        "a terminal done-file row is not a near-duplicate peer: {}",
        o.stderr
    );
}

/// README.md:149 -- with an unparseable `tasks.done.toml`, add / done / edit /
/// fail / sync exit non-zero naming that file and never write either store file.
#[test]
fn claim_r_add_done_edit_fail_and_sync_exit_non_zer() {
    let f = Fixture::new("badfile");
    let a = f.add("alpha");
    let b = f.add("beta");
    f.write(".backlog/tasks.done.toml", "this is [[ not valid toml");
    let tasks_before = std::fs::read(f.tasks_path()).unwrap();
    let done_before = std::fs::read(f.done_path()).unwrap();
    let p = f.project();
    let cmds: Vec<(&str, Vec<&str>)> = vec![
        (
            "add",
            vec![
                "add",
                "--title",
                "gamma",
                "--project",
                &p,
                "--repro-test",
                "bash tests/repro_yes.sh",
            ],
        ),
        ("done", vec!["done", &a, "--duplicate-of", &b]),
        ("edit", vec!["edit", &a, "--title", "renamed"]),
        ("fail", vec!["fail", &a, "--reason", "x"]),
        ("sync", vec!["sync"]),
        ("sync --apply", vec!["sync", "--apply"]),
    ];
    for (name, args) in cmds {
        let o = f.run(&args);
        assert_ne!(o.code, 0, "{name} must exit non-zero: {}", o.both());
        assert!(
            o.stderr.contains("tasks.done.toml"),
            "{name} must name tasks.done.toml: {}",
            o.stderr
        );
        assert_eq!(
            std::fs::read(f.tasks_path()).unwrap(),
            tasks_before,
            "{name} must not rewrite tasks.toml"
        );
        assert_eq!(
            std::fs::read(f.done_path()).unwrap(),
            done_before,
            "{name} must not overwrite the done file"
        );
    }
}

/// A store with two rows already in the done file (written by the real CLI, so
/// they are in the writer's canonical form), plus a pending row kept open.
fn store_with_done_rows(tag: &str) -> (Fixture, String, String, String) {
    let f = Fixture::new(tag);
    let keeper = f.add("keeper that stays pending");
    let x = f.add("row X closed first");
    let y = f.add("row Y closed second");
    for id in [&x, &y] {
        let o = f.run(&["done", id, "--duplicate-of", &keeper]);
        assert_eq!(o.code, 0, "{}", o.both());
    }
    (f, keeper, x, y)
}

/// Text of the `[[task]]` block for `id` in a store file.
fn block_of(path: &Path, id: &str) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    text.split("[[task]]")
        .find(|b| b.contains(&format!("id = \"{id}\"")))
        .unwrap_or_else(|| panic!("no block for {id} in {text}"))
        .trim()
        .to_string()
}

/// README.md:154 -- rows already in the done file keep their position and are
/// rewritten in place only if their content changed; a no-change save leaves
/// the file byte-identical.
#[test]
fn claim_r_rows_already_in_the_done_file_keep_their() {
    let (f, _keeper, x, y) = store_with_done_rows("inplace");
    // Stamp an issue onto X (first done row) so a sync will change X only.
    let text = std::fs::read_to_string(f.done_path()).unwrap();
    let stamped = text.replacen(
        &format!("id = \"{x}\"\n"),
        &format!(
            "id = \"{x}\"\nissue_number = 41\nissue_url = \"https://github.com/o/r/issues/41\"\n"
        ),
        1,
    );
    assert_ne!(text, stamped);
    std::fs::write(f.done_path(), &stamped).unwrap();
    f.git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
    f.write_exec_shim("gh", "#!/bin/sh\nexit 0\n");
    let y_before = block_of(&f.done_path(), &y);

    let o = f.run(&["sync", "--only", "close", "--apply"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let after_rows = stored_rows(&f.done_path());
    let ids: Vec<&str> = after_rows
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![x.as_str(), y.as_str()], "order preserved");
    assert!(
        after_rows[0].get("issue_closed_at").is_some(),
        "X rewritten in place with the sync stamp"
    );
    assert_eq!(block_of(&f.done_path(), &y), y_before, "Y untouched");

    // A save that changes nothing in the done file leaves it byte-identical.
    let bytes = std::fs::read(f.done_path()).unwrap();
    let mtime = std::fs::metadata(f.done_path())
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let _ = f.add("unrelated new pending row");
    assert_eq!(std::fs::read(f.done_path()).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(f.done_path())
            .unwrap()
            .modified()
            .unwrap(),
        mtime,
        "done file not rewritten when nothing in it changed"
    );
}

/// README.md:154 -- the done file is written first. Order is observed by
/// making the SECOND write (the `tasks.toml` replace) fail: the done file
/// already has the row.
#[test]
fn claim_r_done_file_is_written_first() {
    let (f, keeper, _x, _y) = store_with_done_rows("order");
    let z = f.add("row Z still pending");
    let done_before = std::fs::read(f.done_path()).unwrap();
    {
        let _lock = Immutable::set(&f.tasks_path());
        let o = f.run(&["done", &z, "--duplicate-of", &keeper]);
        assert_ne!(o.code, 0, "the second write was made to fail: {}", o.both());
    }
    let done_after = std::fs::read(f.done_path()).unwrap();
    assert_ne!(
        done_after, done_before,
        "done file changed -> it was written"
    );
    assert!(
        stored_rows(&f.done_path())
            .iter()
            .any(|r| r["id"].as_str() == Some(z.as_str())),
        "done file written before tasks.toml (it holds Z although tasks.toml's replace failed)"
    );
    assert!(
        stored_rows(&f.tasks_path()).iter().any(|r| {
            r["id"].as_str() == Some(z.as_str()) && r["status"].as_str() == Some("pending")
        }),
        "tasks.toml was not yet updated"
    );
}

/// README.md:156 -- a crash between the two writes leaves the row in both
/// files; terminal-wins resolves it (listed once, as terminal, never handed out).
#[test]
fn claim_r_a_crash_between_the_two_writes() {
    let f = Fixture::new("crash");
    let z = f.add("row Z");
    let keeper = f.add("keeper");
    {
        let _lock = Immutable::set(&f.tasks_path());
        let o = f.run(&["done", &z, "--duplicate-of", &keeper]);
        assert_ne!(o.code, 0, "second write made to fail: {}", o.both());
    }
    assert!(stored_rows(&f.done_path())
        .iter()
        .any(|r| r["id"].as_str() == Some(z.as_str())));
    assert!(stored_rows(&f.tasks_path())
        .iter()
        .any(|r| r["id"].as_str() == Some(z.as_str())));
    let rows: Vec<serde_json::Value> = f
        .list_all()
        .into_iter()
        .filter(|r| r["id"] == z.as_str())
        .collect();
    assert_eq!(rows.len(), 1, "row returned once: {rows:?}");
    assert_eq!(rows[0]["status"], "done", "returned as terminal");
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        !o.stdout.contains(&z),
        "terminal row never handed out: {}",
        o.stdout
    );
}

// ------------------------------------------------------- merge-driver claims

fn md_files(dir: &Path, base: &str, ours: &str, theirs: &str) -> [PathBuf; 3] {
    let b = dir.join("base.toml");
    let o = dir.join("ours.toml");
    let t = dir.join("theirs.toml");
    std::fs::write(&b, base).unwrap();
    std::fs::write(&o, ours).unwrap();
    std::fs::write(&t, theirs).unwrap();
    [b, o, t]
}

fn md_exec(dir: &Path, files: &[PathBuf; 3]) -> Out {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(["merge-driver"])
        .args(files)
        .env("HOME", dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn md_run(dir: &Path, base: &str, ours: &str, theirs: &str) -> (Out, PathBuf) {
    let files = md_files(dir, base, ours, theirs);
    let out = md_exec(dir, &files);
    (out, files[1].clone())
}

fn mrow(id: &str, title: &str) -> String {
    format!(
        "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"/x/proj\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"pending\"\nnotes = \"n-{id}\"\n\n"
    )
}

/// README.md:174 -- edit/delete and add/add-with-different-content are
/// conflicts too: non-zero exit, no side picked (`<ours>` untouched).
#[test]
fn claim_r_the_same_holds_for_edit_delete() {
    let td = tempfile::tempdir().unwrap();
    // edit (ours) vs delete (theirs)
    let base = format!("{}{}", mrow("a0000001", "orig"), mrow("b0000002", "keep"));
    let ours = format!(
        "{}{}",
        mrow("a0000001", "edited by ours"),
        mrow("b0000002", "keep")
    );
    let theirs = mrow("b0000002", "keep");
    let (o, ours_path) = md_run(td.path(), &base, &ours, &theirs);
    assert_ne!(o.code, 0, "edit/delete must conflict: {}", o.both());
    assert!(o.both().contains("a0000001"), "names the id: {}", o.both());
    assert_eq!(std::fs::read_to_string(&ours_path).unwrap(), ours);
    // delete (ours) vs edit (theirs)
    let theirs2 = format!(
        "{}{}",
        mrow("a0000001", "edited by theirs"),
        mrow("b0000002", "keep")
    );
    let ours2 = mrow("b0000002", "keep");
    let (o, ours_path) = md_run(td.path(), &base, &ours2, &theirs2);
    assert_ne!(o.code, 0, "delete/edit must conflict: {}", o.both());
    assert_eq!(std::fs::read_to_string(&ours_path).unwrap(), ours2);
    // same new id, different content
    let (o, ours_path) = md_run(
        td.path(),
        "",
        &mrow("c0000003", "added one way"),
        &mrow("c0000003", "added another way"),
    );
    assert_ne!(
        o.code,
        0,
        "add/add with different content must conflict: {}",
        o.both()
    );
    assert_eq!(
        std::fs::read_to_string(&ours_path).unwrap(),
        mrow("c0000003", "added one way")
    );
    // control: the same new id with IDENTICAL content merges cleanly.
    let (o, _) = md_run(
        td.path(),
        "",
        &mrow("c0000003", "same"),
        &mrow("c0000003", "same"),
    );
    assert_eq!(o.code, 0, "{}", o.both());
}

fn gitc(repo: &Path, args: &[&str]) -> Out {
    let o = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// README.md:174 -- on conflict `<ours>` is untouched and git leaves the file
/// unmerged for a human.
#[test]
fn claim_r_ours_is_left_untouched() {
    let td = tempfile::tempdir().unwrap();
    let repo = td.path().join("repo");
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    let must = |args: &[&str]| {
        let o = gitc(&repo, args);
        assert_eq!(o.code, 0, "git {args:?}: {}", o.both());
    };
    must(&["init", "-q", "-b", "main"]);
    must(&["config", "commit.gpgsign", "false"]);
    must(&[
        "config",
        "merge.backlog.driver",
        &format!("{} merge-driver %O %A %B", env!("CARGO_BIN_EXE_backlog")),
    ]);
    std::fs::write(
        repo.join(".gitattributes"),
        ".backlog/tasks.toml merge=backlog\n",
    )
    .unwrap();
    let store = repo.join(".backlog/tasks.toml");
    std::fs::write(&store, mrow("a0000001", "orig")).unwrap();
    must(&["add", "-A"]);
    must(&["commit", "-q", "-m", "base"]);
    must(&["checkout", "-q", "-b", "side"]);
    std::fs::write(&store, mrow("a0000001", "side edit")).unwrap();
    must(&["commit", "-qam", "side"]);
    must(&["checkout", "-q", "main"]);
    let ours = mrow("a0000001", "main edit");
    std::fs::write(&store, &ours).unwrap();
    must(&["commit", "-qam", "main"]);

    let m = gitc(&repo, &["merge", "side"]);
    assert_ne!(m.code, 0, "conflicting merge must fail: {}", m.both());
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        ours,
        "<ours> bytes untouched by the driver"
    );
    let st = gitc(&repo, &["status", "--porcelain"]);
    assert!(
        st.stdout
            .lines()
            .any(|l| l.starts_with("UU") && l.contains(".backlog/tasks.toml")),
        "file left unmerged for a human: {}",
        st.stdout
    );
}

/// README.md:177 -- a row without a string id, a duplicate id in one input, an
/// unknown top-level key: non-zero exit, `<ours>` untouched. (The fourth case,
/// "a result that does not re-parse to exactly the merged rows", needs a
/// writer defect to construct and is not reachable from inputs.)
#[test]
fn claim_r_a_row_without_a_string_id() {
    let td = tempfile::tempdir().unwrap();
    let good = format!("{}{}", mrow("a0000001", "one"), mrow("b0000002", "two"));
    let cases: Vec<(&str, String, String)> = vec![
        (
            "row without id",
            format!("{good}[[task]]\ntitle = \"no id here\"\n"),
            good.clone(),
        ),
        (
            "non-string id",
            format!("{good}[[task]]\nid = 7\ntitle = \"int id\"\n"),
            good.clone(),
        ),
        (
            "duplicate id in one input",
            good.clone(),
            format!("{good}{}", mrow("a0000001", "dup")),
        ),
        (
            "unknown top-level key",
            format!("version = 2\n\n{good}"),
            good.clone(),
        ),
    ];
    for (name, ours, theirs) in cases {
        let (o, ours_path) = md_run(td.path(), &good, &ours, &theirs);
        assert_ne!(o.code, 0, "{name}: must exit non-zero: {}", o.both());
        assert_eq!(
            std::fs::read_to_string(&ours_path).unwrap(),
            ours,
            "{name}: <ours> must be untouched"
        );
    }
    // Control: the clean inputs merge fine.
    let (o, _) = md_run(td.path(), &good, &good, &good);
    assert_eq!(o.code, 0, "{}", o.both());
}

/// README.md:179 -- a clean result is written atomically: `<ours>` is replaced
/// (temp + rename), so a hard link to the old file keeps the OLD content and
/// no temp file is left behind.
#[test]
fn claim_r_a_clean_result_is_written_atomically() {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().join("w");
    std::fs::create_dir_all(&dir).unwrap();
    let base = mrow("a0000001", "one");
    let ours = base.clone();
    let theirs = format!("{base}{}", mrow("b0000002", "added by theirs"));
    let files = md_files(&dir, &base, &ours, &theirs);
    let link = dir.join("ours.link");
    std::fs::hard_link(&files[1], &link).unwrap();
    let o = md_exec(&dir, &files);
    assert_eq!(o.code, 0, "{}", o.both());
    let merged = std::fs::read_to_string(&files[1]).unwrap();
    assert!(merged.contains("b0000002"), "merge applied: {merged}");
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        ours,
        "the old inode keeps its content: the driver replaced the file, it did not write in place"
    );
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("tmp")),
        "no temp file left behind: {names:?}"
    );
}

/// README.md:181 -- the same writer `backlog` uses renders the output, so
/// merging a store with itself is byte-identical; rows with fields this binary
/// does not know are kept (generic rendering).
#[test]
fn claim_r_output_is_rendered_with_the_same_writer() {
    let f = Fixture::new("samewriter");
    let _ = f.add("first row");
    let _ = f.add("second row");
    let store = std::fs::read_to_string(f.tasks_path()).unwrap();
    let td = tempfile::tempdir().unwrap();
    let (o, ours_path) = md_run(td.path(), &store, &store, &store);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(
        std::fs::read_to_string(&ours_path).unwrap(),
        store,
        "merging a store with itself is byte-identical"
    );

    // A row with an unknown field survives (and the merge still applies).
    let unknown = store.replacen(
        "status = \"pending\"\n",
        "status = \"pending\"\nfuture_field = \"kept\"\n",
        1,
    );
    assert_ne!(unknown, store);
    let (o, ours_path) = md_run(td.path(), &unknown, &unknown, &unknown);
    assert_eq!(o.code, 0, "{}", o.both());
    let out = std::fs::read_to_string(&ours_path).unwrap();
    assert!(
        out.contains("future_field = \"kept\""),
        "unknown field kept: {out}"
    );
    let rows: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(rows["task"].as_array().unwrap().len(), 2);
}

/// README.md:162 -- `.gitattributes` marks both store files `merge=backlog`.
#[test]
fn claim_r_gitattributes_marks_both() {
    let p = manifest_dir().join("../../.gitattributes");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let has = |path: &str| {
        text.lines().any(|l| {
            let mut it = l.split_whitespace();
            it.next() == Some(path) && it.any(|a| a == "merge=backlog")
        })
    };
    assert!(
        has(".backlog/tasks.toml"),
        "tasks.toml merge=backlog: {text}"
    );
    assert!(
        has(".backlog/tasks.done.toml"),
        "tasks.done.toml merge=backlog: {text}"
    );
}

// ----------------------------------------------- claim ledger / lease claims

/// README.md:200 -- `<project-slug>` (claim ledger name) is the same FNV-1a
/// project hash `backlog lock` uses.
#[test]
fn claim_r_project_slug_is_the_same_fnv_1a() {
    let f = Fixture::new("slug");
    let _ = f.add("claimable");
    let _ = claim_id(&f);
    let ledger = ledger_file(&f);
    let ledger_slug = ledger.file_stem().unwrap().to_string_lossy().into_owned();

    // The lock for the same project (the main working tree this linked
    // worktree normalizes to).
    let main_tree = f
        .repo
        .with_file_name(format!(
            "{}.main",
            f.repo.file_name().unwrap().to_string_lossy()
        ))
        .canonicalize()
        .unwrap();
    let o = f.run(&[
        "lock",
        "acquire",
        "--session-id",
        "s",
        "--project",
        &main_tree.to_string_lossy(),
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    let locks: Vec<String> = std::fs::read_dir(f.home.join(".backlog/locks"))
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        locks,
        vec![ledger_slug.clone()],
        "same slug for lock and ledger"
    );
    let expected = format!(
        "{:016x}",
        harness_core::hash::fnv1a64(main_tree.to_string_lossy().as_bytes())
    );
    assert_eq!(
        ledger_slug, expected,
        "the slug is FNV-1a of the project identity"
    );
}

fn spawn_claim(f: &Fixture) -> std::process::Child {
    let path = format!(
        "{}:{}",
        f.shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(["next", "--claim"])
        .env("HOME", &f.home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(&f.repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn lock_files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "lock"))
                .collect()
        })
        .unwrap_or_default()
}

/// README.md:201 -- lock order, never inverted: the project-wide ledger lock
/// first, then the per-checkout tasks-file lock. Observed from outside by
/// holding one lock and watching which the claimer has taken while it waits.
#[test]
fn claim_r_lock_order_never_inverted() {
    // (1) Hold the LEDGER lock. A claimer that takes ledger-then-tasks blocks
    // there and has NOT taken the tasks-file lock.
    let f = Fixture::new("lockorder-a");
    let _ = f.add("claimable");
    let _ = claim_id(&f); // creates claims dir + ledger
    let claims = claims_dir(&f);
    let ledger_lock = ledger_file(&f).with_extension("lock");
    std::fs::write(&ledger_lock, "held-by-test").unwrap();
    let tasks_lock = PathBuf::from(format!("{}.lock", f.tasks_path().display()));
    assert!(!tasks_lock.exists());
    let mut child = spawn_claim(&f);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "claimer must be blocked on the held ledger lock"
    );
    assert!(
        !tasks_lock.exists(),
        "while blocked on the ledger lock the claimer must NOT already hold the tasks-file lock (inverted order)"
    );
    std::fs::remove_file(&ledger_lock).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // (2) Hold the TASKS-FILE lock (live pid, so it is never reaped). The
    // claimer must already own the ledger lock while it waits for it.
    std::fs::write(&tasks_lock, std::process::id().to_string()).unwrap();
    let mut child = spawn_claim(&f);
    let mut saw_ledger_lock = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !lock_files_in(&claims).is_empty() {
            saw_ledger_lock = true;
            break;
        }
    }
    let still_waiting = child.try_wait().unwrap().is_none();
    std::fs::remove_file(&tasks_lock).unwrap();
    let _ = child.wait_with_output().unwrap();
    assert!(
        still_waiting,
        "claimer must be waiting on the held tasks-file lock"
    );
    assert!(
        saw_ledger_lock,
        "the ledger lock must already be held while waiting for the tasks-file lock"
    );
}

/// README.md:205 -- ledger entries are kept 7 days (pruned afterwards), even
/// though they stopped excluding after 1h.
#[test]
fn claim_r_kept_for_7_days() {
    let f = Fixture::new("retention");
    for t in ["t1", "t2", "t3"] {
        let _ = f.add(t);
    }
    let first = claim_id(&f);
    let ledger = ledger_file(&f);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    let template = v["entries"][0].clone();
    let now = now_unix();
    let mut old = template.clone();
    old["id"] = "old00001".into();
    old["claimed_at"] = (now - 8 * 24 * 3600).into();
    let mut mid = template.clone();
    mid["id"] = "mid00001".into();
    mid["claimed_at"] = (now - 2 * 3600).into(); // stale (>1h) but < 7 days
    let mut near = template;
    near["id"] = "near0001".into();
    near["claimed_at"] = (now - 6 * 24 * 3600).into();
    v["entries"]
        .as_array_mut()
        .unwrap()
        .extend([old, mid, near]);
    std::fs::write(&ledger, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let second = claim_id(&f);
    assert_ne!(first, second);
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    let ids: Vec<&str> = after["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"old00001"),
        "8-day-old entry pruned: {ids:?}"
    );
    assert!(
        ids.contains(&"mid00001"),
        "2h-old (stale) entry kept: {ids:?}"
    );
    assert!(ids.contains(&"near0001"), "6-day-old entry kept: {ids:?}");
    assert!(ids.contains(&first.as_str()) && ids.contains(&second.as_str()));
}

/// README.md:221 -- the ledger directory not being creatable, and the ledger
/// being unwritable, are refused (non-zero, reason on stderr), never
/// `no pending tasks` on exit 0.
#[test]
fn claim_r_the_ledger_directory_not_being_creatable() {
    // (a) A regular file sits where the claims directory must be created.
    let f = Fixture::new("ledgerdir");
    let id = f.add("claimable");
    std::fs::create_dir_all(f.home.join(".backlog")).unwrap();
    std::fs::write(claims_dir(&f), "i am a file, not a directory").unwrap();
    let o = f.run(&["next", "--claim"]);
    assert_ne!(o.code, 0, "unwritable claims dir must refuse: {}", o.both());
    assert!(!o.stderr.trim().is_empty(), "reason on stderr");
    assert!(!o.stdout.contains("no pending tasks"), "{:?}", o.stdout);
    assert!(
        !o.stdout.contains(&id),
        "no task handed out: {:?}",
        o.stdout
    );

    // (b) The ledger exists and is readable but cannot be replaced.
    let g = Fixture::new("ledgerfile");
    let _ = g.add("claimable one");
    let _ = g.add("claimable two");
    let _ = claim_id(&g);
    let ledger = ledger_file(&g);
    let _lock = Immutable::set(&ledger);
    let o = g.run(&["next", "--claim"]);
    assert_ne!(o.code, 0, "unwritable ledger must refuse: {}", o.both());
    assert!(!o.stderr.trim().is_empty(), "reason on stderr");
    assert!(!o.stdout.contains("no pending tasks"), "{:?}", o.stdout);
}

/// README.md:234 -- a done/cancelled row is never shown `claimed`, even with a
/// live lease in the ledger.
#[test]
fn claim_r_is_never_shown_claimed() {
    let f = Fixture::new("neverclaimed");
    let keeper = f.add("keeper");
    let to_done = f.add("to be done");
    let to_cancel = f.add("to be cancelled");
    // Lease every row, then terminate two of them (done/cancel do not release).
    let mut leased = Vec::new();
    for _ in 0..3 {
        leased.push(claim_id(&f));
    }
    for id in [&keeper, &to_done, &to_cancel] {
        assert!(leased.contains(id), "{id} leased: {leased:?}");
    }
    let o = f.run(&["done", &to_done, "--duplicate-of", &keeper]);
    assert_eq!(o.code, 0, "{}", o.both());
    let o = f.run(&["cancel", &to_cancel, "--reason", "not doing it"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&to_done), "done");
    assert_eq!(f.status(&to_cancel), "cancelled");
    assert_eq!(
        f.status(&keeper),
        "claimed",
        "control: a leased pending row IS claimed"
    );
    // Text renderer too.
    let o = f.run(&["list", "--all"]);
    for line in o.stdout.lines() {
        if line.contains(&to_done) || line.contains(&to_cancel) {
            assert!(
                !line.contains("claimed"),
                "terminal row shown claimed: {line}"
            );
        }
    }
}

/// README.md:243 -- `--status pending` omits leased tasks; `--status claimed`
/// selects them.
#[test]
fn claim_r_status_pending_omits_leased() {
    let f = Fixture::new("pendingomits");
    let _ = f.add("one");
    let _ = f.add("two");
    let leased = claim_id(&f);
    let ids = |status: &str| -> Vec<String> {
        let o = f.run(&["list", "--all", "--json", "--status", status]);
        assert_eq!(o.code, 0, "{}", o.both());
        serde_json::from_str::<Vec<serde_json::Value>>(o.stdout.trim())
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let pending = ids("pending");
    assert!(
        !pending.contains(&leased),
        "pending omits the leased row: {pending:?}"
    );
    assert_eq!(pending.len(), 1);
    assert_eq!(ids("claimed"), vec![leased]);
}

/// README.md:245 -- a row an older binary persisted as `status = "claimed"` is
/// rewritten as `pending` the next time an unrelated command saves the store.
#[test]
fn claim_r_it_is_rewritten_as_pending() {
    let f = Fixture::new("legacyclaimed");
    let (_seed, label) = f.add_with_label("seed row");
    f.plant_open("cc000001", "legacy claimed row", "claimed", &label);
    assert!(std::fs::read_to_string(f.tasks_path())
        .unwrap()
        .contains("status = \"claimed\""));
    let _ = f.add("unrelated add that saves the store");
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    assert!(
        !text.contains("status = \"claimed\""),
        "stored claimed must be rewritten: {text}"
    );
    let row = stored_rows(&f.tasks_path())
        .into_iter()
        .find(|r| r["id"].as_str() == Some("cc000001"))
        .unwrap();
    assert_eq!(row["status"].as_str(), Some("pending"));
}

/// README.md:247 -- `edit --status pending` does not release a lease.
#[test]
fn claim_r_edit_status_pending_does_not_release() {
    let f = Fixture::new("editnorelease");
    let _ = f.add("one");
    let _ = f.add("two");
    let leased = claim_id(&f);
    let o = f.run(&["edit", &leased, "--status", "pending"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        !o.stdout.contains(&leased),
        "plain next skips the leased row: {}",
        o.stdout
    );
    let again = f.run(&["next", "--claim"]);
    assert_eq!(again.code, 0, "{}", again.both());
    assert!(
        !again.stdout.contains(&leased),
        "next --claim does not re-hand the leased row after edit --status pending: {}",
        again.stdout
    );
    assert_eq!(f.status(&leased), "claimed");
}

/// README.md:250 -- `list`, plain `next` and `next --claim` locate the ledger
/// the same way: one ledger for the linked worktree and the main tree; outside
/// any repo the identity is the canonical cwd.
#[test]
fn claim_r_locate_the_ledger_the_same_way() {
    // (1) Linked worktree and its main tree share one ledger: corrupt the
    // ledger written by a claim in the worktree; list / plain next / claim must
    // all refuse from BOTH trees.
    let f = Fixture::new("ledgersame");
    let _ = f.add("claimable");
    let _ = claim_id(&f);
    std::fs::write(ledger_file(&f), "{ not json").unwrap();
    let main_tree = f.repo.with_file_name(format!(
        "{}.main",
        f.repo.file_name().unwrap().to_string_lossy()
    ));
    for (where_, cwd) in [
        ("linked worktree", f.repo.clone()),
        ("main tree", main_tree),
    ] {
        for args in [&["list"][..], &["next"][..], &["next", "--claim"][..]] {
            let o = run_in(&f.home, &cwd, args);
            assert_ne!(
                o.code,
                0,
                "{where_} {args:?}: must consult the shared (corrupt) ledger and refuse: {}",
                o.both()
            );
            assert!(
                o.stdout.trim().is_empty(),
                "{where_} {args:?}: nothing on stdout: {:?}",
                o.stdout
            );
        }
    }

    // (2) Outside any repo (pinned store): the ledger is keyed by the
    // canonical cwd, and list / next / claim all find it there.
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let pinned = td.path().join("pinned");
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::create_dir_all(&pinned).unwrap();
    let cwd = orphan_dir(&td, "orphan");
    std::fs::write(
        home.join(".backlog/config.toml"),
        format!("store_dir = \"{}\"\n", pinned.display()),
    )
    .unwrap();
    std::fs::write(
        pinned.join("tasks.toml"),
        pending_block_with_repro("pin00001", "pinned work", &cwd.to_string_lossy()),
    )
    .unwrap();
    let o = run_in(&home, &cwd, &["next", "--claim"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let expected = format!(
        "{:016x}.json",
        harness_core::hash::fnv1a64(cwd.to_string_lossy().as_bytes())
    );
    let ledger = home.join(".backlog/claims").join(&expected);
    assert!(
        ledger.is_file(),
        "ledger keyed by the canonical cwd: {expected}"
    );
    std::fs::write(&ledger, "{ not json").unwrap();
    for args in [&["list"][..], &["next"][..]] {
        let o = run_in(&home, &cwd, args);
        assert_ne!(
            o.code,
            0,
            "{args:?} must read the same ledger: {}",
            o.both()
        );
    }
}

/// README.md:257 -- the store-divergence check does not count leased tasks as
/// queued work.
#[test]
fn claim_r_store_divergence_check_does_not_count() {
    let f = Fixture::new("divergence");
    let (only, label) = f.add_with_label("only queued task");
    // A legacy store holding queued work for the same project.
    std::fs::create_dir_all(f.home.join(".backlog")).unwrap();
    std::fs::write(
        f.home.join(".backlog/tasks.toml"),
        pending_block_with_repro("legacy01", "stranded in legacy store", &label),
    )
    .unwrap();
    // Control: the unleased task IS queued work -> only a warning, list works.
    let o = f.run(&["list", "--all"]);
    assert_eq!(o.code, 0, "queued work present: warn only: {}", o.both());
    assert!(o.stdout.contains(&only), "{}", o.stdout);
    // Lease it: now the resolved store holds no queued work, so the divergence
    // check refuses the (otherwise empty-looking) answer.
    let leased = claim_id(&f);
    assert_eq!(leased, only);
    let o = f.run(&["list", "--all"]);
    assert_ne!(
        o.code,
        0,
        "leased task must not count as queued work: {}",
        o.both()
    );
    assert!(o.stderr.contains("holds no queued work"), "{}", o.stderr);
}

// -------------------------------------------------- read-only / peek claims

/// SKILL.md:11 -- `list --status pending` is a pure read: it writes nothing and
/// claims nothing.
#[test]
fn claim_s_read() {
    let f = Fixture::new("pureread");
    let _ = f.add("one");
    let _ = f.add("two");
    let repo_before = snapshot(&f.repo);
    let home_before = snapshot(&f.home);
    let o = f.run(&["list", "--status", "pending"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(o.stdout.contains("one") && o.stdout.contains("two"));
    assert_eq!(
        snapshot(&f.repo),
        repo_before,
        "no file in the repo changed"
    );
    assert_eq!(snapshot(&f.home), home_before, "nothing written under HOME");
    assert!(!claims_dir(&f).exists(), "no claim ledger created");
    assert_eq!(
        f.list_all()
            .iter()
            .filter(|r| r["status"] == "claimed")
            .count(),
        0
    );
}

/// SKILL.md:12 -- plain `next` peeks: it returns a pending row without
/// reserving it (no lease entry; a second `next` returns the same row).
#[test]
fn claim_s_prohibition_form_not_flagged_b() {
    let f = Fixture::new("peek");
    let id = f.add("peek at me");
    let home_before = snapshot(&f.home);
    let a = f.run(&["next"]);
    assert_eq!(a.code, 0, "{}", a.both());
    let va: serde_json::Value = serde_json::from_str(a.stdout.trim()).unwrap();
    assert_eq!(va["id"], id.as_str());
    let b = f.run(&["next"]);
    let vb: serde_json::Value = serde_json::from_str(b.stdout.trim()).unwrap();
    assert_eq!(vb["id"], id.as_str(), "second peek returns the same row");
    assert_eq!(f.status(&id), "pending", "still pending");
    assert!(!claims_dir(&f).exists(), "no lease ledger exists");
    assert_eq!(snapshot(&f.home), home_before, "nothing written under HOME");
}

// ------------------------------------------------------- driver / lock claims

fn json_of(o: &Out) -> serde_json::Value {
    serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("not JSON ({e}): {:?} / {:?}", o.stdout, o.stderr))
}

/// SKILL.md:27 -- `driver status` lists registered drivers (active / count /
/// drivers).
#[test]
fn claim_s_driver_active_count_drivers() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path();
    let none = lock_cmd(home, &["driver", "status", "--project", "/p"], &[]);
    let v = json_of(&none);
    assert_eq!(v["active"], false);
    assert_eq!(v["count"], 0);
    for s in ["sess-a", "sess-b"] {
        let o = lock_cmd(
            home,
            &["driver", "register", "--session-id", s, "--project", "/p"],
            &[],
        );
        assert_eq!(o.code, 0, "{}", o.both());
    }
    let o = lock_cmd(home, &["driver", "status", "--project", "/p"], &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v = json_of(&o);
    assert_eq!(v["active"], true, "{v}");
    assert_eq!(v["count"], 2, "{v}");
    let mut sessions: Vec<&str> = v["drivers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["session_id"].as_str().unwrap())
        .collect();
    sessions.sort();
    assert_eq!(sessions, vec!["sess-a", "sess-b"]);
}

/// SKILL.md:38 -- the driver registry and the exclusive lock are scoped per
/// project.
#[test]
fn claim_s_driver_project() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path();
    let o = lock_cmd(
        home,
        &[
            "lock",
            "acquire",
            "--session-id",
            "a",
            "--project",
            "/proj-a",
        ],
        &[],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    let o = lock_cmd(
        home,
        &[
            "driver",
            "register",
            "--session-id",
            "da",
            "--project",
            "/proj-a",
        ],
        &[],
    );
    assert_eq!(o.code, 0, "{}", o.both());

    // B sees neither A's lock nor A's driver.
    let st = lock_cmd(home, &["lock", "status", "--project", "/proj-b"], &[]);
    assert_eq!(st.code, 0, "{}", st.both());
    assert_eq!(
        st.stdout.trim(),
        "none",
        "project B is not reported busy: {}",
        st.stdout
    );
    let ds = json_of(&lock_cmd(
        home,
        &["driver", "status", "--project", "/proj-b"],
        &[],
    ));
    assert_eq!(ds["active"], false, "{ds}");
    // ... and A's lock does not block B's acquire.
    let o = lock_cmd(
        home,
        &[
            "lock",
            "acquire",
            "--session-id",
            "b",
            "--project",
            "/proj-b",
        ],
        &[],
    );
    assert_eq!(o.code, 0, "A's lock must not block B: {}", o.both());
    // Control: A is reported held by its own session, and A's lock still blocks A.
    let sa = lock_cmd(home, &["lock", "status", "--project", "/proj-a"], &[]);
    assert!(sa.stdout.contains("\"a\""), "{}", sa.stdout);
    let o = lock_cmd(
        home,
        &[
            "lock",
            "acquire",
            "--session-id",
            "x",
            "--project",
            "/proj-a",
        ],
        &[],
    );
    assert_ne!(o.code, 0, "same-project second acquire must fail");
}

/// SKILL.md:39 -- `--project` is required except for `status`.
#[test]
fn claim_s_project_status() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path();
    let required: [&[&str]; 7] = [
        &["driver", "register", "--session-id", "s"],
        &["driver", "heartbeat", "--session-id", "s"],
        &["driver", "unregister", "--session-id", "s"],
        &["lock", "acquire", "--session-id", "s"],
        &["lock", "release"],
        &["lock", "heartbeat", "--session-id", "s"],
        &["lock", "probe"],
    ];
    for args in required {
        let o = lock_cmd(home, args, &[]);
        assert_ne!(
            o.code,
            0,
            "{args:?} without --project must fail: {}",
            o.both()
        );
        assert!(
            o.stderr.contains("--project"),
            "{args:?}: diagnostic must name --project: {}",
            o.stderr
        );
    }
    for args in [&["driver", "status"][..], &["lock", "status"][..]] {
        let o = lock_cmd(home, args, &[]);
        assert_eq!(
            o.code,
            0,
            "{args:?} without --project is allowed: {}",
            o.both()
        );
    }
}

/// SKILL.md:77 -- `kind: undetermined` is what `lock status` reports when the
/// driver registry could not be read.
#[test]
fn claim_s_kind_undetermined_backlog_regi() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path();
    let o = lock_cmd(
        home,
        &["driver", "register", "--session-id", "s", "--project", "/p"],
        &[],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    let mut records = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "driver") {
                out.push(p);
            }
        }
    }
    walk(&home.join(".backlog/drivers"), &mut records);
    assert_eq!(records.len(), 1);
    std::fs::write(&records[0], "{ not json").unwrap();
    let o = lock_cmd(home, &["lock", "status", "--project", "/p"], &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v = json_of(&o);
    assert_eq!(v["kind"], "undetermined", "{v}");
    assert_eq!(v["undetermined"], true, "{v}");
}

// --------------------------------------------------------- packaging claims

/// README.md:269 -- only the `bin/backlog` launcher is tracked in git.
#[test]
fn claim_r_only_the_bin_backlog_launcher_is_tracked() {
    let o = Command::new("git")
        .args(["ls-files", "--", "bin"])
        .current_dir(manifest_dir())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let tracked: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(tracked, vec!["bin/backlog".to_string()]);
}

fn flow_dir() -> PathBuf {
    manifest_dir().join("../flow")
}

fn assert_flow_skill_exists() {
    let skill = std::fs::read_to_string(flow_dir().join("skills/flow/SKILL.md"))
        .expect("crates/flow/skills/flow/SKILL.md exists");
    let mut lines = skill.lines();
    assert_eq!(lines.next(), Some("---"), "front matter opens");
    let fm: Vec<&str> = lines.by_ref().take_while(|l| *l != "---").collect();
    assert!(
        fm.iter().any(|l| l.trim() == "name: flow"),
        "front matter has `name: flow`: {fm:?}"
    );
    let plugin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(flow_dir().join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(plugin["name"], "flow");
}

/// SKILL.md:8 -- the `/flow` skill the backlog skill points at exists.
#[test]
fn claim_s_flow() {
    assert_flow_skill_exists();
}

/// SKILL.md:69 -- same: `/flow` is the auto-drain skill.
#[test]
fn claim_s_flow_1() {
    assert_flow_skill_exists();
}

// ============================================================ round 2 claims

/// Rewrite `defer_until` of row `id` in `tasks.toml` to `ts`.
fn set_defer(f: &Fixture, id: &str, ts: i64) {
    let text = std::fs::read_to_string(f.tasks_path()).unwrap();
    let mut v: toml::Value = toml::from_str(&text).unwrap();
    let row = v["task"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id} not in tasks.toml"));
    row.as_table_mut()
        .unwrap()
        .insert("defer_until".into(), toml::Value::Integer(ts));
    std::fs::write(f.tasks_path(), toml::to_string(&v).unwrap()).unwrap();
}

/// README.md:10/51, README.ja.md:47 -- `cancel` closes as terminal, never
/// requeued: a cancelled row whose `defer_until` is in the past stays
/// `cancelled` after the SessionStart requeue, and `next` never hands it out.
#[test]
fn claim_n_cancel_never_requeued() {
    let f = Fixture::new("cancelreq");
    let (via_cli, label) = f.add_with_label("cancelled through the cli");
    let keeper = f.add("keeper stays pending");
    // Give the row a deferral, push it into the past, THEN cancel it, so the
    // closed row carries a stale past `defer_until`.
    let o = f.run(&["fail", &via_cli, "--reason", "x"]);
    assert_eq!(o.code, 0, "{}", o.both());
    set_defer(&f, &via_cli, now_unix() - 100);
    let o = f.run(&["cancel", &via_cli, "--reason", "not doing it"]);
    assert_eq!(o.code, 0, "{}", o.both());
    // A second cancelled row hand-planted in the LIVE file with a past
    // deferral (the shape the requeue loop actually iterates over).
    plant(
        &f,
        ".backlog/tasks.toml",
        &task_block(
            "cc000001",
            "planted cancelled",
            &label,
            "cancelled",
            &format!("defer_until = {}", now_unix() - 100),
        ),
    );
    assert_eq!(f.status(&via_cli), "cancelled");
    assert_eq!(f.status("cc000001"), "cancelled");

    let o = session_start(&f);
    assert_eq!(o.code, 0, "{}", o.both());

    assert_eq!(
        f.status(&via_cli),
        "cancelled",
        "cli-cancelled row requeued"
    );
    assert_eq!(f.status("cc000001"), "cancelled", "planted row requeued");
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        !o.stdout.contains(&via_cli) && !o.stdout.contains("cc000001"),
        "next must never hand out a cancelled row: {}",
        o.stdout
    );
    assert!(o.stdout.contains(&keeper), "control: {}", o.stdout);
}

/// SKILL.md:20 / README.md:50 / README.ja.md:46 -- a failed row is re-queued
/// as `pending` once its 2-day deferral has passed; before that it stays
/// `failed`.
#[test]
fn claim_n_failed_requeued_after_defer() {
    let f = Fixture::new("failreq");
    let due = f.add("failed and due");
    let waiting = f.add("failed still waiting");
    for id in [&due, &waiting] {
        let o = f.run(&["fail", id, "--reason", "x"]);
        assert_eq!(o.code, 0, "{}", o.both());
    }
    assert_eq!(f.status_stored(&due), "failed");
    set_defer(&f, &due, now_unix() - 10);
    // the other keeps its fail-time now+2d deferral

    let o = session_start(&f);
    assert_eq!(o.code, 0, "{}", o.both());

    assert_eq!(f.status_stored(&due), "pending", "due row must be requeued");
    assert_eq!(
        f.status_stored(&waiting),
        "failed",
        "a row whose deferral is in the future must stay failed"
    );
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains(&due),
        "requeued row is eligible: {}",
        o.stdout
    );
    assert!(
        !o.stdout.contains(&waiting),
        "still-deferred row is not handed out: {}",
        o.stdout
    );
}

const NEAR_A: &str = "add touched files field to the backlog task record";
const NEAR_B: &str = "add touched files field to the backlog task record and group";

/// README.md:138 / README.ja.md:122 -- the near-duplicate scan does not offer a
/// terminal row as a peer; a pending row with the same title IS offered.
#[test]
fn claim_n_near_dup_terminal_not_peer() {
    // Terminal (done-file) row with a near-duplicate title: not named.
    let f = Fixture::new("neardup-term");
    let label = f.add_with_label("unrelated seed").1;
    f.write(
        ".backlog/tasks.done.toml",
        &done_with_issue("dd00000a", NEAR_A, &label, 5),
    );
    let (o, id) = f.add_extra(NEAR_B, &["--repro-test", "bash tests/repro_yes.sh"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let id = id.expect("added");
    assert!(
        o.stderr.contains("near-duplicate scan for"),
        "scan must have run: {}",
        o.stderr
    );
    assert!(
        !o.stderr.contains("dd00000a") && !o.stderr.contains(NEAR_A),
        "a terminal row must not be offered as a peer: {}",
        o.stderr
    );

    // CONTROL: the same near-duplicate title as a PENDING row is named.
    let c = Fixture::new("neardup-live");
    let live = c.add(NEAR_A);
    let (o, id2) = c.add_extra(NEAR_B, &["--repro-test", "bash tests/repro_yes.sh"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id2.is_some());
    assert!(
        o.stderr.contains(&live),
        "control: a pending near-duplicate peer must be named: {}",
        o.stderr
    );
    let _ = id;
}

/// README.md:141-146 / README.ja.md:124-127 -- terminal wins when an id is in
/// both files; `next` never hands it out; `edit --status pending|failed` and
/// `fail` on a cancelled task are refused and the row stays cancelled.
#[test]
fn claim_n_cancelled_terminal_wins_and_edit_refused() {
    let f = Fixture::new("cancelwins");
    let (gone, label) = f.add_with_label("cancelled twice listed");
    let keeper = f.add("keeper pending");
    let o = f.run(&["cancel", &gone, "--reason", "not doing it"]);
    assert_eq!(o.code, 0, "{}", o.both());
    // Re-introduce the same id as PENDING in the live file.
    plant(
        &f,
        ".backlog/tasks.toml",
        &task_block(&gone, "cancelled twice listed", &label, "pending", ""),
    );
    let live = std::fs::read_to_string(f.tasks_path()).unwrap();
    assert_eq!(
        live.matches(&format!("id = \"{gone}\"")).count(),
        1,
        "setup: id is in tasks.toml as pending: {live}"
    );
    assert!(
        std::fs::read_to_string(f.done_path())
            .unwrap()
            .contains(&format!("id = \"{gone}\"")),
        "setup: id is also in the done file"
    );

    let rows: Vec<_> = f
        .list_all()
        .into_iter()
        .filter(|r| r["id"] == gone.as_str())
        .collect();
    assert_eq!(rows.len(), 1, "listed once: {rows:?}");
    assert_eq!(rows[0]["status"], "cancelled", "terminal wins: {rows:?}");
    let o = f.run(&["next"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(!o.stdout.contains(&gone), "next hands it out: {}", o.stdout);
    assert!(o.stdout.contains(&keeper), "control: {}", o.stdout);

    let before = f.store_bytes();
    for args in [
        vec!["edit", &gone, "--status", "pending"],
        vec!["edit", &gone, "--status", "failed"],
        vec!["fail", &gone, "--reason", "x"],
    ] {
        let o = f.run(&args);
        assert_ne!(o.code, 0, "{args:?} must be refused: {}", o.both());
        assert_eq!(f.status(&gone), "cancelled", "{args:?}: row must stay");
    }
    assert_eq!(f.store_bytes(), before, "refusals must write nothing");
}

/// SKILL.md:75 -- `lock acquire --force` takes the lock from a live holder;
/// without `--force` the second session is refused.
#[test]
fn claim_n_force_takes_live_lock() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let proj = td.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    let project = proj.canonicalize().unwrap().to_string_lossy().into_owned();
    let acquire = |sid: &str, force: bool| {
        let mut a = vec![
            "lock",
            "acquire",
            "--session-id",
            sid,
            "--project",
            &project,
        ];
        if force {
            a.push("--force");
        }
        lock_cmd(&home, &a, &[])
    };
    let status = || {
        let o = lock_cmd(&home, &["lock", "status", "--project", &project], &[]);
        assert_eq!(o.code, 0, "{}", o.both());
        o.stdout
    };

    let o = acquire("sess-AAAA", false);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(status().contains("sess-AAAA"), "A holds: {}", status());

    let o = acquire("sess-BBBB", false);
    assert_ne!(o.code, 0, "live holder must refuse B: {}", o.both());
    assert!(status().contains("sess-AAAA") && !status().contains("sess-BBBB"));

    let o = acquire("sess-BBBB", true);
    assert_eq!(o.code, 0, "--force must take the lock: {}", o.both());
    let st = status();
    assert!(st.contains("sess-BBBB"), "status must name B: {st}");
    assert!(!st.contains("sess-AAAA"), "A displaced: {st}");
}

/// SKILL.md:74 -- another session registered as a driver is NOT a conflict:
/// `next --claim` from a second session still gets a task, and the registered
/// driver's own claim gets a DIFFERENT task.
#[test]
fn claim_n_registered_driver_does_not_block_claim() {
    let f = Fixture::new("driverreg");
    let _ = f.add("task one");
    let _ = f.add("task two");
    let _ = f.add("task three");
    let p = f.project();
    let o = f.run_env(
        &[
            "driver",
            "register",
            "--session-id",
            "sess-A",
            "--project",
            &p,
        ],
        &[("CLAUDE_CODE_SESSION_ID", "sess-A")],
    );
    assert_eq!(o.code, 0, "{}", o.both());
    let o = f.run(&["lock", "status", "--project", &p]);
    assert!(
        o.stdout.contains("sess-A"),
        "precondition: A is registered as a driver: {}",
        o.both()
    );

    let claim = |sid: &str| {
        let o = f.run_env(&["next", "--claim"], &[("CLAUDE_CODE_SESSION_ID", sid)]);
        assert_eq!(o.code, 0, "{sid} claim must succeed: {}", o.both());
        let v: serde_json::Value = serde_json::from_str(o.stdout.trim()).unwrap();
        v["id"].as_str().unwrap().to_string()
    };
    let b = claim("sess-B");
    let a = claim("sess-A");
    assert_ne!(a, b, "the two sessions must get different tasks");
}

/// README.md:97 / README.ja.md:83 -- `add --force` bypasses the cross-session
/// claim rejection (condukt `is-claimed` exit 0 / claimed:true).
#[test]
fn claim_n_add_force_bypasses_claimed() {
    let f = Fixture::new("addforce");
    f.write_exec_shim("condukt", "#!/bin/sh\necho '{\"claimed\": true}'\nexit 0\n");
    let (o, id) = f.add_extra(
        "claimed elsewhere",
        &["--repro-test", "bash tests/repro_yes.sh"],
    );
    assert_ne!(o.code, 0, "claimed add must be refused: {}", o.both());
    assert!(id.is_none(), "{}", o.stdout);
    assert!(
        !std::fs::read_to_string(f.tasks_path())
            .unwrap_or_default()
            .contains("claimed elsewhere"),
        "refused add must store nothing"
    );

    let (o, id) = f.add_extra(
        "claimed elsewhere",
        &["--repro-test", "bash tests/repro_yes.sh", "--force"],
    );
    assert_eq!(o.code, 0, "--force must bypass: {}", o.both());
    let id = id.expect("added with --force");
    assert_eq!(f.status(&id), "pending");
}

/// README.md:203 / README.ja.md:175 -- a lease stops excluding once its holder
/// has not been seen alive for 1h: excluded at 59 min, handed out at 61 min.
#[test]
fn claim_n_lease_stale_threshold_1h() {
    let f = Fixture::new("lease1h");
    let _ = f.add("only task");
    let id = claim_id(&f);
    let ledger = ledger_file(&f);
    let set_age = |secs: i64| {
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
        for e in v["entries"].as_array_mut().unwrap() {
            if e["id"] == id.as_str() {
                e["claimed_at"] = (now_unix() - secs).into();
            }
        }
        std::fs::write(&ledger, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    };

    set_age(59 * 60);
    let o = f.run(&["next", "--claim"]);
    assert!(
        !o.stdout.contains(&id),
        "59 min old lease must still exclude: {}",
        o.both()
    );
    let o = f.run(&["next"]);
    assert!(!o.stdout.contains(&id), "plain next too: {}", o.both());

    set_age(61 * 60);
    let again = claim_id(&f);
    assert_eq!(again, id, "61 min old lease no longer excludes");
}

/// README.md:127 / README.ja.md:111 -- `list` marks a row `[project
/// unresolved: <label>]` when its project label was a guess; a normal row has
/// no such mark.
#[test]
fn claim_n_list_marks_project_unresolved() {
    let f = Fixture::new("unresolved");
    let (normal, label) = f.add_with_label("normal row");
    plant(
        &f,
        ".backlog/tasks.toml",
        &task_block("ee000001", "guessed row", "/guess/label", "pending", "")
            .replace("project_unresolved = false", "project_unresolved = true"),
    );
    let _ = label;
    let o = f.run(&["list", "--all"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let line_of = |id: &str| {
        o.stdout
            .lines()
            .find(|l| l.contains(id))
            .unwrap_or_else(|| panic!("no line for {id}: {}", o.stdout))
            .to_string()
    };
    assert!(
        line_of("ee000001").contains("[project unresolved: /guess/label]"),
        "{}",
        line_of("ee000001")
    );
    assert!(
        !line_of(&normal).contains("[project unresolved:"),
        "{}",
        line_of(&normal)
    );
}

/// README.md:90-95 / README.ja.md:77-81 -- a condukt claim check that exits
/// with anything other than 0/1 (e.g. 3) is "could not check": `add` exits
/// non-zero and stores nothing.
#[test]
fn claim_n_claim_check_undetermined_refuses_add() {
    let f = Fixture::new("undet");
    f.write_exec_shim("condukt", "#!/bin/sh\nexit 3\n");
    let (o, id) = f.add_extra(
        "undetermined claim",
        &["--repro-test", "bash tests/repro_yes.sh"],
    );
    assert_ne!(o.code, 0, "exit 3 must refuse the add: {}", o.both());
    assert!(id.is_none(), "{}", o.stdout);
    assert!(
        !std::fs::read_to_string(f.tasks_path())
            .unwrap_or_default()
            .contains("undetermined claim"),
        "refused add must store nothing"
    );
    assert!(o.stderr.contains("condukt exit 3"), "reason: {}", o.stderr);
}

/// README.md:86 / README.ja.md:75 -- how `add` locates `condukt`.
/// Observed order: the plugin cache
/// (`$HOME/.claude/plugins/cache/yukineko/condukt/<ver>/bin/condukt`) first,
/// `$PATH` second; with neither, `add` is refused naming the reason.
#[test]
fn claim_n_condukt_resolution_order() {
    let run_add = |f: &Fixture, env: &[(&str, &str)]| {
        let p = f.project();
        f.run_env(
            &[
                "add",
                "--title",
                "resolution order probe",
                "--project",
                &p,
                "--repro-test",
                "bash tests/repro_yes.sh",
            ],
            env,
        )
    };
    let marker = |f: &Fixture| f.home.join("who-ran");
    let script = |f: &Fixture, who: &str| {
        format!(
            "#!/bin/sh\necho {who} >> '{}'\necho '{{\"claimed\":false}}'\nexit 1\n",
            marker(f).display()
        )
    };
    let install_cache = |f: &Fixture| {
        let bin = f
            .home
            .join(".claude/plugins/cache/yukineko/condukt/9.9.9/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join("condukt");
        std::fs::write(&p, script(f, "CACHE")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };

    // (1) both a cache copy and a PATH copy: the CACHE copy runs, PATH does not.
    let f = Fixture::new("resorder-both");
    f.write_exec_shim("condukt", &script(&f, "PATH"));
    install_cache(&f);
    let o = run_add(&f, &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let ran = std::fs::read_to_string(marker(&f)).unwrap();
    assert_eq!(ran.trim(), "CACHE", "cache copy must win; ran: {ran:?}");

    // (2) PATH copy only: the PATH copy runs.
    let f = Fixture::new("resorder-path");
    f.write_exec_shim("condukt", &script(&f, "PATH"));
    let o = run_add(&f, &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    let ran = std::fs::read_to_string(marker(&f)).unwrap();
    assert_eq!(ran.trim(), "PATH", "ran: {ran:?}");

    // (3) neither: refused, reason names condukt, nothing stored.
    let f = Fixture::new("resorder-none");
    std::fs::remove_file(f.shim.join("condukt")).unwrap();
    let o = run_add(&f, &[("PATH", "/usr/bin:/bin")]);
    assert_ne!(o.code, 0, "no condukt anywhere must refuse: {}", o.both());
    assert!(o.stderr.contains("condukt not found"), "{}", o.stderr);
    assert!(!std::fs::read_to_string(f.tasks_path())
        .unwrap_or_default()
        .contains("resolution order probe"));
}

/// README.md:79-82 / README.ja.md:70-72 -- hashkey: title trimmed, NFKC,
/// lowercased, whitespace runs collapsed, leading/trailing punctuation
/// stripped; folded with the project via 64-bit FNV-1a (unit separator joined)
/// into 16 lowercase hex digits.
#[test]
fn claim_n_hashkey_normalisation() {
    let f = Fixture::new("hknorm");
    let plain = f.add("abc def");
    let force = &["--repro-test", "bash tests/repro_yes.sh", "--force"];
    let (o, wide) = f.add_extra("  \u{FF21}\u{FF22}\u{FF23}   Def!! ", force);
    assert_eq!(o.code, 0, "{}", o.both());
    let wide = wide.unwrap();
    let (o, lead) = f.add_extra("!!abc def", force);
    assert_eq!(o.code, 0, "{}", o.both());
    let lead = lead.unwrap();
    let (o, inner) = f.add_extra("abc, def", force);
    assert_eq!(o.code, 0, "{}", o.both());
    let inner = inner.unwrap();

    let key = |id: &str| f.row(id)["hashkey"].as_str().unwrap().to_string();
    let project = f.row(&plain)["project"].as_str().unwrap().to_string();
    let expected = format!(
        "{:016x}",
        harness_core::hash::fnv1a64(format!("abc def\u{1f}{project}").as_bytes())
    );
    assert_eq!(
        key(&plain),
        expected,
        "folding of (normalized title, project)"
    );
    assert_eq!(
        key(&wide),
        expected,
        "NFKC + lowercase + collapse + strip trailing"
    );
    assert_eq!(key(&lead), expected, "leading punctuation stripped");
    assert_ne!(key(&inner), expected, "inner punctuation is kept");
    for id in [&plain, &wide, &lead, &inner] {
        let k = key(id);
        assert_eq!(k.len(), 16, "{k}");
        assert!(
            k.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "16 lowercase hex digits: {k}"
        );
    }
}

/// README.md:251-254 / README.ja.md:214-216 -- when the project identity
/// cannot be resolved (dangling worktree `.git` link) `list` and plain `next`
/// refuse: non-zero, a reason on stderr, nothing on stdout.
#[test]
fn claim_n_plain_next_refuses_unresolved_identity() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let cwd = orphan_dir(&td, "broken-worktree");
    std::fs::write(
        cwd.join(".git"),
        "gitdir: /definitely/nonexistent-xyz-q7f3k9z2/.git/worktrees/broken\n",
    )
    .unwrap();
    std::fs::create_dir_all(cwd.join(".backlog")).unwrap();
    std::fs::write(
        cwd.join(".backlog/tasks.toml"),
        pending_block_with_repro("bb000001", "stranded", "/some/old/root"),
    )
    .unwrap();

    for args in [&["next"][..], &["list"][..]] {
        let o = run_in(&home, &cwd, args);
        assert_ne!(o.code, 0, "{args:?} must refuse: {}", o.both());
        assert!(
            !o.stderr.trim().is_empty(),
            "{args:?}: a reason on stderr required"
        );
        assert!(
            o.stdout.trim().is_empty(),
            "{args:?}: nothing on stdout: {:?}",
            o.stdout
        );
    }

    // CONTROL: a resolvable checkout (real `git init`) with the same store
    // is not refused, so the refusals above are due to the identity.
    let ok = orphan_dir(&td, "healthy");
    plain_git(&ok, &["init", "-q"]);
    std::fs::create_dir_all(ok.join(".backlog")).unwrap();
    std::fs::write(
        ok.join(".backlog/tasks.toml"),
        pending_block_with_repro("bb000002", "healthy", "/some/old/root"),
    )
    .unwrap();
    let o = run_in(&home, &ok, &["next"]);
    assert_eq!(o.code, 0, "control must not be refused: {}", o.both());
}

/// SKILL.md: "repo root が上に無い cwd では `add` は拒否する（`~/.backlog`
/// へフォールバックしない）". Unlike the project_scope test, this passes a
/// complete `--repro-test` so the ONLY remaining reason to refuse is the
/// missing repo -- and asserts that reason, so deleting the no-repo refusal
/// cannot go unnoticed.
#[test]
fn claim_s_add_outside_repo_names_the_missing_repo() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    // orphan_dir asserts no `.git` exists at or above the cwd (non-vacuity).
    let cwd = orphan_dir(&td, "orphan");
    let before = snapshot(&home);
    let o = run_in(
        &home,
        &cwd,
        &[
            "add",
            "--title",
            "outside any repo",
            "--project",
            &cwd.to_string_lossy(),
            "--repro-test",
            "bash tests/repro_yes.sh",
        ],
    );
    assert_ne!(
        o.code,
        0,
        "add outside any repo must be refused: {}",
        o.both()
    );
    assert!(
        o.stderr.contains("no git repo above"),
        "the refusal reason must be the missing repo, got stderr: {:?}",
        o.stderr
    );
    assert!(
        o.stdout.trim().is_empty(),
        "no row may be reported added, got stdout: {:?}",
        o.stdout
    );
    assert!(
        !home.join(".backlog").join("tasks.toml").exists(),
        "add must not fall back to writing ~/.backlog/tasks.toml"
    );
    assert_eq!(
        snapshot(&home),
        before,
        "$HOME must be left exactly as it was"
    );
}
