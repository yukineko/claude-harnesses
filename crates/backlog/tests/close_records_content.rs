#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A mirror close must RECORD THE CONTENT, not just flip the issue state.
//!
//! Measured today (2026-10-02) at `crates/backlog/src/github.rs`
//! (`build_issue_close_args`), the argv a close is performed with is exactly
//! `["issue","close","<number>","--reason",<reason>]` — a bare close. The task's
//! own `notes`, which is the only place the work's outcome is written down, are
//! never pushed anywhere. So an issue that was closed by this mirror carries no
//! answer to "what happened?" — the content lives only in a local
//! `.backlog/tasks.done.toml` that nobody reading the issue can see. Dropping
//! content on the floor without saying so is the thing CLAUDE.md §4 forbids.
//!
//! The contract pinned here: every close carries `--comment <body>`, where the
//! body carries THAT task's notes and names the task, is never empty, and when
//! the notes are too large to fit says so with the literal word `truncated`
//! rather than silently shortening.
//!
//! Written by a disinterested party BEFORE the implementation (CLAUDE.md
//! §2(a)); every test that pins new behaviour is expected to be RED right now.
//!
//! Like `done_closes_issue_inline.rs` and `sync_only_scope.rs`, these drive the
//! real binary against a stub `gh` that records every argv, so what is asserted
//! is the GitHub-visible write actually attempted. Unlike those two, the stub
//! records **one argument per record with explicit delimiters**: the comment
//! body is multi-line and contains spaces, so a `"$*"`-style log cannot be
//! taken apart again into individual argument values.
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Record delimiters. One `INV` per `gh` invocation, then one
/// `ARG` ... `END` pair per argument, so an argument value containing spaces or
/// newlines (the whole point of these tests) is still recoverable verbatim.
const INV: &str = "==BL-INV==\n";
const ARG: &str = "==BL-ARG==\n";
const END: &str = "\n==BL-END==\n";

/// A stub `gh` body that logs every argument individually and, for
/// `issue create`, answers with a new issue URL the way the real thing does.
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
    let t = std::env::temp_dir().join(format!("bl-closecontent-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    // `bl()` runs the binary with env_clear() + PATH=bin, so every tool the
    // stub itself shells out to has to live in that same dir.
    symlink(tool_path("grep"), bin.join("grep")).unwrap();

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

fn write_live(e: &Env, body: &str) {
    std::fs::write(e.repo.join(".backlog/tasks.toml"), body).unwrap();
}

/// Terminal tasks live in the done file. The "terminal task whose issue was
/// never closed" shape is not reachable through the CLI (because `done` closes
/// inline), so it is written by hand — exactly as `sync_only_scope.rs` does.
fn write_done(e: &Env, body: &str) {
    std::fs::write(e.repo.join(".backlog/tasks.done.toml"), body).unwrap();
}

fn done_file(e: &Env) -> String {
    std::fs::read_to_string(e.repo.join(".backlog/tasks.done.toml")).unwrap_or_default()
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

fn closes(invs: &[Vec<String>]) -> Vec<&Vec<String>> {
    invs.iter()
        .filter(|a| a.len() >= 2 && a[0] == "issue" && a[1] == "close")
        .collect()
}

/// The value that FOLLOWS `flag`, or None when the flag is absent. Asserting on
/// this rather than on a rigid full-argv equality keeps the relative order of
/// `--reason` / `--comment` out of the contract.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).cloned()
}

/// Short, readable rendering of an invocation for failure messages, with huge
/// values elided so an 80k-char body cannot drown the assertion text.
fn render(invs: &[Vec<String>]) -> String {
    invs.iter()
        .map(|a| {
            let args: Vec<String> = a
                .iter()
                .map(|v| {
                    if v.chars().count() > 160 {
                        format!(
                            "<{} chars: {}...>",
                            v.chars().count(),
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

/// Assert the shared shape every close must have: it carries a `--comment`, and
/// that comment is never an empty string. An empty flag value is not
/// "recording the content" — it is a close that merely looks like one.
fn assert_comment_present(args: &[String], what: &str) -> String {
    let body = flag_value(args, "--comment").unwrap_or_else(|| {
        panic!("{what}: the close must carry --comment; argv was {args:?}");
    });
    assert!(
        !body.trim().is_empty(),
        "{what}: --comment must never be passed an empty body; argv was {args:?}"
    );
    body
}

// ---------------------------------------------------------------------------

/// (0) HARNESS SELF-CHECK — must pass before AND after. Every assertion in this
/// file rests on `invocations()` recovering individual argument values from the
/// stub's log, and the values this file is about contain spaces, newlines and
/// tens of thousands of characters. A `"$*"`-joined log could not do that, and a
/// harness that silently mangled the body would make a missing-content bug look
/// like a present one (or the reverse).
///
/// So the round-trip is observed directly: hand the stub awkward arguments and
/// assert they come back byte-identical. If THIS fails, no other verdict in the
/// file means anything.
#[test]
fn the_argv_log_round_trips_multiline_and_huge_arguments() {
    let e = setup("selfcheck");
    let multiline = "first line\n  second line with  spaces\n\nfourth after blank\ttab";
    let quoted = "has \"quotes\" and 'apostrophes' and == signs ==BL== and a trailing space ";
    let huge: String = std::iter::repeat_n("abcdefghij", 6_000).collect();
    assert_eq!(huge.chars().count(), 60_000);

    let st = Command::new(e.bin.join("gh"))
        .args([
            "issue",
            "close",
            "7",
            "--reason",
            "not planned",
            "--comment",
        ])
        .arg(multiline)
        .arg(quoted)
        .arg(&huge)
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
            "close".to_string(),
            "7".to_string(),
            "--reason".to_string(),
            "not planned".to_string(),
            "--comment".to_string(),
            multiline.to_string(),
            quoted.to_string(),
            huge.clone(),
        ],
        "every argument must round-trip verbatim through the log"
    );
    assert_eq!(
        flag_value(&invs[0], "--comment").as_deref(),
        Some(multiline),
        "flag_value must return the value that follows the flag, newlines and all"
    );
    assert_eq!(
        flag_value(&invs[0], "--reason").as_deref(),
        Some("not planned"),
        "a flag value containing a space must not be split"
    );
    assert_eq!(
        flag_value(&invs[0], "--no-such-flag"),
        None,
        "an absent flag must read as absent, not as some neighbouring value"
    );
}

/// (1) THE CORE CONTRACT, inline path. `done` on a task that already has issue
/// #51 must make exactly ONE `gh` call, and it must close #51 as `completed`
/// with a comment carrying that task's notes verbatim and naming the task.
///
/// Dies if the close stays bare (today), if the comment is a generic "closed by
/// backlog" that does not carry the notes, if it closes some other number, or
/// if recording the content costs an extra API call per close.
#[test]
fn done_close_carries_the_tasks_notes_and_id_in_a_comment() {
    let e = setup("done");
    write_live(
        &e,
        &task_block(
            "aaaa0001",
            "inline close records content",
            "pending",
            "OUTCOME-MARKER-ALPHA: fixed the off-by-one in the reaper.\nsecond line of the record.",
            Some(51),
            1,
        ),
    );

    let (c, o, er) = bl(&e, &["done", "aaaa0001"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "done must succeed: {er}");

    assert_eq!(
        invs.len(),
        1,
        "exactly one gh invocation (the close itself must carry the content, \
         not a second comment call); got:\n{}",
        render(&invs)
    );
    let cl = closes(&invs);
    assert_eq!(cl.len(), 1, "exactly one close; got:\n{}", render(&invs));
    let args = cl[0];

    assert_eq!(
        args.get(2).map(String::as_str),
        Some("51"),
        "the close must target the task's OWN issue number; argv was {args:?}"
    );
    assert_eq!(
        flag_value(args, "--reason").as_deref(),
        Some("completed"),
        "a done task closes as completed; argv was {args:?}"
    );

    let body = assert_comment_present(args, "done close");
    assert!(
        body.contains("OUTCOME-MARKER-ALPHA: fixed the off-by-one in the reaper."),
        "the comment must carry the task's notes verbatim; body was {body:?}"
    );
    assert!(
        body.contains("second line of the record."),
        "the comment must carry ALL of the notes, not just the first line; body was {body:?}"
    );
    assert!(
        body.contains("aaaa0001"),
        "the comment must name the task id, so the issue points back at the record; \
         body was {body:?}"
    );
}

/// (2) THE PAIRING TEST. Two drifted tasks, two distinct notes. Each close must
/// carry ITS OWN task's notes.
///
/// This is the test that catches the implementation that reaches for the wrong
/// loop variable — building the comment from, say, the first task in the plan,
/// or from the last one, or from the task the enclosing iterator happens to
/// hold. Such a bug passes test (1) (one task, so right and wrong agree) and is
/// invisible in any assertion that only checks "a comment was present".
#[test]
fn sync_apply_pairs_each_close_with_its_own_tasks_notes() {
    let e = setup("pair");
    write_live(&e, "");
    write_done(
        &e,
        &format!(
            "{}{}",
            task_block(
                "bbbb0061",
                "drifted one",
                "done",
                "NOTES-ALPHA-SIXTYONE: rewired the lease reaper.",
                Some(61),
                3,
            ),
            task_block(
                "cccc0062",
                "drifted two",
                "done",
                "NOTES-BRAVO-SIXTYTWO: deleted the dead migration.",
                Some(62),
                4,
            ),
        ),
    );

    let (c, o, er) = bl(&e, &["sync", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "a fully-applied close reconciliation must exit 0: {er}"
    );

    let cl = closes(&invs);
    assert_eq!(
        cl.len(),
        2,
        "both drifted issues must be closed; got:\n{}",
        render(&invs)
    );

    let by_number = |n: &str| -> &Vec<String> {
        cl.iter()
            .copied()
            .find(|a| a.get(2).map(String::as_str) == Some(n))
            .unwrap_or_else(|| panic!("no close targeting #{n}; got:\n{}", render(&invs)))
    };

    let a61 = by_number("61");
    let b61 = assert_comment_present(a61, "close #61");
    assert!(
        b61.contains("NOTES-ALPHA-SIXTYONE: rewired the lease reaper."),
        "#61's comment must carry #61's task notes; body was {b61:?}"
    );
    assert!(
        !b61.contains("NOTES-BRAVO-SIXTYTWO"),
        "#61's comment must NOT carry the other task's notes (wrong loop variable); \
         body was {b61:?}"
    );
    assert!(
        b61.contains("bbbb0061") && !b61.contains("cccc0062"),
        "#61's comment must name its own task id and no other; body was {b61:?}"
    );

    let a62 = by_number("62");
    let b62 = assert_comment_present(a62, "close #62");
    assert!(
        b62.contains("NOTES-BRAVO-SIXTYTWO: deleted the dead migration."),
        "#62's comment must carry #62's task notes; body was {b62:?}"
    );
    assert!(
        !b62.contains("NOTES-ALPHA-SIXTYONE"),
        "#62's comment must NOT carry the other task's notes (wrong loop variable); \
         body was {b62:?}"
    );
    assert!(
        b62.contains("cccc0062") && !b62.contains("bbbb0061"),
        "#62's comment must name its own task id and no other; body was {b62:?}"
    );
}

/// (3) `cancelled` keeps its own reason AND still records the content.
/// Abandoned work must not be filed on GitHub as completed, and a close that
/// gains a comment must not quietly lose the reason it had.
///
/// (`cancelled` is only reachable from a hand-written store — see
/// `store::STATUS_CANCELLED`'s doc comment — hence `sync --apply`.)
#[test]
fn cancelled_close_is_not_planned_and_still_records_the_content() {
    let e = setup("cancel");
    write_live(&e, "");
    write_done(
        &e,
        &task_block(
            "dddd0077",
            "abandoned work",
            "cancelled",
            "NOTES-CHARLIE-CANCEL: superseded by the new scheduler; not doing this.",
            Some(77),
            5,
        ),
    );

    let (c, o, er) = bl(&e, &["sync", "--apply"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "{er}");

    let cl = closes(&invs);
    assert_eq!(cl.len(), 1, "one close; got:\n{}", render(&invs));
    let args = cl[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some("77"),
        "argv was {args:?}"
    );
    assert_eq!(
        flag_value(args, "--reason").as_deref(),
        Some("not planned"),
        "cancelled work must close as `not planned`, never `completed`; argv was {args:?}"
    );
    let body = assert_comment_present(args, "cancelled close");
    assert!(
        body.contains("NOTES-CHARLIE-CANCEL: superseded by the new scheduler; not doing this."),
        "a cancelled close must record its notes too; body was {body:?}"
    );
    assert!(
        body.contains("dddd0077"),
        "the comment must name the task id; body was {body:?}"
    );
}

/// (4) EMPTY NOTES. The close must still happen, and the comment must still say
/// something real: the task id and the terminal state it reached. What must NOT
/// happen is `--comment ""` — an empty flag value records nothing while making
/// the argv look like it does, which is exactly the "looks checked, wasn't"
/// shape CLAUDE.md §3 is about.
///
/// Dies if the implementation builds the body as the raw notes (empty → empty
/// comment), and dies if it drops `--comment` whenever the notes are empty.
#[test]
fn empty_notes_still_close_with_a_nonempty_comment_naming_id_and_state() {
    let e = setup("empty");
    write_live(
        &e,
        &task_block(
            "eeee0088",
            "no notes were written",
            "pending",
            "",
            Some(88),
            6,
        ),
    );

    let (c, o, er) = bl(&e, &["done", "eeee0088"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "done must succeed: {er}");

    let cl = closes(&invs);
    assert_eq!(
        cl.len(),
        1,
        "the close must still happen; got:\n{}",
        render(&invs)
    );
    let args = cl[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some("88"),
        "argv was {args:?}"
    );

    // No empty flag value anywhere: if --comment is there, it carries something.
    if let Some(i) = args.iter().position(|a| a == "--comment") {
        assert!(
            args.get(i + 1).is_some_and(|v| !v.trim().is_empty()),
            "--comment must never be given an empty body; argv was {args:?}"
        );
    }
    let body = assert_comment_present(args, "empty-notes close");
    assert!(
        body.contains("eeee0088"),
        "with no notes to carry, the comment must at least name the task; body was {body:?}"
    );
    assert!(
        body.to_lowercase().contains("done"),
        "the comment must name the terminal state the task reached; body was {body:?}"
    );
}

/// (5) BOUND + HONESTY. Notes far larger than any comment should be must still
/// close, the body must be bounded at 60000 characters, and because content was
/// dropped the body must say so with the literal lowercase word `truncated`.
///
/// Dies three ways: if an unbounded body is pushed (GitHub would reject it and
/// the close would fail), if the body is silently shortened with no marker
/// (CLAUDE.md §4: dropping content invisibly), and if the oversized task is
/// skipped instead of closed.
#[test]
fn oversized_notes_are_truncated_with_a_marker_and_a_bounded_body() {
    let e = setup("big");
    // > 80000 chars. The filler is plain ASCII and contains none of the log's
    // record delimiters, so the value stays recoverable verbatim.
    let mut notes = String::from("NOTES-DELTA-HUGE-HEAD: ");
    while notes.chars().count() <= 80_000 {
        notes.push_str("the quick brown fox jumps over the lazy dog 0123456789 ");
    }
    assert!(
        notes.chars().count() > 80_000,
        "harness: the oversized notes must actually be oversized"
    );
    write_live(
        &e,
        &task_block("ffff0099", "enormous notes", "pending", &notes, Some(99), 7),
    );

    let (c, o, er) = bl(&e, &["done", "ffff0099"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(c, 0, "done must succeed: {er}");

    let cl = closes(&invs);
    assert_eq!(
        cl.len(),
        1,
        "the close must still happen; got:\n{}",
        render(&invs)
    );
    let args = cl[0];
    assert_eq!(
        args.get(2).map(String::as_str),
        Some("99"),
        "argv was {args:?}"
    );

    let body = assert_comment_present(args, "oversized close");
    let n = body.chars().count();
    assert!(
        n <= 60_000,
        "the comment body must be bounded at 60000 chars; it was {n}"
    );
    assert!(
        body.contains("truncated"),
        "content was dropped, so the body must say `truncated` (CLAUDE.md §4: \
         silent dropping is forbidden); body had {n} chars and began {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        body.contains("NOTES-DELTA-HUGE-HEAD:"),
        "a truncated body must still carry the beginning of the notes; body began {:?}",
        body.chars().take(200).collect::<String>()
    );
    assert!(
        body.contains("ffff0099"),
        "the comment must name the task id even when truncated; body began {:?}",
        body.chars().take(200).collect::<String>()
    );
}

/// (6a) REGRESSION CONTROL — must pass before AND after. A dry run reports the
/// plan and touches GitHub not at all. If recording the content ever moves work
/// into the planning phase (fetching, templating against the live issue), this
/// is what notices.
#[test]
fn dry_run_sync_performs_no_gh_invocation() {
    let e = setup("dry");
    write_live(&e, "");
    write_done(
        &e,
        &task_block(
            "bbbb0061",
            "drifted one",
            "done",
            "NOTES-ALPHA-SIXTYONE: rewired the lease reaper.",
            Some(61),
            3,
        ),
    );

    let (c, o, er) = bl(&e, &["sync"]);
    eprintln!("code={c}\nstdout={o}\nstderr={er}");
    assert_eq!(c, 0, "{er}");
    assert!(
        o.contains("1 issue(s) to close"),
        "the dry run must still plan the close (otherwise this control is vacuous): {o:?}"
    );
    assert_eq!(
        invocations(&e).len(),
        0,
        "a dry run must not invoke gh at all; got:\n{}",
        render(&invocations(&e))
    );
}

/// (6b) REGRESSION CONTROL — must pass before AND after. When `gh` fails, the
/// local completion still stands, the close is NOT recorded, and the next
/// `sync` still plans it. Adding a comment to the close must not turn a failed
/// close into a recorded one — a close that errored is not a close, whatever
/// else the invocation carried.
#[test]
fn a_failed_close_is_not_recorded_and_stays_in_the_next_sync_plan() {
    let e = setup("ghfail");
    write_live(
        &e,
        &task_block(
            "gggg0100",
            "close will fail",
            "pending",
            "NOTES-ECHO-FAIL: the mirror will not accept this.",
            Some(100),
            8,
        ),
    );
    install_stub(&e, false);

    let (c, o, er) = bl(&e, &["done", "gggg0100"]);
    let invs = invocations(&e);
    eprintln!(
        "code={c}\nstdout={o}\nstderr={er}\ngh invocations:\n{}",
        render(&invs)
    );
    assert_eq!(
        c, 0,
        "the local completion is authoritative and must succeed: {er}"
    );
    assert_eq!(
        closes(&invs).len(),
        1,
        "the close must have been attempted; got:\n{}",
        render(&invs)
    );
    let done = done_file(&e);
    assert!(
        !done.contains("issue_closed_at"),
        "an UNCONFIRMED close must not be stamped: {done}"
    );

    let (c, o, er) = bl(&e, &["sync"]);
    eprintln!("code={c}\nstdout={o}\nstderr={er}");
    assert_eq!(c, 0, "{er}");
    assert!(
        o.contains("1 issue(s) to close"),
        "an unconfirmed close must still be in the next sync plan: {o:?}"
    );
}
