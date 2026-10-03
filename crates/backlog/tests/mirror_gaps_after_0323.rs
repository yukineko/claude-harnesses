#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Three independent holes left in the GitHub mirror after v0.3.23, pinned
//! BEFORE the implementation by a disinterested party (CLAUDE.md §2(a)).
//!
//! The mirror is one-way: the local `.backlog` store is authoritative and the
//! GitHub issue is its public face. v0.3.23 made the *close* arm honest (it now
//! carries `--comment` with the task's notes, bounded at 60000 bytes, and says
//! `truncated` when it cuts). The three gaps below are the places that honesty
//! does not reach, each measured today (2026-10-02) against this tree:
//!
//! **Gap 1 — `edit --status` does not mirror a terminal transition.**
//! `Command::Edit` in `crates/backlog/src/main.rs` calls `store::edit`, prints
//! `updated: <id>`, and NEVER calls `mirror_close_for` (compare `Command::Done`
//! three arms above it, which does). So `backlog edit <id> --status done` moves
//! the task to its terminal state locally and leaves the GitHub issue OPEN with
//! nothing said about it — the exact silent divergence that let 467 issues
//! accumulate. A terminal transition is a terminal transition whichever verb
//! performed it.
//!
//! *Merge note (main 04ea9b35, close-evidence):* `edit --status` can no longer
//! reach a terminal state at all, so Gap 1 is now pinned as "refused, nothing
//! written, nothing sent" for `edit` plus "closes inline and records it" for
//! the verbs that can (`done` with evidence, `cancel --reason`).
//!
//! **Gap 2 — the create-side body is unbounded while the close side is
//! bounded.** `store::CLOSE_COMMENT_MAX` bounds the close comment to 60000
//! bytes and marks the cut. `github::build_issue_create_args` has no bound at
//! all: `gh issue create --body <notes>` passes the notes straight through, and
//! GitHub rejects a body over 65536 characters — so an oversized task does not
//! degrade to "mirrored without its notes", it degrades to "not mirrored at
//! all". The asymmetry is the bug.
//!
//! **Gap 3 — an issue body is written once at create and never reconciled.**
//! `store::sync_plan`'s `Create` arm fires only on `(STATUS_PENDING, None, _)`
//! — i.e. only while `issue_number` is None. Once the issue exists, every later
//! edit to the task's notes stays local forever. There are exactly two sync
//! points today (create, close) and no third.
//!
//! Like `close_records_content.rs`, these drive the real binary against a stub
//! `gh` that records **one argument per delimited record**, because the values
//! under test are multi-line and tens of thousands of bytes long and a
//! `"$*"`-joined log cannot be taken apart again.
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

struct Env {
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    ghlog: PathBuf,
}

/// Record delimiters, verbatim from `close_records_content.rs`. One `INV` per
/// `gh` invocation, then one `ARG` ... `END` pair per argument, so an argument
/// value containing spaces or newlines is still recoverable byte-for-byte.
const INV: &str = "==BL-INV==\n";
const ARG: &str = "==BL-ARG==\n";
const END: &str = "\n==BL-END==\n";

/// A stub `gh` that logs every argument individually and, for `issue create`,
/// answers with a new issue URL the way the real thing does.
/// `ok = false` makes every invocation fail AFTER logging it.
fn stub_body(log: &Path, ok: bool) -> String {
    format!(
        "#!/bin/sh\n\
         printf '{inv}' >> {log}\n\
         for a in \"$@\"; do printf '{arg}%s{end}' \"$a\" >> {log}; done\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
         \u{20} n=$(grep -c '^create$' {log})\n\
         \u{20} echo \"https://github.com/o/r/issues/$((500 + n))\"\n\
         fi\n\
         exit {code}\n",
        inv = INV.escape_default(),
        arg = ARG.escape_default(),
        end = END.escape_default(),
        log = log.display(),
        code = if ok { 0 } else { 1 },
    )
}

fn install_stub(e: &Env, ok: bool) {
    std::fs::write(e.bin.join("gh"), stub_body(&e.ghlog, ok)).unwrap();
    std::fs::set_permissions(
        e.bin.join("gh"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
}

/// A repo with a github.com origin (required for the mirror to engage at all)
/// and a stub `gh` on PATH. The store is written per test.
fn setup(tag: &str) -> Env {
    let t = std::env::temp_dir().join(format!("bl-gaps0323-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    // `bl()` runs the binary with env_clear() + PATH=bin, so every tool the
    // stub itself shells out to has to live in that same dir.
    symlink(tool_path("grep"), bin.join("grep")).unwrap();
    // The repro runner (`add --repro-test bash tests/...`) executes `bash`.
    symlink(tool_path("bash"), bin.join("bash")).unwrap();

    let e = Env {
        home,
        bin,
        repo,
        ghlog: t.join("gh.log"),
    };
    install_stub(&e, true);

    let g = |a: &[&str]| {
        assert!(Command::new("git")
            .args(a)
            .current_dir(&e.repo)
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", &e.home)
            .status()
            .unwrap()
            .success());
    };
    g(&["init", "-q", "."]);
    g(&["remote", "add", "origin", "https://github.com/o/r.git"]);

    std::fs::create_dir_all(e.repo.join(".backlog")).unwrap();
    e
}

/// Close-evidence (main 04ea9b35): a bare `backlog done ID` is refused, so the
/// close these tests mirror goes through the cheapest evidence route,
/// `--duplicate-of`, naming a canonical row seeded here as `done` with NO
/// issue. That row contributes nothing to any sync plan (`sync_plan` only acts
/// on a terminal row that HOLDS an unclosed issue), so every count asserted
/// below is about the task under test alone.
const DUP_TARGET: &str = "d0p0cafe";

fn seed_duplicate_target(done_file: &Path) {
    let block = format!(
        "[[task]]\nid = \"{DUP_TARGET}\"\ntitle = \"canonical ticket\"\nproject = \"/repo\"\ntags = []\nstatus = \"done\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n\n"
    );
    let mut cur = std::fs::read_to_string(done_file).unwrap_or_default();
    if !cur.contains(DUP_TARGET) {
        cur.push_str(&block);
        std::fs::create_dir_all(done_file.parent().unwrap()).unwrap();
        std::fs::write(done_file, cur).unwrap();
    }
}

fn bl(e: &Env, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("PATH", &e.bin)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Drive the SessionStart hook the way `mirror_drift_surfaced.rs` does: the
/// hook JSON goes in on stdin and the injected context comes back as one JSON
/// line on stdout.
fn session_start(e: &Env) -> String {
    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        e.repo.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("session-start")
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("PATH", &e.bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// TOML basic-string escaping, so a `notes` value with quotes/newlines can be
/// written into a hand-made store without corrupting it.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One `[[task]]` block, with the same field set the existing hand-written
/// store tests use, so it parses through the production `load` path.
fn task_block(
    id: &str,
    title: &str,
    status: &str,
    notes: &str,
    issue: Option<u64>,
    t: i64,
) -> String {
    let mut s = format!(
        "[[task]]\nid = {id}\ntitle = {title}\nproject = \"/repo\"\ntags = []\nstatus = {status}\nnotes = {notes}\ncreated_at = {t}\nupdated_at = {t}\nweight = 0.0\n",
        id = toml_str(id),
        title = toml_str(title),
        status = toml_str(status),
        notes = toml_str(notes),
    );
    if let Some(n) = issue {
        s.push_str(&format!(
            "issue_number = {n}\nissue_url = \"https://github.com/o/r/issues/{n}\"\n"
        ));
    }
    s.push('\n');
    s
}

fn live_path(e: &Env) -> PathBuf {
    e.repo.join(".backlog/tasks.toml")
}

fn done_path(e: &Env) -> PathBuf {
    e.repo.join(".backlog/tasks.done.toml")
}

fn write_live(e: &Env, body: &str) {
    std::fs::write(live_path(e), body).unwrap();
}

/// Terminal tasks live in the done file. The "terminal task whose issue was
/// never closed" shape is not reachable through the CLI, so it is written by
/// hand — exactly as `sync_only_scope.rs` does.
fn write_done(e: &Env, body: &str) {
    std::fs::write(done_path(e), body).unwrap();
}

fn append(p: &Path, body: &str) {
    let mut cur = std::fs::read_to_string(p).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push('\n');
    cur.push_str(body);
    std::fs::write(p, cur).unwrap();
}

fn live_file(e: &Env) -> String {
    std::fs::read_to_string(live_path(e)).unwrap_or_default()
}

fn done_file(e: &Env) -> String {
    std::fs::read_to_string(done_path(e)).unwrap_or_default()
}

/// Every `gh` invocation, as a list of its individual argument values.
fn invocations(e: &Env) -> Vec<Vec<String>> {
    let raw = std::fs::read_to_string(&e.ghlog).unwrap_or_default();
    let mut out = Vec::new();
    for chunk in raw.split(INV).skip(1) {
        let mut args: Vec<String> = Vec::new();
        let mut rest = chunk;
        while let Some(i) = rest.find(ARG) {
            rest = &rest[i + ARG.len()..];
            match rest.find(END) {
                Some(j) => {
                    args.push(rest[..j].to_string());
                    rest = &rest[j + END.len()..];
                }
                None => panic!("gh log record is unterminated; harness bug: {chunk:?}"),
            }
        }
        out.push(args);
    }
    out
}

/// Forget every invocation recorded so far, so a multi-phase test can count
/// what ONE later run did rather than a running total.
fn clear_log(e: &Env) {
    let _ = std::fs::remove_file(&e.ghlog);
}

fn subcommand<'a>(invs: &'a [Vec<String>], verb: &str) -> Vec<&'a Vec<String>> {
    invs.iter()
        .filter(|a| a.len() >= 2 && a[0] == "issue" && a[1] == verb)
        .collect()
}

fn closes(invs: &[Vec<String>]) -> Vec<&Vec<String>> {
    subcommand(invs, "close")
}

fn creates(invs: &[Vec<String>]) -> Vec<&Vec<String>> {
    subcommand(invs, "create")
}

/// `gh issue edit <N> --body <body>` — the third sync point Gap 3 is about.
/// Deliberately matched on `issue edit` and nothing else: `issue comment` would
/// APPEND a comment (leaving the stale body in place, which is the thing being
/// fixed) and a second `issue create` would publish a duplicate issue.
fn body_edits(invs: &[Vec<String>]) -> Vec<&Vec<String>> {
    subcommand(invs, "edit")
}

/// The value that FOLLOWS `flag`, or None when the flag is absent. Asserting on
/// this rather than on a rigid full-argv equality keeps flag ORDER out of the
/// contract.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).cloned()
}

/// Short, readable rendering of an invocation for failure messages, with huge
/// values elided so an 85k-byte body cannot drown the assertion text.
fn render(invs: &[Vec<String>]) -> String {
    if invs.is_empty() {
        return "(no gh invocation at all)".to_string();
    }
    invs.iter()
        .map(|a| {
            let args: Vec<String> = a
                .iter()
                .map(|v| {
                    if v.chars().count() > 160 {
                        format!(
                            "<{} chars / {} bytes: {}...>",
                            v.chars().count(),
                            v.len(),
                            v.chars().take(80).collect::<String>()
                        )
                    } else {
                        format!("{v:?}")
                    }
                })
                .collect();
            format!("[{}]", args.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `backlog add`, returning the generated task id. The project is this repo's
/// real path so `git_remote_origin_url` finds the github.com origin and the
/// create arm actually engages.
fn add(e: &Env, title: &str, notes: &str) -> String {
    let project = e.repo.display().to_string();
    let (c, o, er) = bl(
        e,
        &[
            "add",
            "--title",
            title,
            "--project",
            &project,
            "--notes",
            notes,
        ],
    );
    assert_eq!(c, 0, "backlog add must succeed\nstdout={o}\nstderr={er}");
    o.lines()
        .find_map(|l| l.strip_prefix("added: "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| panic!("add must print `added: <id>`; stdout was {o:?}"))
}

/// Like [`add`], but the task lands `pending`: since close-evidence (main
/// 04ea9b35) a finding filed without a REPRODUCED repro test lands
/// `unconfirmed`, and the body re-sync arm deliberately acts only on workable
/// rows (`pending` / `failed`). The repro script exits 1 (= reproduced) and is
/// committed, because only a git-tracked script at HEAD is evidence.
fn add_pending(e: &Env, title: &str, notes: &str) -> String {
    let script = e.repo.join("tests/repro_yes.sh");
    if !script.exists() {
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/bin/bash\necho 'bug present'\nexit 1\n").unwrap();
        for a in [
            &["add", "--", "tests/repro_yes.sh"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "repro",
            ][..],
        ] {
            assert!(Command::new("git")
                .args(a)
                .current_dir(&e.repo)
                .env("PATH", std::env::var("PATH").unwrap())
                .env("HOME", &e.home)
                .status()
                .unwrap()
                .success());
        }
    }
    let project = e.repo.display().to_string();
    let (c, o, er) = bl(
        e,
        &[
            "add",
            "--title",
            title,
            "--project",
            &project,
            "--notes",
            notes,
            "--repro-test",
            "bash tests/repro_yes.sh",
        ],
    );
    assert_eq!(c, 0, "backlog add must succeed\nstdout={o}\nstderr={er}");
    assert!(
        live_file(e).contains("status = \"pending\""),
        "fixture: a reproduced repro must land the task pending:\n{}",
        live_file(e)
    );
    o.lines()
        .find_map(|l| l.strip_prefix("added: "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| panic!("add must print `added: <id>`; stdout was {o:?}"))
}

/// A notes blob well over 80000 BYTES made of MULTI-BYTE characters, with a
/// marker at each end. Japanese on purpose: a naive `&notes[..60_000]` byte
/// slice panics on a split codepoint, so the bound has to be taken on a char
/// boundary — and that is only observable with multi-byte content.
fn oversized_japanese_notes(head: &str, tail: &str) -> String {
    let mut s = String::from(head);
    while s.len() <= 85_000 {
        s.push_str("このタスクの記録は非常に長く、直列化された観測の全文を保持している。");
    }
    s.push_str(tail);
    assert!(
        s.len() > 80_000,
        "harness: the oversized notes must actually be oversized ({} bytes)",
        s.len()
    );
    assert!(
        s.chars().count() < s.len(),
        "harness: the notes must be multi-byte, else the codepoint-boundary \
         hazard is not exercised"
    );
    s
}

// ===========================================================================
// (0) HARNESS SELF-CHECK — must pass before AND after.
// ===========================================================================

/// Every verdict in this file rests on `invocations()` recovering individual
/// argument values from the stub's log, and the values under test are
/// multi-line, multi-byte and tens of thousands of bytes long. A harness that
/// mangled them would make a missing bound look present (or the reverse).
#[test]
fn the_argv_log_round_trips_multibyte_and_huge_arguments() {
    let e = setup("selfcheck");
    let multiline = "一行目\n  二行目は  空白つき\n\n四行目\tタブ";
    let huge = oversized_japanese_notes("HEAD-SELFCHECK: ", " TAIL-SELFCHECK");

    let st = Command::new(e.bin.join("gh"))
        .args(["issue", "edit", "7", "--body"])
        .arg(&huge)
        .arg(multiline)
        .env_clear()
        .env("PATH", &e.bin)
        .status()
        .unwrap();
    assert!(st.success(), "the stub must exit 0");

    let invs = invocations(&e);
    assert_eq!(
        invs.len(),
        1,
        "one invocation logged; got:\n{}",
        render(&invs)
    );
    assert_eq!(
        invs[0],
        vec![
            "issue".to_string(),
            "edit".to_string(),
            "7".to_string(),
            "--body".to_string(),
            huge.clone(),
            multiline.to_string(),
        ],
        "every argument must round-trip verbatim through the log"
    );
    assert_eq!(
        flag_value(&invs[0], "--body").as_deref(),
        Some(huge.as_str()),
        "flag_value must return the whole value that follows the flag"
    );
    assert_eq!(
        body_edits(&invs).len(),
        1,
        "`issue edit` must be recognised as a body edit; got:\n{}",
        render(&invs)
    );
    assert!(
        creates(&invs).is_empty() && closes(&invs).is_empty(),
        "an `issue edit` must not be miscounted as a create or a close"
    );
}

// ===========================================================================
// GAP 1 — a terminal transition must mirror the close, whichever verb made it.
// ===========================================================================
//
// When these tests were written (local line, v0.3.24) `edit --status done|
// cancelled` was a supported terminal transition and Gap 1 was that it left
// the issue open. main's close-evidence change (04ea9b35) then removed that
// route entirely: `edit --status` may only name `pending` / `failed`, and a
// terminal state needs a recorded closure (`done` with evidence, `cancel
// --reason`, or a human `ruling approve`). The merge of the two lines keeps
// the stricter rule, so Gap 1 is now closed from BOTH ends and is pinned that
// way:
//   * `edit --status <terminal>` is refused with nothing written and nothing
//     sent to GitHub — so it can never again finish work locally while the
//     issue stays open;
//   * every verb that CAN reach a terminal state closes the issue inline, with
//     the reason the state implies and a comment carrying id + notes, and
//     records the close so the next sync plans nothing.

/// GAP 1, THE EDIT ROUTE. `edit --status done` and `edit --status cancelled` on
/// a task holding an issue must be REFUSED, leave the task exactly where it
/// was (live, not in the done file), and invoke `gh` not at all. A refusal
/// that still closed the issue, or an acceptance that did not, would both
/// reopen the silent divergence Gap 1 is about.
#[test]
fn edit_status_terminal_is_refused_and_leaves_store_and_issue_untouched() {
    let e = setup("editrefused");
    write_live(
        &e,
        &task_block(
            "aaaa0051",
            "terminal via edit, not via done",
            "pending",
            "EDIT-NOTES-ALPHA: tried to reach the terminal state through `edit --status`.",
            Some(51),
            1,
        ),
    );

    for status in ["done", "cancelled"] {
        let (c, o, er) = bl(&e, &["edit", "aaaa0051", "--status", status]);
        let invs = invocations(&e);
        eprintln!(
            "edit --status {status}: code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
            render(&invs)
        );
        assert_ne!(
            c, 0,
            "`edit --status {status}` must be refused (a terminal state needs a recorded \
             closure): stdout={o:?} stderr={er:?}"
        );
        assert!(
            er.contains("refused"),
            "the refusal must say so on stderr, naming the route that does work; \
             stderr was {er:?}"
        );
        assert!(
            invs.is_empty(),
            "a refused `edit --status {status}` must not touch GitHub; gh was invoked \
             with:\n{}",
            render(&invs)
        );
        assert!(
            !done_file(&e).contains("aaaa0051"),
            "a refused edit must not move the task to the terminal file:\n{}",
            done_file(&e)
        );
        let live = live_file(&e);
        assert!(
            live.contains("aaaa0051") && live.contains("status = \"pending\""),
            "a refused edit must leave the task live and pending:\n{live}"
        );
    }
}

/// GAP 1, `done`. Closing through the evidence-gated `done` must close the
/// task's OWN issue as `completed`, with a `--comment` carrying the id and the
/// notes, and RECORD the close so the next `sync --only close` plans zero.
#[test]
fn done_closes_the_issue_and_records_it_so_the_next_sync_plans_nothing() {
    let e = setup("donerecord");
    write_live(
        &e,
        &task_block(
            "bbbb0052",
            "recorded close via done",
            "pending",
            "DONE-NOTES-BRAVO: the close must be stamped.",
            Some(52),
            2,
        ),
    );

    seed_duplicate_target(&e.repo.join(".backlog/tasks.done.toml"));
    let (c, o, er) = bl(&e, &["done", "bbbb0052", "--duplicate-of", DUP_TARGET]);
    let invs = invocations(&e);
    eprintln!(
        "done: code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "the evidence-bearing done must succeed: {er}");
    assert!(
        done_file(&e).contains("bbbb0052"),
        "harness/precondition: the task must actually be terminal:\n{}",
        done_file(&e)
    );

    let cl = closes(&invs);
    assert_eq!(cl.len(), 1, "exactly one close; gh:\n{}", render(&invs));
    let args = cl[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some("52"),
        "the close must target the task's OWN issue number; argv was {args:?}"
    );
    assert_eq!(
        flag_value(args, "--reason").as_deref(),
        Some("completed"),
        "a done task closes as completed; argv was {args:?}"
    );
    let body = flag_value(args, "--comment")
        .unwrap_or_else(|| panic!("the close must carry --comment; argv was {args:?}"));
    assert!(
        body.contains("bbbb0052") && body.contains("DONE-NOTES-BRAVO: the close must be stamped."),
        "the comment must name the task and carry its notes; body was {body:?}"
    );

    let (c2, o2, er2) = bl(&e, &["sync", "--only", "close"]);
    eprintln!(
        "sync: code={c2}\nstdout={o2}\nstderr={er2}\ndone file:\n{}",
        done_file(&e)
    );
    assert_eq!(c2, 0, "{er2}");
    assert!(
        o2.contains("0 issue(s) to close"),
        "the close `done` performed must be recorded, so the next sync plans \
         ZERO closes; the plan said: {o2:?}"
    );
}

/// GAP 1, `cancel`. Abandoned work must close as `not planned` — never
/// `completed` — with the comment carrying the id and the notes (which now
/// include the recorded discard reason), and the close must be recorded.
#[test]
fn cancel_closes_the_issue_as_not_planned_and_records_it() {
    let e = setup("cancelclose");
    write_live(
        &e,
        &task_block(
            "cccc0053",
            "abandoned",
            "pending",
            "EDIT-NOTES-CHARLIE: superseded; not doing this.",
            Some(53),
            3,
        ),
    );

    let (c, o, er) = bl(
        &e,
        &[
            "cancel",
            "cccc0053",
            "--reason",
            "CANCEL-REASON-CHARLIE: superseded",
        ],
    );
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "`cancel --reason` must succeed: stdout={o:?} stderr={er:?}"
    );

    let cl = closes(&invs);
    assert_eq!(
        cl.len(),
        1,
        "cancel must close the issue; gh:\n{}",
        render(&invs)
    );
    let args = cl[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some("53"),
        "argv was {args:?}"
    );
    assert_eq!(
        flag_value(args, "--reason").as_deref(),
        Some("not planned"),
        "cancelled work must close as `not planned`, never `completed`; argv was {args:?}"
    );
    let body = flag_value(args, "--comment")
        .unwrap_or_else(|| panic!("the close must carry --comment; argv was {args:?}"));
    assert!(
        body.contains("cccc0053")
            && body.contains("EDIT-NOTES-CHARLIE: superseded; not doing this.")
            && body.contains("CANCEL-REASON-CHARLIE: superseded"),
        "the comment must name the task and carry its notes and the discard reason; \
         body was {body:?}"
    );

    let (c2, o2, er2) = bl(&e, &["sync", "--only", "close"]);
    assert_eq!(c2, 0, "{er2}");
    assert!(
        o2.contains("0 issue(s) to close"),
        "the close `cancel` performed must be recorded; the plan said: {o2:?}"
    );
}

/// GAP 1, CONTROL — GREEN NOW AND MUST STAY GREEN. A NON-terminal edit must not
/// touch GitHub at all. This is the test that stops the implementer from
/// hanging the mirror off "any edit": closing an issue because someone fixed a
/// typo in the notes is an outward-facing write nobody asked for.
#[test]
fn a_non_terminal_edit_makes_no_gh_invocation() {
    let e = setup("editnoop");
    write_live(
        &e,
        &task_block(
            "dddd0054",
            "still in flight",
            "pending",
            "EDIT-NOTES-DELTA: original.",
            Some(54),
            4,
        ),
    );

    let (c, o, er) = bl(
        &e,
        &["edit", "dddd0054", "--notes", "EDIT-NOTES-DELTA: revised."],
    );
    eprintln!("notes edit: code={c}\nstdout={o}\nstderr={er}");
    assert_eq!(c, 0, "{er}");
    assert_eq!(
        invocations(&e).len(),
        0,
        "editing the NOTES of a live task must not touch GitHub; gh was invoked with:\n{}",
        render(&invocations(&e))
    );

    let (c, o, er) = bl(&e, &["edit", "dddd0054", "--status", "pending"]);
    eprintln!("status pending edit: code={c}\nstdout={o}\nstderr={er}");
    assert_eq!(c, 0, "{er}");
    assert_eq!(
        invocations(&e).len(),
        0,
        "re-asserting a NON-terminal status must not close anything; gh was \
         invoked with:\n{}",
        render(&invocations(&e))
    );
    assert!(
        live_file(&e).contains("dddd0054"),
        "precondition: the task must still be live, else the control is vacuous:\n{}",
        live_file(&e)
    );
}

/// GAP 1, CONTROL — GREEN NOW AND MUST STAY GREEN. `fail` must NOT close the
/// issue. A failed task is a DEFERRED RETRY by existing design
/// (`sync_plan` excludes `STATUS_FAILED` from the close arm on purpose), so its
/// issue stays open. Pinned here so "mirror the terminal transition" is not
/// over-reached into "mirror every status change".
#[test]
fn fail_still_leaves_the_issue_open() {
    let e = setup("failopen");
    write_live(
        &e,
        &task_block(
            "eeee0055",
            "will be retried",
            "pending",
            "EDIT-NOTES-ECHO: deferred retry.",
            Some(55),
            5,
        ),
    );

    let (c, o, er) = bl(&e, &["fail", "eeee0055", "--reason", "flaky dependency"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "{er}");
    assert_eq!(
        invs.len(),
        0,
        "`fail` is a deferred retry, not a terminal state: its issue must stay \
         OPEN and gh must not be invoked; gh was invoked with:\n{}",
        render(&invs)
    );
}

// ===========================================================================
// GAP 2 — the create-side `--body` must be bounded and honest about cutting.
// ===========================================================================

/// GAP 2, THE `add` PATH. `backlog add` with notes far over GitHub's limit must
/// still file the issue: the `--body` is bounded at 60000 BYTES, carries the
/// HEAD of the notes, drops the TAIL, and says `truncated` because content was
/// dropped (CLAUDE.md §4 — silent dropping is forbidden).
///
/// RED today: `build_issue_create_args` passes `notes` straight through, so the
/// body is ~85000 bytes and the real `gh` would reject it outright.
#[test]
fn add_bounds_the_issue_body_and_says_truncated() {
    let e = setup("addbig");
    write_live(&e, "");
    let notes = oversized_japanese_notes("CREATE-HEAD-FOXTROT: ", " CREATE-TAIL-FOXTROT");

    let id = add(&e, "oversized notes on create", &notes);
    let invs = invocations(&e);
    eprintln!(
        "id={id}\ngh invocations:\n{}\nlive store head:\n{}",
        render(&invs),
        live_file(&e).chars().take(400).collect::<String>()
    );

    let cr = creates(&invs);
    assert_eq!(
        cr.len(),
        1,
        "the oversized task must still be mirrored (bounded, not skipped); gh \
         was invoked with:\n{}",
        render(&invs)
    );
    assert!(
        live_file(&e).contains(&id),
        "the task must still be created locally:\n{}",
        live_file(&e).chars().take(400).collect::<String>()
    );

    let args = cr[0];
    let body = flag_value(args, "--body")
        .unwrap_or_else(|| panic!("`issue create` must carry --body; argv was {args:?}"));
    let bytes = body.len();
    assert!(
        bytes <= 60_000,
        "the create body must be bounded at 60000 BYTES, symmetric with the \
         close comment's `CLOSE_COMMENT_MAX`; it was {bytes} bytes \
         ({} chars)",
        body.chars().count()
    );
    assert!(
        body.contains("truncated"),
        "content was dropped, so the body must say the lowercase word \
         `truncated`; body was {bytes} bytes beginning {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        body.contains("CREATE-HEAD-FOXTROT:"),
        "the HEAD of the notes must survive the bound; body began {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        !body.contains("CREATE-TAIL-FOXTROT"),
        "the TAIL is the part that must be dropped — a body that still carries \
         it is not bounded by cutting the tail; body was {bytes} bytes"
    );
}

/// GAP 2, THE `sync --apply` CREATE ARM. The same bound must hold on the other
/// create call site, else reconciling a drifted store fails on exactly the
/// tasks that most need mirroring.
///
/// RED today for the same reason as the test above (one shared
/// `build_issue_create_args`), but asserted separately because a fix applied
/// only at `add`'s call site would leave this one unbounded.
#[test]
fn sync_apply_create_bounds_the_issue_body_and_says_truncated() {
    let e = setup("syncbig");
    let notes = oversized_japanese_notes("SYNC-HEAD-GOLF: ", " SYNC-TAIL-GOLF");
    write_live(
        &e,
        &task_block(
            "ffff0060",
            "oversized notes, unmirrored",
            "pending",
            &notes,
            None,
            6,
        ),
    );
    write_done(&e, "");

    let (c, o, er) = bl(&e, &["sync", "--only", "create", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "the create must succeed, not fail on an oversized body: {er}"
    );

    let cr = creates(&invs);
    assert_eq!(cr.len(), 1, "one create; got:\n{}", render(&invs));
    let args = cr[0];
    let body = flag_value(args, "--body")
        .unwrap_or_else(|| panic!("`issue create` must carry --body; argv was {args:?}"));
    let bytes = body.len();
    assert!(
        bytes <= 60_000,
        "the sync create arm's body must be bounded at 60000 BYTES too; it was \
         {bytes} bytes ({} chars)",
        body.chars().count()
    );
    assert!(
        body.contains("truncated"),
        "a cut body must say `truncated`; body was {bytes} bytes beginning {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        body.contains("SYNC-HEAD-GOLF:"),
        "the HEAD must survive; body began {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        !body.contains("SYNC-TAIL-GOLF"),
        "the TAIL must be the dropped part; body was {bytes} bytes"
    );
}

/// GAP 2, CONTROL — GREEN NOW AND MUST STAY GREEN. Notes that FIT must be
/// passed through verbatim and must NOT acquire a truncation marker. Without
/// this, "bound the body" could be satisfied by always trimming, or by stamping
/// `truncated` on every issue, and the marker would stop meaning anything.
#[test]
fn a_body_that_fits_is_passed_through_verbatim_and_unmarked() {
    let e = setup("addsmall");
    write_live(&e, "");
    let notes = "CREATE-SMALL-HOTEL: 日本語の短い記録。\n二行目もそのまま残ること。";

    let id = add(&e, "small notes on create", notes);
    let invs = invocations(&e);
    eprintln!("id={id}\ngh invocations:\n{}", render(&invs));

    let cr = creates(&invs);
    assert_eq!(cr.len(), 1, "one create; got:\n{}", render(&invs));
    let body = flag_value(cr[0], "--body")
        .unwrap_or_else(|| panic!("`issue create` must carry --body; argv was {:?}", cr[0]));
    assert!(
        body.contains(notes),
        "notes that fit must reach the issue body verbatim, newlines and all; \
         body was {body:?}"
    );
    assert!(
        !body.contains("truncated"),
        "nothing was dropped, so the body must NOT claim a truncation; body was {body:?}"
    );
}

// ===========================================================================
// GAP 3 — a third sync arm that re-pushes a stale body, gated behind
// `--only body`.
// ===========================================================================

/// Build the Gap-3 fixture through the CLI on purpose. A hand-written store
/// cannot express "the body WAS pushed, and then the notes changed" without
/// guessing the name of the per-task stamp the implementer will add — so the
/// fixture walks the real path instead: `add` (which pushes the body) and then
/// `edit --notes` (which makes it stale). Whatever the stamp is called, this
/// task is unambiguously stale afterwards and the one in `add`-only state is
/// unambiguously fresh.
///
/// Returns `(stale_id, issue_number)`.
fn stale_body_fixture(e: &Env, marker: &str) -> (String, String) {
    let id = add_pending(
        e,
        "task whose notes will change",
        "ORIGINAL-NOTES-INDIA: first draft.",
    );
    let invs = invocations(e);
    let cr = creates(&invs);
    assert_eq!(
        cr.len(),
        1,
        "fixture: add must have filed one issue; got:\n{}",
        render(&invs)
    );
    let number = live_file(e)
        .lines()
        .find_map(|l| l.trim().strip_prefix("issue_number = "))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            panic!(
                "fixture: add must have recorded an issue_number:\n{}",
                live_file(e)
            )
        });

    let (c, o, er) = bl(e, &["edit", &id, "--notes", marker]);
    assert_eq!(
        c, 0,
        "fixture: the notes edit must succeed\nstdout={o}\nstderr={er}"
    );
    assert!(
        live_file(e).contains(marker),
        "fixture: the new notes must be in the store:\n{}",
        live_file(e)
    );
    clear_log(e);
    (id, number)
}

/// GAP 3, THE CORE CASE. `sync --only body --apply` must re-push the stale body
/// with `gh issue edit <N> --body <body>` — not `issue comment` (which would
/// leave the stale body in place) and not a second `issue create` (which would
/// publish a duplicate).
///
/// RED today: `--only body` is not a value `SyncOnly` accepts, so clap rejects
/// the invocation; and even with the flag, `sync_plan` has no third arm.
#[test]
fn only_body_apply_pushes_issue_edit_with_the_new_body() {
    let e = setup("bodyapply");
    let marker = "UPDATED-NOTES-JULIET: the account of the work changed after the issue existed.";
    let (id, number) = stale_body_fixture(&e, marker);

    let (c, o, er) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "id={id} issue=#{number}\ncode={c}\nstdout={o}\nstderr={er}\n\
         gh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "`sync --only body --apply` must be a valid, successful invocation: \
         stdout={o:?} stderr={er:?}"
    );

    let ed = body_edits(&invs);
    assert_eq!(
        ed.len(),
        1,
        "the stale body must be re-pushed with exactly one `gh issue edit`; gh \
         was invoked with:\n{}",
        render(&invs)
    );
    let args = ed[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some(number.as_str()),
        "the body edit must target the task's own issue number; argv was {args:?}"
    );
    let body = flag_value(args, "--body").unwrap_or_else(|| {
        panic!(
            "the re-push must carry `--body` (replacing the body, not commenting \
             on it); argv was {args:?}"
        )
    });
    assert!(
        body.contains(marker),
        "the pushed body must carry the task's CURRENT notes; body was {body:?}"
    );
    assert!(
        !body.contains("ORIGINAL-NOTES-INDIA"),
        "the pushed body must be the current notes, not the stale ones it \
         replaces; body was {body:?}"
    );
    assert!(
        creates(&invs).is_empty(),
        "`--only body` must publish NO new issue; gh was invoked with:\n{}",
        render(&invs)
    );
    assert!(
        closes(&invs).is_empty(),
        "`--only body` must close NOTHING; gh was invoked with:\n{}",
        render(&invs)
    );
}

/// GAP 3, THE DRY RUN. An operator checks the plan before applying it, so
/// `sync --only body` must LIST the stale task and touch GitHub not at all.
///
/// RED today: clap rejects the `body` value, so there is no plan to print.
#[test]
fn only_body_dry_run_lists_the_stale_task_and_invokes_nothing() {
    let e = setup("bodydry");
    let marker = "UPDATED-NOTES-KILO: stale body, listed but not pushed.";
    let (id, number) = stale_body_fixture(&e, marker);

    let (c, o, er) = bl(&e, &["sync", "--only", "body"]);
    let invs = invocations(&e);
    eprintln!(
        "id={id} issue=#{number}\ncode={c}\nstdout={o}\nstderr={er}\n\
         gh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "`sync --only body` (dry run) must be a valid, successful invocation: \
         stdout={o:?} stderr={er:?}"
    );
    assert!(
        o.contains(&id),
        "the dry run must name the stale task so the operator can see WHAT \
         would be rewritten; stdout was {o:?}"
    );
    assert_eq!(
        invs.len(),
        0,
        "a dry run must not invoke gh at all; gh was invoked with:\n{}",
        render(&invs)
    );
}

/// GAP 3, IDEMPOTENCE AND RE-STALENESS. The whole arm turns on staleness being
/// derived from the store, so the observable cycle is pinned end to end:
/// apply once (1 push) -> apply again (0 pushes, nothing is stale) ->
/// `edit --notes` (stale again) -> apply (exactly 1 push, carrying the NEW
/// text).
///
/// This is the test that catches both degenerate implementations: one that
/// re-pushes every mirrored task on every run (a mass outward write — the
/// second phase would be non-zero), and one that pushes once and then never
/// notices a change again (the fourth phase would be zero).
#[test]
fn a_pushed_body_goes_quiet_until_the_notes_change_again() {
    let e = setup("bodycycle");
    let first = "UPDATED-NOTES-LIMA-ONE: first revision of the account.";
    let (id, number) = stale_body_fixture(&e, first);

    // Phase 1: the stale body is pushed.
    let (c1, o1, er1) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let inv1 = invocations(&e);
    eprintln!(
        "phase1: code={c1}\nstdout={o1}\nstderr={er1}\ngh:\n{}",
        render(&inv1)
    );
    assert_eq!(c1, 0, "phase 1 must succeed: stdout={o1:?} stderr={er1:?}");
    assert_eq!(
        body_edits(&inv1).len(),
        1,
        "phase 1: the stale body must be pushed once; gh:\n{}",
        render(&inv1)
    );

    // Phase 2: nothing changed since, so nothing is stale.
    clear_log(&e);
    let (c2, o2, er2) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let inv2 = invocations(&e);
    eprintln!(
        "phase2: code={c2}\nstdout={o2}\nstderr={er2}\ngh:\n{}",
        render(&inv2)
    );
    assert_eq!(c2, 0, "phase 2 must succeed: stdout={o2:?} stderr={er2:?}");
    assert_eq!(
        body_edits(&inv2).len(),
        0,
        "a body that was just pushed is NOT stale: an immediate re-run must plan \
         and perform ZERO body updates (otherwise every run rewrites every \
         mirrored issue); gh:\n{}",
        render(&inv2)
    );

    // Phase 3: the notes change, so the body is stale again.
    let second = "UPDATED-NOTES-LIMA-TWO: second revision, written after the first push.";
    clear_log(&e);
    let (c3, o3, er3) = bl(&e, &["edit", &id, "--notes", second]);
    eprintln!("phase3 edit: code={c3}\nstdout={o3}\nstderr={er3}");
    assert_eq!(c3, 0, "phase 3: the notes edit must succeed: {er3}");
    assert_eq!(
        invocations(&e).len(),
        0,
        "phase 3: editing notes must not itself push anything (that is what \
         `--only body` is for); gh:\n{}",
        render(&invocations(&e))
    );

    // Phase 4: exactly one update again, carrying the new text.
    let (c4, o4, er4) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let inv4 = invocations(&e);
    eprintln!(
        "phase4: code={c4}\nstdout={o4}\nstderr={er4}\ngh:\n{}",
        render(&inv4)
    );
    assert_eq!(c4, 0, "phase 4 must succeed: stdout={o4:?} stderr={er4:?}");
    let ed4 = body_edits(&inv4);
    assert_eq!(
        ed4.len(),
        1,
        "changed notes must make the body stale again — EXACTLY ONE update for \
         issue #{number}; gh:\n{}",
        render(&inv4)
    );
    let body = flag_value(ed4[0], "--body")
        .unwrap_or_else(|| panic!("the re-push must carry --body; argv was {:?}", ed4[0]));
    assert!(
        body.contains(second),
        "the second push must carry the NEWEST notes; body was {body:?}"
    );
    assert!(
        !body.contains(first),
        "the second push must not re-send the superseded text; body was {body:?}"
    );
}

/// GAP 3, THE DEFAULT MUST NOT CHANGE. `sync --apply` with no `--only` must
/// make ZERO body edits even when a stale body exists. A mass body rewrite over
/// a store with hundreds of mirrored issues is an outward-facing action nobody
/// asked for, so the new arm has to be opt-in — exactly as `--only create`
/// exists because publishing 58 public issues could not be allowed to ride on
/// the same switch as bookkeeping closes.
///
/// Non-vacuous by construction: the same store also holds one unmirrored
/// pending task and one terminal task with an open issue, so the default run
/// genuinely does work — one create and one close — and the assertion is that
/// the body edit is NOT among it.
///
/// RED today only in its precondition (there is no body arm to suppress yet);
/// it must be GREEN after, and it is the guard against the fix being applied to
/// `Both`.
#[test]
fn default_sync_apply_performs_no_body_edit() {
    let e = setup("bodydefault");
    let marker = "UPDATED-NOTES-MIKE: stale, and the default run must leave it alone.";
    let (id, number) = stale_body_fixture(&e, marker);
    append(
        &live_path(&e),
        &task_block("gggg0070", "unmirrored pending", "pending", "", None, 7),
    );
    append(
        &done_path(&e),
        &task_block("hhhh0071", "terminal, issue open", "done", "", Some(71), 8),
    );

    let (c, o, er) = bl(&e, &["sync", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "stale={id} issue=#{number}\ncode={c}\nstdout={o}\nstderr={er}\ngh:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "the default reconciliation must succeed: {er}");
    // Precondition: the default run really did the two arms it has always done.
    assert_eq!(
        creates(&invs).len(),
        1,
        "precondition: the default run must still file the unmirrored task \
         (otherwise this control proves nothing); gh:\n{}",
        render(&invs)
    );
    assert_eq!(
        closes(&invs).len(),
        1,
        "precondition: the default run must still close the stale issue; gh:\n{}",
        render(&invs)
    );
    assert_eq!(
        body_edits(&invs).len(),
        0,
        "`sync --apply` with no `--only` must make ZERO `issue edit` calls even \
         with a stale body present: the default must stay EXACTLY what it is \
         today, because rewriting hundreds of public issue bodies is not \
         something an unqualified `--apply` may do; gh:\n{}",
        render(&invs)
    );
}

/// GAP 3, THE OTHER TWO SCOPES. `--only create` and `--only close` must each
/// make zero body edits, so the scopes stay disjoint in both directions (the
/// `--only body` test above already pins the converse).
#[test]
fn only_create_and_only_close_perform_no_body_edit() {
    let e = setup("bodyscopes");
    let marker = "UPDATED-NOTES-NOVEMBER: stale under every other scope too.";
    let (id, number) = stale_body_fixture(&e, marker);
    append(
        &live_path(&e),
        &task_block("iiii0072", "unmirrored pending", "pending", "", None, 9),
    );
    append(
        &done_path(&e),
        &task_block("jjjj0073", "terminal, issue open", "done", "", Some(73), 10),
    );

    let (c, o, er) = bl(&e, &["sync", "--only", "create", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "only create: stale={id} issue=#{number}\ncode={c}\nstdout={o}\n\
         stderr={er}\ngh:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "{er}");
    assert_eq!(
        creates(&invs).len(),
        1,
        "precondition: `--only create` must still file the unmirrored task; gh:\n{}",
        render(&invs)
    );
    assert_eq!(
        body_edits(&invs).len(),
        0,
        "`--only create` must make no body edit; gh:\n{}",
        render(&invs)
    );

    clear_log(&e);
    let (c, o, er) = bl(&e, &["sync", "--only", "close", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "only close: code={c}\nstdout={o}\nstderr={er}\ngh:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "{er}");
    assert_eq!(
        closes(&invs).len(),
        1,
        "precondition: `--only close` must still close the stale issue; gh:\n{}",
        render(&invs)
    );
    assert_eq!(
        body_edits(&invs).len(),
        0,
        "`--only close` must make no body edit; gh:\n{}",
        render(&invs)
    );
}

/// GAP 3, FAIL-CLOSED RECORDING. A `gh issue edit` that EXITS NON-ZERO did not
/// happen, whatever else it carried, so it must NOT be recorded as pushed — the
/// next `--only body` must still plan it. A push recorded on failure is the
/// `3.`-class bug: "could not update" written down as "updated", and nothing
/// downstream could ever tell.
#[test]
fn a_failed_body_push_is_not_recorded_and_is_retried() {
    let e = setup("bodyfail");
    let marker = "UPDATED-NOTES-OSCAR: the push will be refused.";
    let (id, number) = stale_body_fixture(&e, marker);

    // The stub now fails AFTER logging, so the attempt is observable and the
    // outcome is unambiguous.
    install_stub(&e, false);
    let (c1, o1, er1) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let inv1 = invocations(&e);
    eprintln!(
        "failing run: id={id} issue=#{number}\ncode={c1}\nstdout={o1}\n\
         stderr={er1}\ngh:\n{}",
        render(&inv1)
    );
    assert_eq!(
        body_edits(&inv1).len(),
        1,
        "the push must have been ATTEMPTED (otherwise the retry below proves \
         nothing); gh:\n{}",
        render(&inv1)
    );

    // Now let gh succeed and re-run: the unrecorded update must still be there.
    install_stub(&e, true);
    clear_log(&e);
    let (c2, o2, er2) = bl(&e, &["sync", "--only", "body", "--apply"]);
    let inv2 = invocations(&e);
    eprintln!(
        "retry run: code={c2}\nstdout={o2}\nstderr={er2}\ngh:\n{}",
        render(&inv2)
    );
    assert_eq!(
        c2, 0,
        "the retry must succeed: stdout={o2:?} stderr={er2:?}"
    );
    let ed = body_edits(&inv2);
    assert_eq!(
        ed.len(),
        1,
        "a FAILED body push must not be recorded, so the next `--only body` \
         must still plan and perform it; gh:\n{}",
        render(&inv2)
    );
    let body = flag_value(ed[0], "--body")
        .unwrap_or_else(|| panic!("the retry must carry --body; argv was {:?}", ed[0]));
    assert!(
        body.contains(marker),
        "the retry must push the current notes; body was {body:?}"
    );
}

/// GAP 3, THE NOISE CONTROL. `session-start` must NOT grow a body-drift line.
///
/// The SessionStart hook already states the two drift counts, and those can
/// both reach zero: the operator runs `sync --only close --apply` (bookkeeping)
/// or `--only create --apply` (a deliberate publish) and the number falls. A
/// body-staleness count cannot — driving it to zero means rewriting hundreds of
/// public issue bodies, which the test above pins as something the default run
/// may never do. A count that can never reach zero is permanent noise, and
/// permanent noise trains the reader to skip the whole section, including the
/// two lines that DO mean something. Filed as ticket 7438ea3a.
///
/// (Both the task title and the notes are injected into the context verbatim,
/// so this fixture's text deliberately contains no occurrence of the word this
/// test searches for.)
#[test]
fn session_start_does_not_report_body_staleness() {
    let e = setup("bodyquiet");
    let marker = "UPDATED-NOTES-PAPA: stale, and the hook must stay quiet about it.";
    let (id, number) = stale_body_fixture(&e, marker);

    let out = session_start(&e);
    eprintln!("stale={id} issue=#{number}\nsession-start stdout={out}");
    let ctx = out
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            v["additionalContext"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| panic!("the hook must still emit one JSON line of context: {out:?}"));
    assert!(
        ctx.contains(&id),
        "precondition: the stale task must be in the injected context at all, \
         else this control is vacuous: {ctx:?}"
    );
    assert!(
        !ctx.to_lowercase().contains("body"),
        "the SessionStart hook must NOT grow a body-drift line: a count that \
         can never reach zero without a mass outward-facing write is permanent \
         noise (ticket 7438ea3a). Injected context was: {ctx:?}"
    );
    assert!(
        !ctx.contains("to update"),
        "no body-update count may be reported in the drift section: {ctx:?}"
    );
}
