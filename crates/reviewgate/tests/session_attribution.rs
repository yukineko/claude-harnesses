//! RED (layer 2) for backlog `1e44bfd9` — reviewgate must review only the files
//! **this session** edited, not every file dirty in a shared working tree.
//!
//! # The defect
//!
//! `review::evaluate` (`src/review.rs:96-118`) feeds `git::changed_files(root)`
//! straight into `reviewable_files`. `changed_files` shells out to
//! `git diff --name-only`, `git diff --cached --name-only` and
//! `git ls-files --others --exclude-standard` — all three describe the WORKING
//! TREE, and a working tree carries no session identity. When two Claude Code
//! sessions share the primary tree (forbidden by CLAUDE.md 第8節, observed
//! anyway), reviewgate hands session A a file session B wrote and blocks A's
//! stop with a review demand for a diff A never made.
//!
//! Observed twice, both in the ticket:
//!   * 2026-07-21, session `c6a1fdbf` — blocked on `scripts/check-fail-open.py`,
//!     actually written by concurrent session `e0d74f05`.
//!   * 2026-07-23, session `143f3d21` — same class, via pre-commit
//!     `check-doc-claims`.
//!
//! This is a fail-open *vector*, not just noise: the cheapest way out of a block
//! you cannot fix is to create the project-root `.reviewgate-skip` file, which
//! is consumed exactly once and shared between the sessions — so clearing your
//! own phantom block silently waves through the other session's real one
//! (CLAUDE.md 第5節).
//!
//! # What is pinned here, and at what altitude
//!
//! These are BEHAVIOURAL tests driven through the `reviewgate review` binary
//! with a real Stop-hook payload on stdin. That is deliberate: the fix needs
//! `transcript_path` plumbed into `evaluate`, and pinning the behaviour at the
//! hook boundary leaves the implementer free to choose that plumbing instead of
//! having a signature dictated by a test. Nothing in this file names a private
//! function, so it compiles today against the unmodified crate.
//!
//! Two observation channels, used together:
//!   * **stdout** — the `{"decision":"block","reason":…}` JSON. This is the
//!     human/model-visible reason.
//!   * **`$HOME/.reviewgate/state/log.jsonl`** — `log_event` records the exact
//!     `files` list that `Decision::Block` carried (`src/main.rs`). That is the
//!     machine-readable "what did it actually review", and is what the
//!     narrowing assertions key on rather than scraping Japanese prose.
//!
//! # Contract
//!
//! | transcript answer | review set | reason must say |
//! |---|---|---|
//! | `Known(mine)` | `changed − (peers − mine)` | how many were excluded, and which |
//! | `Undetermined`  | **the full `changed` set — no narrowing** | attribution was undetermined |
//!
//! The `Undetermined` row is the fail-closed one (CLAUDE.md 第3節): narrowing on
//! an unreadable transcript would rewrite "I cannot tell whose these are" into
//! "none of these are mine", which is the permissive answer.
//!
//! ## Why the first row is a subtraction and not `changed ∩ mine`
//!
//! This file's first version wrote that row as `changed ∩ mine`, and it was
//! wrong for the same reason the second row is right. An intersection reads
//! "absent from my footprint" as "not mine", and that read is refuted by
//! measurement, not by argument. Re-measured independently on 2026-09-09
//! against this repository's own session `e9ebfcb6` (13.2 MB transcript, 92
//! recorded `Edit`/`Write` paths, plus 31 sidechain transcripts):
//!
//! ```text
//! $ git diff --stat crates/harness-core/src/lib.rs
//!  crates/harness-core/src/lib.rs | 1 +
//! Edit/Write blocks naming harness-core/src/lib.rs (parent + sidechains): 0
//! Bash blocks naming it: 1
//!   sed -i '/^pub mod boundary;$/i pub mod attribution;' crates/harness-core/src/lib.rs
//! ```
//!
//! That line is this session's own edit — it is what makes the new module
//! reachable at all — and it is in NO footprint, because `sed -i` is a `Bash`
//! tool_use and `harness_core::transcript::EDIT_TOOLS` collects only
//! `Edit`/`Write`/`MultiEdit`/`NotebookEdit`. The same session ran 24 `sed -i`
//! commands. So:
//!
//! ```text
//! f in my footprint      ==> f IS mine     (sound)
//! f not in my footprint  ==> UNKNOWN       (NOT "f is not mine")
//! ```
//!
//! An intersection collapses the second line into the third, which is exactly
//! the 判定不能→permissive collapse 第3節 forbids — and it is worse here than
//! the second row's case, because it fails *silently*: a session that did its
//! editing through a heredoc or `sed -i` intersects to ∅, `reviewable_files`
//! comes back empty, and `evaluate` returns `allow("no-reviewable-changes")`.
//! The gate goes DARK, which is the failure mode
//! `own_edit_is_still_reviewed_across_the_abs_rel_boundary` below already
//! exists to catch from the other direction.
//!
//! Hence: an exclusion must be a POSITIVE observation about a peer. A file
//! leaves the review set only when another session's transcript claims it and
//! mine does not. `mine` still wins on an overlap — a file I touched is never
//! someone else's problem to review.
//!
//! Paths need normalising before the intersection: `changed_files` returns paths
//! **relative to the repo root**, a transcript records **absolute** ones. A
//! comparison that never matches would narrow every review to nothing — the gate
//! would go dark rather than red — and would still satisfy a careless test.
//! `own_edit_is_still_reviewed_across_the_abs_rel_boundary` is the control for
//! exactly that.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// What one `reviewgate review` invocation produced.
struct Outcome {
    /// `"block"` / `"allow"` — from stdout JSON (allow prints nothing in hook mode).
    decision: String,
    /// The model-facing reason text (empty on allow).
    reason: String,
    /// `verdict` from the JSONL log line this run appended.
    log_verdict: String,
    /// `files` from that same log line: exactly what the gate reviewed.
    log_files: Vec<String>,
}

/// An isolated HOME + a real git repo with one committed file.
struct Fixture {
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("reviewgate-attrib-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(git(&repo, args).success(), "git setup failed: {args:?}");
        }
        std::fs::write(repo.join("base.rs"), "fn base() {}\n").expect("write base");
        assert!(git(&repo, &["add", "base.rs"]).success());
        assert!(git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "init"]
        )
        .success());
        // Canonicalise: on some hosts the temp dir is reached through a symlink,
        // and a transcript recorded under one spelling must still line up with a
        // repo root spelled the other way.
        let repo = repo.canonicalize().unwrap_or(repo);
        Fixture { home, repo }
    }

    /// Create an untracked source file (so it shows up in `changed_files`).
    fn dirty(&self, name: &str, body: &str) {
        std::fs::write(self.repo.join(name), body).expect("write dirty file");
    }

    /// Absolute path of a file in the repo, as a transcript would record it.
    fn abs(&self, name: &str) -> String {
        self.repo.join(name).to_string_lossy().into_owned()
    }

    /// The project-slug directory of a realistic Claude Code transcript store.
    ///
    /// Layout matters and is not decoration. Transcripts live at
    /// `$HOME/.claude/projects/<project-slug>/<session-id>.jsonl`, and
    /// `harness_core::attribution::attribute_from_transcript` finds a PEER by
    /// walking two levels up from this session's own transcript and scanning
    /// every slug it finds. The first version of this fixture wrote the
    /// transcript straight into `$HOME`, which makes that grandparent
    /// `std::env::temp_dir()` — so no peer transcript existed anywhere the code
    /// looks, and every test built the *unattributed* case regardless of the
    /// scenario its name described. Two of them then asserted the *peer*
    /// outcome against it. Keep new transcripts in this tree.
    fn slug_dir(&self) -> PathBuf {
        let d = self
            .home
            .join(".claude")
            .join("projects")
            .join("-home-t-project");
        std::fs::create_dir_all(&d).expect("create projects slug dir");
        d
    }

    /// This session's transcript.
    fn transcript(&self, edited: &[String], read_only: &[String]) -> PathBuf {
        self.transcript_named("this-session", edited, read_only)
    }

    /// A **concurrent** session's transcript, in the same projects tree — the
    /// artefact that makes a file "someone else's" rather than merely
    /// unattributed. Without one on disk there is no peer, only an absence of
    /// evidence, and an absence of evidence is not an exclusion.
    fn peer_transcript(&self, edited: &[String], read_only: &[String]) -> PathBuf {
        self.transcript_named("peer-session", edited, read_only)
    }

    /// Write a transcript whose assistant turns record an `Edit` of each of
    /// `edited` (absolute paths) plus a `Read` of each of `read_only`.
    fn transcript_named(&self, session: &str, edited: &[String], read_only: &[String]) -> PathBuf {
        let p = self.slug_dir().join(format!("{session}.jsonl"));
        let mut body = String::new();
        body.push_str(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for f in read_only {
            body.push_str(&tool_use_line("Read", "file_path", f));
            body.push('\n');
        }
        for f in edited {
            body.push_str(&tool_use_line("Edit", "file_path", f));
            body.push('\n');
        }
        std::fs::write(&p, body).expect("write transcript");
        p
    }

    /// Run the Stop hook once with `transcript_path` set to `tp`.
    fn run(&self, session: &str, tp: &str) -> Outcome {
        let payload = serde_json::json!({
            "session_id": session,
            "transcript_path": tp,
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        self.run_in(&self.repo, session, &payload)
    }

    /// Run the Stop hook with an arbitrary cwd/payload (used by the
    /// NotRepo / Failed regression controls).
    fn run_in(&self, cwd: &Path, session: &str, payload: &str) -> Outcome {
        let bin = env!("CARGO_BIN_EXE_reviewgate");
        let mut child = Command::new(bin)
            .arg("review")
            .current_dir(cwd)
            .env("HOME", &self.home)
            // Make sure a developer's own env can never silently neuter the gate
            // and turn a red test green.
            .env_remove("REVIEWGATE_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("reviewgate spawns");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write payload");
        let out = child.wait_with_output().expect("reviewgate runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "the Stop hook must always exit 0 toward Claude; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let (decision, reason) = match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => (
                v.get("decision")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                v.get("reason")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            Err(_) => ("allow".to_string(), String::new()),
        };
        let (log_verdict, log_files) = self.last_log_for(session);
        Outcome {
            decision,
            reason,
            log_verdict,
            log_files,
        }
    }

    /// The most recent log line for `session`: `(verdict, files)`.
    fn last_log_for(&self, session: &str) -> (String, Vec<String>) {
        let path = self.home.join(".reviewgate/state/log.jsonl");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let line = text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|v| v.get("session").and_then(|s| s.as_str()) == Some(session))
            .unwrap_or_else(|| panic!("no log line for session {session} in:\n{text}"));
        let verdict = line
            .get("verdict")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let files = line
            .get("files")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        (verdict, files)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn git(dir: &Path, args: &[&str]) -> std::process::ExitStatus {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs")
}

fn tool_use_line(tool: &str, arg_key: &str, arg_value: &str) -> String {
    // The real live-transcript shape (see the harness-core sibling test's
    // module docstring for a verbatim sample).
    let mut input = serde_json::Map::new();
    input.insert(
        arg_key.to_string(),
        serde_json::Value::String(arg_value.to_string()),
    );
    input.insert("old_string".into(), serde_json::Value::String("a".into()));
    input.insert("new_string".into(), serde_json::Value::String("b".into()));
    serde_json::json!({
        "parentUuid": "p",
        "isSidechain": false,
        "message": {
            "model": "claude-opus-5",
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_1",
                "name": tool,
                "input": serde_json::Value::Object(input)
            }]
        }
    })
    .to_string()
}

// ── the defect ──────────────────────────────────────────────────────────────

/// EXPECTED RED. **This is the ticket.**
///
/// The tree is dirty with two files; the transcript shows this session edited
/// only `mine.rs`. `peer.rs` belongs to a concurrent session sharing the tree —
/// and that session's transcript is on disk in the same projects tree, saying
/// so. That second transcript is what makes this the peer case; without it the
/// scenario is `an_unattributed_file_stays_in_the_review_set` below, which has
/// the opposite correct answer.
///
/// Two assertions, and they are non-vacuous only as a pair:
///   1. the REVIEWED set (log `files`) is exactly `["mine.rs"]` — red today,
///      where it is `["mine.rs","peer.rs"]`;
///   2. `peer.rs` is nonetheless NAMED in the human-visible reason. Alone this
///      passes trivially today (peer.rs is named *as a review target*), but
///      once (1) holds it is the thing that stops the exclusion from becoming
///      invisible. 「黙って全件を自分の変更として提示しない」cuts both ways: a
///      silent drop is as bad as a silent claim.
#[test]
fn peer_written_file_is_excluded_from_the_review_set() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("peer.rs", "fn peer() {}\n");
    let tp = f.transcript(&[f.abs("mine.rs")], &[]);
    let peer = f.peer_transcript(&[f.abs("peer.rs")], &[]);
    assert!(
        peer.exists() && peer.parent() == tp.parent(),
        "fixture precondition: a CONCURRENT session's transcript must exist \
         alongside ours, or this test silently becomes the unattributed case"
    );

    let o = f.run("s-peer", tp.to_str().unwrap());

    assert_eq!(
        o.decision, "block",
        "a real own-change must still be reviewed"
    );
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string()],
        "reviewgate must review only what THIS session edited; peer.rs was \
         written by a concurrent session sharing the working tree and must not \
         be presented as this session's change"
    );
    assert!(
        o.reason.contains("peer.rs"),
        "the excluded file must be NAMED in the reason — dropping it silently \
         is as bad as claiming it silently. reason was:\n{}",
        o.reason
    );
}

/// EXPECTED RED (anti-vacuity partner of the test above).
///
/// Fails if the implementation attributes anything the transcript merely
/// *mentions*. Reading a peer's file is exactly what a session sharing a tree
/// does, so a grep-for-`file_path` implementation would re-open the bug while
/// looking fixed.
///
/// The peer transcript is what makes this observable at all. `mine` beats
/// `peers` on an overlap, so if a `Read` wrongly counted as an edit, `peer.rs`
/// would land in `mine`, out-rank the peer's real claim, and come back into the
/// review set. With no peer on disk the file would stay in for the *legitimate*
/// reason (unattributed) and the test would pass while observing nothing.
#[test]
fn a_file_this_session_only_read_is_not_attributed_to_it() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("peer.rs", "fn peer() {}\n");
    // The session Read peer.rs (a normal thing to do) but Edited only mine.rs.
    let tp = f.transcript(&[f.abs("mine.rs")], &[f.abs("peer.rs")]);
    // ...and the session that actually WROTE peer.rs is on disk saying so.
    f.peer_transcript(&[f.abs("peer.rs")], &[]);

    let o = f.run("s-readonly", tp.to_str().unwrap());

    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string()],
        "a Read is not an edit: peer.rs appears in the transcript but this \
         session did not write it"
    );
}

/// PASSING CONTROL — the same rule pointed at the PEER's transcript.
///
/// A peer that merely `Read` a file has not claimed it, so the file is
/// unattributed and stays in. An implementation that treated any `file_path` in
/// a peer transcript as a claim would drop files nobody wrote — and it would
/// drop the most-read files first, which are the ones most likely to be edited
/// by the shell.
#[test]
fn a_file_a_peer_only_read_is_not_excluded() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("nobodys.rs", "fn nobodys() {}\n");
    let tp = f.transcript(&[f.abs("mine.rs")], &[]);
    f.peer_transcript(&[], &[f.abs("nobodys.rs")]);

    let o = f.run("s-peer-read", tp.to_str().unwrap());

    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string(), "nobodys.rs".to_string()],
        "a peer's Read is not a peer's edit; nobodys.rs is unattributed and \
         unattributed stays in the review set"
    );
}

/// PASSING CONTROL — **the property the intersection rule got wrong.**
///
/// Same dirty tree as `peer_written_file_is_excluded_from_the_review_set`, with
/// the one difference that decides the answer: no other session's transcript
/// claims `unowned.rs`. Nobody's footprint mentions it, which is precisely what
/// this session's own `sed -i` edits look like (module docstring, measured), so
/// it must be reviewed. Excluding it is the fail-open the intersection rule
/// shipped: had every changed file looked like this, `reviewable_files` would
/// come back empty and `evaluate` would `allow("no-reviewable-changes")`.
///
/// Asserting the block as well as the file list is what distinguishes "reviewed
/// both" from "the gate went dark and reviewed neither".
#[test]
fn an_unattributed_file_stays_in_the_review_set() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("unowned.rs", "fn unowned() {}\n");
    let tp = f.transcript(&[f.abs("mine.rs")], &[]);
    // Deliberately NO peer transcript: an exclusion needs positive evidence.
    assert!(
        std::fs::read_dir(f.slug_dir())
            .expect("slug dir")
            .filter_map(|e| e.ok())
            .all(|e| e.path() == tp),
        "fixture precondition: ours must be the ONLY transcript on disk"
    );

    let o = f.run("s-unattributed", tp.to_str().unwrap());

    assert_eq!(
        o.decision, "block",
        "an unattributed change is still a change to review; an allow here \
         means the gate went dark on a file this session may well have written \
         through the shell"
    );
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string(), "unowned.rs".to_string()],
        "absence from every footprint is UNKNOWN, not `not mine` — measured: a \
         `sed -i` edit this session really made appears in no footprint at all"
    );
}

/// EXPECTED RED.
///
/// The `Undetermined` branch must be *visible*. If reviewgate silently reviews
/// everything when it could not read the transcript, an operator staring at a
/// phantom block has no way to tell "your peer's file leaked in again" from
/// "attribution is off because the transcript is unreadable" — and the cheapest
/// escape from either is the shared `.reviewgate-skip` file.
///
/// The assertion accepts any of the repo's usual words for 判定不能 rather than
/// pinning a sentence, so the implementer keeps control of the prose.
#[test]
fn undetermined_attribution_is_disclosed_in_the_reason() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("peer.rs", "fn peer() {}\n");
    let missing = f.home.join("no-such-transcript.jsonl");
    assert!(!missing.exists(), "fixture precondition");

    let o = f.run("s-undet-reason", missing.to_str().unwrap());

    assert_eq!(o.decision, "block");
    let lower = o.reason.to_lowercase();
    assert!(
        lower.contains("undetermined")
            || o.reason.contains("判定不能")
            || o.reason.contains("特定できません"),
        "when attribution could not be determined the reason must SAY so \
         (one of: \"undetermined\" / 「判定不能」/「特定できません」), otherwise the \
         reader cannot distinguish an un-narrowed review from a narrowed one. \
         reason was:\n{}",
        o.reason
    );
}

// ── controls that must KEEP passing ─────────────────────────────────────────

/// PASSING CONTROL — the anti-vacuity guard against "narrow to ∅ always".
///
/// Everything dirty in the tree WAS edited by this session, so nothing may be
/// excluded. An implementation that answers `Known(∅)` on any hiccup, or whose
/// abs-vs-relative comparison never matches, turns reviewgate off entirely and
/// fails right here.
#[test]
fn everything_this_session_edited_is_still_reviewed() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("other.rs", "fn other() {}\n");
    let tp = f.transcript(&[f.abs("mine.rs"), f.abs("other.rs")], &[]);

    let o = f.run("s-all-mine", tp.to_str().unwrap());

    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string(), "other.rs".to_string()],
        "no file may be excluded when the session edited all of them"
    );
}

/// PASSING CONTROL — the abs/rel normalisation guard, in its sharpest form.
///
/// One dirty file, edited by this session, recorded in the transcript as an
/// ABSOLUTE path while `changed_files` reports it RELATIVE to the repo root. If
/// the intersection compares the two spellings naively it yields ∅, the review
/// set is empty, and `evaluate` returns `allow("no-reviewable-changes")` — the
/// gate goes *dark* rather than red, which is the failure mode that survives a
/// careless test. Asserting the block (not just the file list) is what catches
/// it.
#[test]
fn own_edit_is_still_reviewed_across_the_abs_rel_boundary() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    let tp = f.transcript(&[f.abs("mine.rs")], &[]);
    let recorded = std::fs::read_to_string(&tp).expect("read transcript");
    assert!(
        recorded.contains(&f.abs("mine.rs")) && f.abs("mine.rs").starts_with('/'),
        "fixture precondition: the transcript records an ABSOLUTE path"
    );

    let o = f.run("s-absrel", tp.to_str().unwrap());

    assert_eq!(
        o.decision, "block",
        "the session's own change must still be reviewed; an allow here means \
         the intersection produced ∅ because absolute and relative paths were \
         compared without normalising"
    );
    assert_eq!(o.log_files, vec!["mine.rs".to_string()]);
    assert_eq!(o.log_verdict, "blocked-inject");
}

/// PASSING CONTROL — 判定不能 must not narrow (CLAUDE.md 第3節).
///
/// The transcript path points at a file that does not exist. We cannot tell
/// whose changes these are, so we review them all, exactly as today. This is
/// the assertion that stops the fix from being "when in doubt, review nothing".
#[test]
fn unreadable_transcript_does_not_narrow_the_review_set() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("peer.rs", "fn peer() {}\n");
    let missing = f.home.join("no-such-transcript.jsonl");

    let o = f.run("s-undet", missing.to_str().unwrap());

    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string(), "peer.rs".to_string()],
        "an unreadable transcript is 判定不能, not 'none of these are mine' — \
         the full changed set must still be reviewed"
    );
}

/// PASSING CONTROL — same rule for the payload that carries no transcript at
/// all. `HookInput::transcript_path` is `#[serde(default)]`
/// (`harness-core/src/hook.rs:42`), so `""` is the routine shape for a manual
/// run or an older Claude Code, and must not disable the gate.
#[test]
fn empty_transcript_path_does_not_narrow_the_review_set() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    f.dirty("mine.rs", "fn mine() {}\n");
    f.dirty("peer.rs", "fn peer() {}\n");

    let o = f.run("s-empty-tp", "");

    assert_eq!(o.decision, "block");
    assert_eq!(
        o.log_files,
        vec!["mine.rs".to_string(), "peer.rs".to_string()],
        "no transcript_path means we could not look; review everything"
    );
}

// ── regression controls for the untouched ChangeScan arms ───────────────────

/// PASSING CONTROL (regression). `ChangeScan::Failed` must stay a BLOCK.
///
/// A `.git` that exists while git refuses to talk about it is
/// `RepoProbe::Undetermined` → `ChangeScan::Failed` (`src/git.rs`). The new
/// narrowing code sits downstream of that arm and must not reach it: an
/// undetermined change set intersected with anything is still undetermined, and
/// quietly turning this into an allow would be a strictly worse fail-open than
/// the bug being fixed.
#[test]
fn failed_git_scan_still_blocks_and_reviews_nothing() {
    if !git_available() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::new();
    let broken = f.home.join("broken");
    std::fs::create_dir_all(&broken).expect("mkdir");
    // A `.git` FILE pointing at a gitdir that does not exist: git exits non-zero
    // while the filesystem still shows repository evidence.
    std::fs::write(broken.join(".git"), "gitdir: /nonexistent/xyzzy\n").expect("write .git");
    std::fs::write(broken.join("a.rs"), "fn a() {}\n").expect("write a.rs");

    let payload = serde_json::json!({
        "session_id": "s-failed",
        "transcript_path": "",
        "cwd": broken.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let o = f.run_in(&broken, "s-failed", &payload);

    assert_eq!(
        o.decision, "block",
        "an undetermined change set must never become an allow"
    );
    assert_eq!(o.log_verdict, "git-scan-failed");
    assert!(
        o.log_files.is_empty(),
        "nothing was reviewed: there is no trustworthy change set to review"
    );
}

/// PASSING CONTROL (regression). `ChangeScan::NotRepo` must stay an ALLOW.
///
/// No git scope means nothing to review. The narrowing must not accidentally
/// convert "out of scope" into a block (which would trap turns in every
/// non-git project).
#[test]
fn non_repo_still_allows() {
    let f = Fixture::new();
    let plain = f.home.join("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");
    std::fs::write(plain.join("a.rs"), "fn a() {}\n").expect("write a.rs");

    let payload = serde_json::json!({
        "session_id": "s-notrepo",
        "transcript_path": "",
        "cwd": plain.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let o = f.run_in(&plain, "s-notrepo", &payload);

    assert_eq!(o.decision, "allow", "a non-git dir has nothing to review");
    assert_eq!(o.log_verdict, "no-git");
}
