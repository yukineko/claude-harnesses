//! Contract tests for the three properties that decide **whose** footprint may
//! silence a gate, and **how much of the transcript store** answering that
//! question is allowed to cost.
//!
//! `crates/harness-core/src/attribution.rs` states the rule these tests defend:
//! a changed file leaves a gate's working set ONLY when a peer session's
//! transcript positively claims it and this session's does not. Every property
//! below is a way that rule can be broken in the *permissive* direction —
//! excluding a file from the audit that nobody had the standing to exclude.
//!
//! A missed exclusion costs a session one extra file to look at. A FALSE
//! exclusion drops a real change out of a gate that blocks on hard-coded
//! secrets and missing tests, and does it silently. The two errors are not
//! symmetric, and every assertion message below is written from that asymmetry.
//!
//! Written by an agent that did not write the implementation (CLAUDE.md
//! 第2節-(a)), and every test here was observed RED against a deliberately
//! broken implementation before being accepted green (第2節-(b)).
//!
//! # What this file does NOT pin — stated because a gap nobody is told about
//! is the same defect as a gate nobody is told about
//!
//! `peer_edit_footprint_within` also drops a transcript whose **mtime cannot be
//! read**. That arm is not pinned here: on Linux the only way to make
//! `fs::metadata` fail for a directory entry (a dangling symlink, a vanished
//! file) also makes `File::open` fail, so the transcript contributes nothing
//! whether the mtime filter runs or not. A test of it would stay green against
//! an implementation with the filter deleted — i.e. it would observe nothing.
//! See the report accompanying this file; the honest state is "unpinned",
//! not "covered".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use harness_core::attribution::{attribute_from_transcript, resolve, Attribution};
use harness_core::transcript::peer_edit_footprint_within;

// ── fixture helpers ─────────────────────────────────────────────────────────

/// A private temp directory that removes itself on drop. Named with the pid, a
/// per-test tag and a process-wide counter so that two threads of this binary
/// and two concurrent `cargo test` processes never share a tree.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "hc-peerwin-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create the fixture root");
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One assistant turn carrying a single `Edit` `tool_use` block, in the shape
/// `files_edited_by_session` reads out of a live transcript.
fn edit_line(file_path: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Edit","input":{{"file_path":"{}","old_string":"a","new_string":"b"}}}}]}}}}"#,
        esc(file_path)
    )
}

/// Write `<projects>/<slug>/<session>.jsonl` containing one `Edit` block per
/// path in `edits`, and return the transcript's path.
fn transcript(projects: &Path, slug: &str, session: &str, edits: &[String]) -> PathBuf {
    let dir = projects.join(slug);
    std::fs::create_dir_all(&dir).expect("create the project slug directory");
    let p = dir.join(format!("{session}.jsonl"));
    let mut body = String::new();
    for e in edits {
        body.push_str(&edit_line(e));
        body.push('\n');
    }
    std::fs::write(&p, body).expect("write the transcript fixture");
    p
}

/// Create `<root>/<rel>` (with parents) and return its resolved absolute path —
/// the spelling a transcript records and the one `resolve` compares against.
fn touch(root: &Path, rel: &str) -> String {
    let p = root.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("create the working-tree subdirectory");
    }
    std::fs::write(&p, b"x").expect("create the working-tree file");
    resolve(&p).to_string_lossy().into_owned()
}

/// Back-date both timestamps of `p` by `age`.
///
/// `expect`s rather than degrading: a filesystem that will not honour this is a
/// filesystem on which the window rule cannot be OBSERVED, and a test that
/// quietly stopped observing its property is the silent fail-open this whole
/// module exists to forbid (第2節).
fn age_by(p: &Path, age: Duration) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(p)
        .expect("open the transcript fixture to back-date it");
    let t = SystemTime::now()
        .checked_sub(age)
        .expect("system clock is far enough past the epoch to back-date a fixture");
    f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
        .expect("set mtime on the transcript fixture");
}

#[track_caller]
fn narrowed(a: &Attribution, what: &str) -> (Vec<String>, Vec<String>) {
    match a {
        Attribution::Narrowed { keep, excluded } => (keep.clone(), excluded.clone()),
        Attribution::Undetermined { why } => panic!(
            "{what}: got Undetermined({why}). This fixture's own transcript is present and \
             readable, so the split was refused on a footprint that WAS observed — the test can no \
             longer see which files the rule excludes, and the property below is unobserved rather \
             than satisfied."
        ),
    }
}

const HOUR: Duration = Duration::from_secs(3600);
const WINDOW: Duration = Duration::from_secs(24 * 3600);

// ── Fix 1: a peer is a CONCURRENT session, not any session that ever ran ────

/// Pins the window at the footprint layer: a transcript written now is a peer,
/// the same transcript back-dated past the window is not.
///
/// Without the bound, "peer" means "any session that has ever existed on this
/// machine", and my own finished sessions from last week keep claiming the
/// absolute paths they edited — forever.
#[test]
fn a_transcript_older_than_the_window_is_not_a_peer() {
    let tmp = Tmp::new("footprint");
    let projects = tmp.path().join("projects");
    let repo = tmp.path().join("repo");
    let fresh_file = touch(&repo, "fresh.rs");
    let stale_file = touch(&repo, "stale.rs");

    transcript(
        &projects,
        "slug-a",
        "peer-fresh",
        std::slice::from_ref(&fresh_file),
    );
    let stale = transcript(
        &projects,
        "slug-b",
        "peer-stale",
        std::slice::from_ref(&stale_file),
    );
    age_by(&stale, WINDOW + HOUR);

    let peers = peer_edit_footprint_within(&projects, "me", WINDOW);

    assert!(
        peers.contains(&fresh_file),
        "a transcript written moments ago was not counted as a peer (peers={peers:?}). If a live \
         concurrent session stops being visible, its edits arrive at my gate as MINE and block me \
         on work I did not do — the unearned block whose cheapest exit is a shared skip marker, \
         i.e. the gate switched off for everyone."
    );
    assert!(
        !peers.contains(&stale_file),
        "a transcript last written {} hours ago — outside the {}h activity window — was still \
         counted as a peer (peers={peers:?}). Concurrency is a property of TIME: with no bound, \
         every session that ever ran is a peer forever, so a file I edit today through `sed -i` \
         (invisible in my own footprint) is excluded from my gate on the strength of a footprint I \
         myself left last week. That is the exact fail-open this module exists to close, arriving \
         through the other door.",
        (WINDOW + HOUR).as_secs() / 3600,
        WINDOW.as_secs() / 3600
    );
}

/// The same property one layer up, through the real entry point and the real
/// 24h constant: the file a stale session claimed must land in `keep`, not in
/// `excluded`.
///
/// The layer matters. `peer_edit_footprint_within` could honour the window
/// perfectly while `attribute_from_transcript` calls a variant that does not,
/// and the gate — which only ever sees this function — would be permissive
/// anyway.
#[test]
fn a_file_only_a_stale_session_claims_stays_in_the_audit_set() {
    let tmp = Tmp::new("endtoend");
    let projects = tmp.path().join("projects");
    let repo = tmp.path().join("repo");
    let fresh_file = touch(&repo, "peer_fresh.rs");
    let stale_file = touch(&repo, "peer_stale.rs");

    // My own transcript is present and readable and records no Edit block —
    // exactly what a session that worked through `sed -i` looks like.
    let mine = transcript(&projects, "slug-me", "me", &[]);
    transcript(&projects, "slug-a", "peer-fresh", &[fresh_file]);
    let stale = transcript(&projects, "slug-b", "peer-stale", &[stale_file]);
    // 48h: twice the shipped PEER_ACTIVE_WINDOW, so no clock skew or slow
    // machine can put this fixture back inside the window.
    age_by(&stale, 48 * HOUR);

    let changed = vec!["peer_fresh.rs".to_string(), "peer_stale.rs".to_string()];
    let a = attribute_from_transcript(&repo, &mine.to_string_lossy(), &changed);
    let (keep, excluded) = narrowed(&a, "a readable own transcript beside two peer transcripts");

    assert_eq!(
        excluded,
        vec!["peer_fresh.rs".to_string()],
        "excluded={excluded:?}, keep={keep:?}. Only the CONCURRENT peer earns an exclusion. A \
         session whose transcript has not been touched for 48h is not sharing this checkout now, \
         so its footprint is not evidence about who wrote today's change; letting it exclude drops \
         a real edit out of a gate that blocks on secrets and missing tests, and drops it in \
         silence."
    );
    assert!(
        keep.contains(&"peer_stale.rs".to_string()),
        "keep={keep:?}. The file only a stale session claims is UNATTRIBUTED — nobody currently \
         sharing this tree was observed editing it — and unattributed stays in. Absence of \
         evidence is not evidence of a peer."
    );
}

// ── Fix 2: peer discovery must not trust an arbitrary grandparent ───────────

/// Pins both halves in one fixture: identical transcript content excludes a
/// peer's file when it really lives under `…/projects/<slug>/`, and excludes
/// NOTHING when the same two files sit two levels under some other directory.
///
/// `attribute_from_transcript` derives the projects root as
/// `transcript.parent().parent()`. Handed a transcript from anywhere else —
/// a fixture tree, a copied log, a path an attacker chose — that grandparent is
/// whatever happened to be there, and the "peer set" becomes whatever `.jsonl`
/// files happen to sit beside it. A wrong peer set does not produce noise; it
/// produces FALSE EXCLUSIONS.
#[test]
fn peers_are_only_read_from_a_real_projects_root() {
    let tmp = Tmp::new("root");
    let repo = tmp.path().join("repo");
    let their_file = touch(&repo, "theirs.rs");
    let changed = vec!["theirs.rs".to_string()];

    // (a) The real shape: <…>/.claude/projects/<slug>/<sid>.jsonl
    let real = tmp.path().join(".claude").join("projects");
    let mine_real = transcript(&real, "slug", "me", &[]);
    transcript(&real, "slug", "peer", std::slice::from_ref(&their_file));
    let a = attribute_from_transcript(&repo, &mine_real.to_string_lossy(), &changed);
    let (_, excluded_real) = narrowed(&a, "a transcript under a genuine projects root");
    assert_eq!(
        excluded_real,
        vec!["theirs.rs".to_string()],
        "excluded={excluded_real:?}. This is the control: under a genuine projects root a peer's \
         claim MUST still exclude. Without it the negative case below would pass against an \
         implementation that had simply stopped finding peers at all, and would prove nothing."
    );

    // (b) The same bytes, two levels under a directory that is not `projects`.
    let fake = tmp.path().join("somewhere");
    let mine_fake = transcript(&fake, "slug", "me", &[]);
    transcript(&fake, "slug", "peer", &[their_file]);
    let b = attribute_from_transcript(&repo, &mine_fake.to_string_lossy(), &changed);
    let (keep_fake, excluded_fake) = narrowed(&b, "a transcript whose grandparent is not projects");

    assert!(
        excluded_fake.is_empty(),
        "excluded={excluded_fake:?}. A sibling `.jsonl` under a directory that is NOT the \
         transcript store was treated as a peer session. The two error directions are not equal: \
         failing to find a real peer costs this session one extra file to review, while inventing \
         a peer silently deletes a changed file from a gate that blocks on hard-coded secrets and \
         missing tests. Peer identity must be established by the store's shape, not assumed from \
         whatever directory the caller was pointed at."
    );
    assert_eq!(
        keep_fake, changed,
        "keep={keep_fake:?}. With no trustworthy peer evidence the file is unattributed, and \
         unattributed stays in the audit set (attribution.rs: an exclusion must be a positive \
         observation, never the absence of one)."
    );
}

// ── Fix 3: the Undetermined path must not walk the transcript store ────────

/// Pins non-traversal as an OBSERVATION, not as a wall-clock threshold.
///
/// `attribute_from_transcript` used to build the peer footprint before checking
/// whether its own footprint was even readable, then discard it — a full walk of
/// a 93 MB store on every Stop hook and every `git commit`. Timing that is
/// flaky, so the fixture makes traversal *detectable* instead: the peer slug
/// holds a FIFO named `peer.jsonl` with no writer. Opening it blocks forever.
///
/// So the assertion is binary and clock-free: an implementation that does not
/// walk the store returns promptly; one that does never returns at all. The
/// generous bound below is a deadlock detector, not a performance budget —
/// failing it does not mean "too slow", it means "the walk happened".
#[test]
fn an_unreadable_own_footprint_returns_without_touching_the_peer_store() {
    let tmp = Tmp::new("earlyexit");
    let projects = tmp.path().join("projects");
    let slug = projects.join("slug");
    std::fs::create_dir_all(&slug).expect("create the project slug directory");

    let fifo = slug.join("peer.jsonl");
    let st = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo — without it this property cannot be observed at all");
    assert!(
        st.success(),
        "mkfifo exited {st}: the trap that makes a peer-store walk observable was not built, so a \
         green result below would mean nothing (第2節-(b))."
    );

    // My own transcript does not exist -> files_edited_by_session_and_subagents
    // answers Undetermined, which is the arm under test.
    let missing = slug.join("me.jsonl").to_string_lossy().into_owned();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create the working tree");
    let changed = vec!["a.rs".to_string(), "b.rs".to_string()];

    let (tx, rx) = std::sync::mpsc::channel();
    let (r2, c2) = (repo.clone(), changed.clone());
    std::thread::spawn(move || {
        let a = attribute_from_transcript(&r2, &missing, &c2);
        let _ = tx.send(match a {
            Attribution::Undetermined { why } => Ok(why),
            Attribution::Narrowed { keep, excluded } => Err(format!("{keep:?} / {excluded:?}")),
        });
    });

    let got = rx
        .recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|_| {
            panic!(
                "attribute_from_transcript never returned. It blocked inside the peer transcript \
             store, which means it walked the store even though its own footprint was \
             Undetermined and the peer set could not affect the answer. On the real store that \
             walk is 93 MB of IO on every Stop hook and every `git commit`; a gate slow enough to \
             resent is a gate someone switches off, and a switched-off gate is the fail-open this \
             whole module is built to prevent."
            )
        });
    let why = got.unwrap_or_else(|split| {
        panic!(
            "attribution narrowed ({split}) from a transcript that does not exist. A split built \
             on a footprint that was never read drops changed files from the audit on no evidence \
             at all."
        )
    });
    assert!(
        why.contains("me.jsonl"),
        "the Undetermined reason must name the transcript it failed to read, got: {why:?}. An \
         operator who cannot see WHICH observation failed cannot tell a broken hook payload from a \
         gate that is simply auditing everything."
    );

    // Same inputs, no peer store at all: the answer a caller acts on must be
    // identical, because the peer set is irrelevant on this arm by construction.
    let bare = Tmp::new("earlyexit-bare");
    let lone = bare.path().join("projects/slug/me.jsonl");
    let c = attribute_from_transcript(&repo, &lone.to_string_lossy(), &changed);
    match c {
        Attribution::Undetermined { .. } => {}
        other => panic!(
            "with no peer store present the same missing transcript produced {other:?} instead of \
             Undetermined; the arm's answer must not depend on what happens to be on disk beside \
             it."
        ),
    }
    assert_eq!(
        c.files(&changed),
        changed.as_slice(),
        "判定不能 must hand back the FULL change list. Narrowing here would turn `I could not \
         tell whose these are` into `none of these are mine` and switch the gate off in silence."
    );

    // A regression parks the worker thread on the FIFO forever. That is
    // deliberate and harmless: the thread is detached, the panic above has
    // already reported the failure, and the test process kills it on exit.
    // Nothing here tries to open the FIFO for writing — that would block too.
}
