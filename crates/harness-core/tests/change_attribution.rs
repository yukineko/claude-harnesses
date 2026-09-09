//! Contract tests for change attribution: "which of these working-tree changes
//! must this session still answer for?"
//!
//! # Why this contract exists
//!
//! Two sessions can share one git checkout. `git diff --name-only` then answers
//! for BOTH of them, so a gate that reads the working tree presents a peer's
//! edits as yours and blocks you on work you did not do. The shortest way out
//! of a block you did not earn is to write a skip marker — a shared, one-shot
//! hatch — so the gate's own pressure pushes the operator toward switching the
//! gate off. The session **transcript**
//! (`~/.claude/projects/<slug>/<session-id>.jsonl`) is the only artefact that
//! carries session identity, so it is the only thing that can answer the
//! question at all.
//!
//! # The measurement that fixes the direction of this contract
//!
//! An earlier version of this API narrowed the audit set to *my* transcript's
//! edit footprint and dropped everything else. A probe over a live 12.6 MB
//! transcript against the working tree it produced showed why that is wrong:
//!
//! ```text
//! transcript footprint: 90 path(s)
//! working tree changed: 10 path(s)
//!   IN footprint  crates/donegate/src/main.rs
//!   ...
//!   MISSING       crates/harness-core/src/lib.rs
//!   MISSING       crates/harness-core/tests/gate_giveup_concession.rs
//! ```
//!
//! Both MISSING files **were** edited by that session. `lib.rs` was changed with
//! `sed -i` — a `Bash` `tool_use`, not an `Edit`/`Write` one, so
//! `files_edited_by_session` cannot see it. `gate_giveup_concession.rs` was
//! written by a **subagent**, whose `tool_use` blocks live in the subagent's own
//! sidechain transcript, not the parent's.
//!
//! So my own footprint is a strict UNDER-approximation:
//!
//! ```text
//! f in my_footprint      ==> f IS mine        (sound)
//! f not in my_footprint  ==> UNKNOWN          (NOT "not mine")
//! ```
//!
//! Reading the second line as "not mine" is a fail-open introduced by the fix
//! itself: it hides shell-driven and subagent-authored edits from a gate that
//! blocks on hard-coded secrets and missing tests. CLAUDE.md 第3節.
//!
//! # The rule these tests pin
//!
//! **An exclusion must be a positive observation, never the absence of one.**
//! A file leaves the audit set only when some *peer* transcript is observed to
//! have edited it and mine is not. Everything else — mine, and everything
//! unattributed — is kept.
//!
//! Two anti-vacuity controls hold the contract down from both sides, and
//! neither is optional:
//!
//! * [`unattributed_change_is_kept_not_excluded`] forbids collapsing back to
//!   the intersection semantics (keep-nothing).
//! * [`peer_only_changes_are_actually_excluded`] forbids the trivial
//!   "keep everything, exclude nothing" implementation that would satisfy every
//!   other test here while making the narrowing a no-op.
//!
//! # What is NOT pinned here, and why
//!
//! `attribute_from_session` resolves `$HOME` internally. Mutating the
//! process-wide `HOME` in an in-process `#[test]` would make every other test in
//! this binary order-dependent (they run in parallel threads of one process), so
//! this file does **not** do it. The end-to-end path is pinned with
//! `attribute_from_transcript` over a `~/.claude/projects`-shaped fixture, with
//! `locate_session_transcript_in` asserted over the SAME fixture in the same
//! test — so both halves of `attribute_from_session` are observed without
//! touching the environment. `attribute_from_session` itself is still exercised
//! directly for its `$HOME`-independent undetermined arm.
//!
//! Every test builds its own temp directory keyed by `std::process::id()`, a
//! per-test tag and a monotonic counter, and removes it on drop, so parallel
//! threads and concurrent `cargo test` processes cannot collide.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use harness_core::attribution::{
    attribute_from_session, attribute_from_transcript, attribute_with_footprints, resolve,
    Attribution,
};
use harness_core::transcript::{
    files_edited_by_session, locate_session_transcript_in, peer_edit_footprint_in,
};
use harness_core::verdict::Determination;

// ── fixture helpers ─────────────────────────────────────────────────────────

/// A private temp directory that removes itself on drop.
///
/// Named with the pid, a per-test tag and a process-wide counter: two threads of
/// this test binary, and two concurrent `cargo test` processes, all get disjoint
/// trees.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "harness-core-change-attribution-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create isolated temp dir for this test");
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

/// Create `root/rel` (with parents) so canonicalisation has something real to
/// resolve on both sides of the comparison.
fn touch(root: &Path, rel: &str) -> PathBuf {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir for fixture file");
    }
    std::fs::write(&p, b"fixture\n").expect("write fixture file");
    p
}

/// The absolute, resolved form of `root/rel` — the shape a transcript records.
///
/// Goes through the crate's own `resolve` so that a symlinked temp dir (e.g.
/// `/tmp` -> `/private/tmp`) cannot make the test disagree with the
/// implementation about what "the same file" means.
fn abs(root: &Path, rel: &str) -> String {
    resolve(&root.join(rel)).to_string_lossy().into_owned()
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn set(items: &[String]) -> BTreeSet<String> {
    items.iter().cloned().collect()
}

fn sorted(items: &[String]) -> Vec<String> {
    let mut out = items.to_vec();
    out.sort();
    out
}

/// Minimal JSON string escaping — `serde_json` is a normal dependency of
/// `harness-core`, not a dev-dependency, so an integration test cannot link it.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One assistant turn carrying a single `Edit` `tool_use` block, in the live
/// transcript shape:
///
/// ```json
/// {"message":{"role":"assistant","content":[
///    {"type":"tool_use","id":"toolu_1","name":"Edit",
///     "input":{"file_path":"/abs/path/x.rs","old_string":"a","new_string":"b"}}]}}
/// ```
fn edit_line(file_path: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Edit","input":{{"file_path":"{}","old_string":"a","new_string":"b"}}}}]}}}}"#,
        esc(file_path)
    )
}

/// Build a `~/.claude/projects`-shaped tree and drop `lines` into
/// `<projects>/<slug>/<session>.jsonl`. Returns the transcript path.
fn transcript_under(projects: &Path, slug: &str, session: &str, lines: &[String]) -> PathBuf {
    let dir = projects.join(slug);
    std::fs::create_dir_all(&dir).expect("create project-slug dir");
    write_transcript(&dir, session, lines)
}

/// Drop `lines` into `<dir>/<session>.jsonl`. `dir` is the *slug* directory —
/// the one `attribute_from_transcript` derives peers from, and the one
/// `peer_edit_footprint_in` scans.
fn write_transcript(dir: &Path, session: &str, lines: &[String]) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create slug dir");
    let p = dir.join(format!("{session}.jsonl"));
    let mut body = String::new();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&p, body).expect("write transcript fixture");
    p
}

/// Set both timestamps of `p` to `secs` after the epoch.
///
/// Deliberately `expect`s rather than degrading to a weaker assertion: if the
/// filesystem cannot honour the request, that is "the ordering rule could not be
/// observed here", and a test that quietly stopped observing it would be exactly
/// the silent fail-open this file is about.
fn set_mtime(p: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(p)
        .expect("open transcript fixture to set its mtime");
    let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
        .expect("set mtime on transcript fixture");
}

#[track_caller]
fn expect_narrowed(a: &Attribution, what: &str) -> (Vec<String>, Vec<String>) {
    match a {
        Attribution::Narrowed { keep, excluded } => (keep.clone(), excluded.clone()),
        Attribution::Undetermined { why } => panic!(
            "{what}: attribution came back Undetermined({why}), so the gate falls back to auditing \
             every change in a shared checkout — including a peer session's — and blocks this \
             session on work it did not do. That block is unearned, and the cheapest way out of it \
             is a shared skip marker, i.e. the gate turned off for everyone."
        ),
    }
}

#[track_caller]
fn expect_undetermined(a: &Attribution, what: &str) {
    match a {
        Attribution::Undetermined { .. } => {}
        Attribution::Narrowed { keep, excluded } => panic!(
            "{what}: attribution narrowed (keep={keep:?}, excluded={excluded:?}) instead of \
             answering Undetermined. Reporting a split built on an observation that never happened \
             lets the gate drop files from the audit on no evidence at all."
        ),
    }
}

// ── A. locate_session_transcript_in ─────────────────────────────────────────

/// Pins: a session id resolves to the exact transcript file under any project
/// slug.
///
/// This is the whole bridge from "who am I" (a session id) to "what did I edit"
/// (a transcript). If it returns anything other than the real path, every
/// attribution downstream is answering about the wrong session.
#[test]
fn locate_finds_the_transcript_for_a_known_session_under_any_slug() {
    let tmp = Tmp::new("locate-known");
    let projects = tmp.path().join("projects");
    let session = "11111111-2222-3333-4444-555555555555";
    let want = transcript_under(&projects, "-home-someone-src-proj", session, &[]);

    match locate_session_transcript_in(&projects, session) {
        Determination::Known(got) => assert_eq!(
            got, want,
            "the session id resolved to the wrong transcript file; attribution would then be \
             computed from a DIFFERENT session's edit footprint, so that session's edits count as \
             mine and my own changes are treated as a peer's and dropped from the audit"
        ),
        Determination::Undetermined(why) => panic!(
            "a transcript that exists at {want:?} was reported Undetermined({why}); attribution \
             then cannot run at all and every shared-checkout gate falls back to blocking on a \
             peer's changes"
        ),
    }
}

/// Pins: an unknown session id is `Undetermined`, never a guessed path.
///
/// Returning a constructed-but-nonexistent path would be worse than useless: the
/// caller reads "found it", fails to open it, and the failure surfaces far from
/// its cause — or, if the guess happens to name a real file, a stranger's edits
/// are treated as this session's own.
#[test]
fn unknown_session_id_is_undetermined_not_a_guessed_path() {
    let tmp = Tmp::new("locate-unknown");
    let projects = tmp.path().join("projects");
    transcript_under(&projects, "-home-someone-src-proj", "aaaaaaaa-0000", &[]);

    match locate_session_transcript_in(&projects, "bbbbbbbb-9999") {
        Determination::Undetermined(_) => {}
        Determination::Known(p) => panic!(
            "a session id with no transcript on disk resolved to {p:?}. A guessed path is not an \
             observation: downstream this reads as \"the transcript was found\", so a failure to \
             locate the session is laundered into a positive answer about which files are mine"
        ),
    }
}

/// Pins: an absent or unreadable `projects_dir` is `Undetermined`.
///
/// "The directory isn't there" is precisely a case of *cannot determine*. If it
/// collapsed to a known-empty answer, the caller would conclude this session
/// edited nothing — and since my footprint is only ever used to KEEP files, an
/// empty one silently removes my protection against a peer's exclusions.
#[test]
fn absent_or_unreadable_projects_dir_is_undetermined() {
    let tmp = Tmp::new("locate-noroot");
    let session = "11111111-2222";

    let missing = tmp.path().join("no-such-projects-dir");
    match locate_session_transcript_in(&missing, session) {
        Determination::Undetermined(_) => {}
        Determination::Known(p) => panic!(
            "a projects dir that does not exist ({missing:?}) produced Known({p:?}); an IO failure \
             was reported as a successful lookup, so \"I could not look\" becomes \"I looked and \
             this is the answer\""
        ),
    }

    // A regular file where a directory is expected: readable bytes, but not
    // enumerable as a directory — the portable stand-in for "unreadable".
    let not_a_dir = tmp.path().join("projects-is-a-file");
    std::fs::write(&not_a_dir, b"not a directory\n").expect("write non-directory fixture");
    match locate_session_transcript_in(&not_a_dir, session) {
        Determination::Undetermined(_) => {}
        Determination::Known(p) => panic!(
            "a non-enumerable projects path ({not_a_dir:?}) produced Known({p:?}); the lookup could \
             not run yet reported a conclusion, which downstream is indistinguishable from a real \
             observation of this session's edits"
        ),
    }
}

/// Pins: an empty or whitespace-only session id is `Undetermined`.
///
/// This is the ordinary case, not an exotic one: a plain terminal `git commit`
/// has no session id at all, so the gate reads an empty string. An empty id must
/// not glob-match some arbitrary `*.jsonl` and hand back a stranger's edit
/// footprint as this session's.
#[test]
fn empty_or_whitespace_session_id_is_undetermined() {
    let tmp = Tmp::new("locate-blankid");
    let projects = tmp.path().join("projects");
    transcript_under(&projects, "-home-someone-src-proj", "aaaaaaaa-0000", &[]);

    for id in ["", " ", "\t", "   \n "] {
        match locate_session_transcript_in(&projects, id) {
            Determination::Undetermined(_) => {}
            Determination::Known(p) => panic!(
                "a blank session id ({id:?}) resolved to {p:?}. \"I have no session identity\" was \
                 answered with some other session's transcript, so that session's edits would be \
                 credited to this process"
            ),
        }
    }
}

/// Pins: the same session id under two project slugs still resolves to a real
/// transcript, and the newest mtime wins.
///
/// Both halves are asserted. The mtime rule is pinned outright rather than
/// weakened, because `std::fs::File::set_times` makes it deterministic here (no
/// sleeping, no wall-clock race); the weaker existence/filename property is
/// asserted too, since a rule that picked the newest *nonexistent* path would
/// satisfy the ordering and still be useless. A duplicate id is not hypothetical
/// — one session moving between worktrees of a repo produces two project slugs.
#[test]
fn duplicate_session_id_across_slugs_resolves_to_a_real_newest_transcript() {
    let tmp = Tmp::new("locate-dup");
    let projects = tmp.path().join("projects");
    let session = "dddddddd-eeee-ffff-0000-111111111111";

    let older = transcript_under(&projects, "-home-someone-src-proj", session, &[]);
    let newer = transcript_under(&projects, "-home-someone-worktrees-proj", session, &[]);
    set_mtime(&older, 1_000_000_000);
    set_mtime(&newer, 2_000_000_000);

    match locate_session_transcript_in(&projects, session) {
        Determination::Known(got) => {
            assert!(
                got.exists(),
                "the lookup returned {got:?}, which does not exist on disk; a path that cannot be \
                 opened is a guess, and the footprint read from it will be empty — which silently \
                 removes this session's evidence that its own files are its own"
            );
            assert_eq!(
                got.file_name().and_then(|n| n.to_str()),
                Some(format!("{session}.jsonl").as_str()),
                "the lookup returned {got:?}, which is not this session's transcript file; the \
                 attribution downstream would then describe some other session's edits"
            );
            assert_eq!(
                got, newer,
                "with the same session id under two project slugs the lookup must take the \
                 newest-mtime transcript ({newer:?}); taking the stale one ({older:?}) means \
                 reading a frozen edit footprint, so the files this session is editing right now \
                 carry no evidence of being mine"
            );
        }
        Determination::Undetermined(why) => panic!(
            "two real transcripts exist for this session and the lookup answered \
             Undetermined({why}); a duplicate id makes the lookup give up entirely, so no \
             attribution is possible in exactly the multi-worktree case it was built for"
        ),
    }
}

// ── B. attribute_with_footprints — the split ────────────────────────────────

/// **The regression the live measurement demands.** Pins rule 1: a changed file
/// in NO transcript's footprint — not mine, not any peer's — lands in `keep`.
///
/// `f not in my_footprint` means UNKNOWN, never "not mine". The measured
/// counter-examples are concrete: a file changed with `sed -i` appears as a
/// `Bash` `tool_use`, which `files_edited_by_session` does not read, and a file
/// written by a subagent appears only in that subagent's own sidechain
/// transcript, never the parent's. Both were genuinely this session's work and
/// both were MISSING from the footprint.
///
/// If an unattributed file were excluded, the gate that blocks on hard-coded
/// secrets and missing tests would never look at it — and the file it skipped is
/// the very one this session just wrote through a shell.
#[test]
fn unattributed_change_is_kept_not_excluded() {
    let tmp = Tmp::new("unattributed");
    let root = tmp.path();
    for rel in ["src/mine.rs", "src/sed_edited.rs", "src/subagent_wrote.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/mine.rs", "src/sed_edited.rs", "src/subagent_wrote.rs"]);

    // Only the Edit-tool file is visible in my footprint. The other two are the
    // measured blind spots: `sed -i` (a Bash tool_use) and a subagent sidechain.
    let mine: BTreeSet<String> = [abs(root, "src/mine.rs")].into_iter().collect();
    // No peer has claimed anything.
    let peers: BTreeSet<String> = BTreeSet::new();

    let a = attribute_with_footprints(root, Determination::known(mine), &peers, &changed);
    let (keep, excluded) = expect_narrowed(&a, "a footprint that misses two of my own edits");

    assert!(
        excluded.is_empty(),
        "{excluded:?} were excluded although NO transcript — mine or a peer's — was observed to \
         have edited them. Absence from my footprint means UNKNOWN, not \"not mine\": it is \
         exactly what a `sed -i` edit (a Bash tool_use, unreadable by files_edited_by_session) and \
         a subagent-authored file (its tool_use lives in the subagent's own sidechain transcript) \
         look like. Excluding them hides shell-driven and subagent-written changes from a gate \
         whose job is to block on hard-coded secrets and missing tests — a fail-open introduced by \
         the attribution fix itself"
    );
    assert_eq!(
        sorted(&keep),
        sorted(&changed),
        "every changed file must stay in the audit set: one because my transcript proves it is \
         mine, the other two because nothing proves they are anyone else's. An exclusion has to be \
         a positive observation, never the absence of one"
    );
    assert_eq!(
        a.files(&changed),
        keep.as_slice(),
        "files() must hand back `keep` for a Narrowed attribution; anything else means the audit \
         runs over a different set than the split just computed"
    );
}

/// Pins rule 3, and is the **anti-vacuity control for exclusion**: a change in a
/// PEER's footprint and not in mine is the one and only thing that leaves the
/// audit set.
///
/// Without this, "keep everything, exclude nothing" would satisfy every other
/// test in this file while making the narrowing a no-op — the shared-checkout
/// block would never lift, this session would keep being stopped on a peer's
/// work, and the operator would reach for the shared skip marker that turns the
/// gate off for everyone. This test is what makes the keep-side tests mean
/// something.
#[test]
fn peer_only_changes_are_actually_excluded() {
    let tmp = Tmp::new("peer-only");
    let root = tmp.path();
    for rel in ["src/mine.rs", "src/peer.rs", "src/unknown.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/mine.rs", "src/peer.rs", "src/unknown.rs"]);

    let mine: BTreeSet<String> = [abs(root, "src/mine.rs")].into_iter().collect();
    let peers: BTreeSet<String> = [
        abs(root, "src/peer.rs"),
        // A peer file that git does not report as changed: it must not invent an
        // exclusion for a path that is not in `changed`.
        abs(root, "src/peer-not-in-the-diff.rs"),
    ]
    .into_iter()
    .collect();

    let a = attribute_with_footprints(root, Determination::known(mine), &peers, &changed);
    let (keep, excluded) = expect_narrowed(&a, "a peer transcript claiming one of the changes");

    assert_eq!(
        sorted(&excluded),
        v(&["src/peer.rs"]),
        "exactly the change a peer transcript was OBSERVED to have edited (and mine was not) must \
         be excluded. Excluding less means this session stays blocked on a peer's work, which is \
         the pressure that produces shared skip markers; excluding more means a file leaves the \
         audit on no evidence"
    );
    assert_eq!(
        sorted(&keep),
        v(&["src/mine.rs", "src/unknown.rs"]),
        "my own change and the unattributed one must both stay: the first has positive evidence it \
         is mine, the second has no evidence it is anyone else's"
    );
    assert_eq!(
        keep.len() + excluded.len(),
        changed.len(),
        "the split must be total: keep={keep:?} excluded={excluded:?} against changed={changed:?}. \
         A path in neither list disappears from the audit AND from the exclusion notice, so nobody \
         ever learns it was skipped"
    );
    let union: BTreeSet<String> = set(&keep).union(&set(&excluded)).cloned().collect();
    assert_eq!(
        union,
        set(&changed),
        "keep and excluded together must be exactly the input change list; a path appearing in \
         neither, or one appearing in the output but not the input, means the audit is running \
         over a different set of files than git reported"
    );
    assert_eq!(
        a.files(&changed),
        keep.as_slice(),
        "files() must hand back `keep` for a Narrowed attribution"
    );
}

/// Pins rule 2: a change in MY footprint stays in `keep` even when a peer
/// touched it too. My evidence wins; an overlap is not a peer's file.
///
/// Sharing a checkout means two sessions can genuinely edit the same file. If
/// the peer's claim overrode mine, my own edit would leave the audit set — the
/// gate would stop looking at a file I am actively writing, which is the exact
/// hole a secrets check must not have.
#[test]
fn overlapping_change_stays_mine() {
    let tmp = Tmp::new("overlap");
    let root = tmp.path();
    for rel in ["src/shared.rs", "src/peer.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/shared.rs", "src/peer.rs"]);

    let mine: BTreeSet<String> = [abs(root, "src/shared.rs")].into_iter().collect();
    let peers: BTreeSet<String> = [abs(root, "src/shared.rs"), abs(root, "src/peer.rs")]
        .into_iter()
        .collect();

    let a = attribute_with_footprints(root, Determination::known(mine), &peers, &changed);
    let (keep, excluded) = expect_narrowed(&a, "a file both sessions edited");

    assert!(
        keep.contains(&"src/shared.rs".to_string()),
        "a file MY transcript records me editing was dropped from the audit because a peer edited \
         it too (keep={keep:?}, excluded={excluded:?}). A peer's claim cannot override my own \
         positive evidence: the gate would stop inspecting a file this session is actively writing"
    );
    assert_eq!(
        sorted(&excluded),
        v(&["src/peer.rs"]),
        "only the change with a peer's evidence and none of mine may be excluded; excluding the \
         overlap as well removes my own work from the audit"
    );
}

/// Pins rule 5 (the `Undetermined` arm) — **the most load-bearing test on the
/// keep side of the split**: if MY footprint could not be read at all, the
/// answer is `Undetermined` and `.files(full)` is the FULL change list.
///
/// With no footprint of my own there is no evidence that any file is mine, so
/// applying peer exclusions would silently drop my work from the audit on the
/// strength of an observation that never happened. Returning the full list is
/// the fail-closed answer — noisier, and correct.
#[test]
fn undetermined_own_footprint_does_not_narrow_and_files_returns_the_full_list() {
    let tmp = Tmp::new("undet-mine");
    let root = tmp.path();
    for rel in ["src/a.rs", "src/b.rs", "src/c.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/a.rs", "src/b.rs", "src/c.rs"]);

    // A peer positively claims two of the three. It must not matter: without my
    // own footprint I cannot know those are not also mine.
    let peers: BTreeSet<String> = [abs(root, "src/a.rs"), abs(root, "src/b.rs")]
        .into_iter()
        .collect();
    let mine: Determination<BTreeSet<String>> =
        Determination::undetermined("transcript unreadable in this test");

    let a = attribute_with_footprints(root, mine, &peers, &changed);

    expect_undetermined(&a, "my own transcript could not be read");
    assert_eq!(
        a.files(&changed),
        changed.as_slice(),
        "when my own footprint is undetermined the audit set must stay the full change list, even \
         though a peer claimed two of these files. Applying those exclusions would drop files from \
         the audit while holding no evidence at all about whether they are also mine — the gate \
         then reports clean over an unexamined tree"
    );
}

/// Pins rule 5's other half: a `Known` but EMPTY own footprint is a real
/// observation and is NOT `Undetermined` — and with no peer claims, everything
/// is still KEPT.
///
/// This is the test that most directly encodes the inversion the measurement
/// forced. Under the old intersection semantics an empty own footprint excluded
/// *every* changed file. Under the corrected rule it excludes nothing, because
/// "I saw no Edit tool_use of my own" is not evidence that somebody else made
/// the change — it is exactly what a session that worked entirely through
/// `sed -i` or subagents looks like.
#[test]
fn known_empty_own_footprint_keeps_everything_and_is_not_undetermined() {
    let tmp = Tmp::new("known-empty");
    let root = tmp.path();
    for rel in ["src/a.rs", "src/b.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/a.rs", "src/b.rs"]);

    let no_peers: BTreeSet<String> = BTreeSet::new();
    let a = attribute_with_footprints(
        root,
        Determination::known(BTreeSet::new()),
        &no_peers,
        &changed,
    );
    let (keep, excluded) = expect_narrowed(
        &a,
        "a transcript that was read successfully and records no Edit/Write tool_use blocks",
    );

    assert!(
        excluded.is_empty(),
        "{excluded:?} were excluded because MY footprint was empty. An empty own footprint is not \
         evidence that a peer made these changes — it is what a session that edited only through \
         `sed -i` (Bash tool_use) or through subagents (sidechain transcripts) looks like. This is \
         the intersection semantics the live measurement disproved: it would hide those very edits \
         from the secrets and missing-test gates"
    );
    assert_eq!(
        sorted(&keep),
        sorted(&changed),
        "with no peer evidence at all, every changed file must remain in the audit set"
    );
    assert_eq!(
        a.files(&changed),
        changed.as_slice(),
        "with nothing excluded the audit set must equal the change list; any difference is the \
         gate quietly auditing less than git reported"
    );
}

// ── C. peer_edit_footprint_in ───────────────────────────────────────────────

/// Pins the peer scan: it collects OTHER sessions' edits and NEVER includes my
/// own session's transcript.
///
/// The self-exclusion is load-bearing in one direction only, and it is the
/// dangerous one: if my own `<session>.jsonl` were scanned as a peer, every file
/// I edited would carry "peer evidence", and — for any file I edited through a
/// shell or a subagent, so absent from my *own* parsed footprint — it would be
/// excluded from the audit as somebody else's work.
///
/// # Measured directory level (a spec/implementation discrepancy, reported)
///
/// The written spec for this helper says it scans `<projects_dir>/*.jsonl`. The
/// landed implementation scans one level deeper — `<projects_dir>/<slug>/*.jsonl`
/// — which is the layout `~/.claude/projects` actually has, and which finds
/// peers across ALL project slugs (a peer session in another worktree of the
/// same repo gets a different slug, so this is the more useful level). Measured
/// directly: the same fixture returns the two peer paths when the projects dir
/// is passed, and `{}` when the slug dir is passed.
///
/// This test therefore pins the level the rest of the system actually uses. The
/// discrepancy is NOT asserted as correct anywhere in this file: a caller who
/// follows the written spec and passes a slug dir gets a silently empty peer
/// set, i.e. exclusion switched off with no error. That is the restrictive
/// direction, so it is not a fail-open — but it is a contradiction between the
/// prose and the code, and it is reported rather than encoded here.
#[test]
fn peer_scan_collects_other_sessions_and_never_my_own_session() {
    let tmp = Tmp::new("peer-scan");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("create repo root");
    touch(&root, "src/mine.rs");
    touch(&root, "src/peer_one.rs");
    touch(&root, "src/peer_two.rs");

    let projects = tmp.path().join("projects");
    let me = "aaaaaaaa-1111-2222-3333-444444444444";
    transcript_under(
        &projects,
        "-home-someone-src-repo",
        me,
        &[edit_line(&abs(&root, "src/mine.rs"))],
    );
    // A peer in my own slug, and a peer in a different slug — the second is the
    // same repo checked out as another worktree, which is exactly the
    // shared-checkout case this whole mechanism exists for.
    transcript_under(
        &projects,
        "-home-someone-src-repo",
        "bbbbbbbb-1111",
        &[edit_line(&abs(&root, "src/peer_one.rs"))],
    );
    transcript_under(
        &projects,
        "-home-someone-worktrees-repo",
        "cccccccc-2222",
        &[edit_line(&abs(&root, "src/peer_two.rs"))],
    );

    let peers = peer_edit_footprint_in(&projects, me);

    assert!(
        peers.contains(&abs(&root, "src/peer_one.rs"))
            && peers.contains(&abs(&root, "src/peer_two.rs")),
        "the peer scan returned {peers:?} and is missing another session's edits. Every peer edit \
         it fails to see is an exclusion that never happens, so this session keeps being blocked \
         on a peer's work — the pressure that produces shared skip markers"
    );
    assert!(
        !peers.contains(&abs(&root, "src/mine.rs")),
        "the peer scan returned {peers:?}, which includes a file from MY OWN transcript \
         ({me}.jsonl). Treating my own session as a peer means every file I edited carries peer \
         evidence, so any of my edits that my own parsed footprint cannot see — `sed -i` through \
         Bash, or a subagent's sidechain — gets excluded from the audit as somebody else's work"
    );
}

/// Pins rule 4 at the scan level: any failure to read the peer directory yields
/// an EMPTY set.
///
/// Note for the next reader: this returns a plain `BTreeSet`, not a
/// `Determination`, **on purpose** — do not "fix" it into one. Peer evidence is
/// only ever used to REMOVE files from the audit set, so an empty peer footprint
/// means zero exclusions, i.e. the audit stays as large as possible. Empty is
/// the restrictive direction here, which is the opposite of
/// `files_edited_by_session`, where an empty own footprint would remove this
/// session's own evidence and so must stay distinguishable from "could not read".
///
/// These two assertions are not vacuous: the same call on a populated tree
/// returns a non-empty set in
/// [`peer_scan_collects_other_sessions_and_never_my_own_session`].
#[test]
fn unreadable_projects_directory_yields_an_empty_peer_footprint() {
    let tmp = Tmp::new("peer-unreadable");

    let missing = tmp.path().join("no-such-projects-dir");
    assert!(
        peer_edit_footprint_in(&missing, "aaaa-1111").is_empty(),
        "a projects dir that does not exist ({missing:?}) produced peer evidence out of nowhere; \
         an invented exclusion drops a file from the audit with no transcript behind it"
    );

    let not_a_dir = tmp.path().join("projects-is-a-file");
    std::fs::write(&not_a_dir, b"not a directory\n").expect("write non-directory fixture");
    assert!(
        peer_edit_footprint_in(&not_a_dir, "aaaa-1111").is_empty(),
        "a non-enumerable projects path ({not_a_dir:?}) produced peer evidence; a scan that could \
         not run must not manufacture exclusions"
    );
}

/// Pins rule 4 where it actually bites: an unreadable or unparseable peer
/// transcript costs exclusion power, and that loss must land on the KEEP side.
///
/// A garbage peer is skipped silently, so the changed file it might have claimed
/// stays in the audit. That is the correct direction — the gate looks at more,
/// not less. The failure this forbids is the mirror image: a parse error that
/// somehow produces an exclusion, dropping a file from the audit on unreadable
/// evidence.
///
/// A **readable** peer sits in the same tree as a positive control. Without it
/// every assertion below would also hold for a scan that reads nothing at all,
/// and this test would prove only that an empty answer is empty.
#[test]
fn unparseable_peer_transcript_keeps_files_never_excludes_them() {
    let tmp = Tmp::new("peer-garbage");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("create repo root");
    touch(&root, "src/only_garbage_peers_nearby.rs");
    touch(&root, "src/peer_claimed.rs");

    let projects = tmp.path().join("projects");
    let slug = "-home-someone-src-repo";
    let me = "aaaaaaaa-1111";
    let my_transcript = transcript_under(&projects, slug, me, &[]);

    // A peer that is not JSONL at all, and one that is a directory wearing a
    // `.jsonl` name.
    std::fs::write(
        projects.join(slug).join("bbbbbbbb-2222.jsonl"),
        b"{not json at all\n\x00\xff binary garbage\n",
    )
    .expect("write garbage peer transcript");
    std::fs::create_dir_all(projects.join(slug).join("cccccccc-3333.jsonl"))
        .expect("create directory masquerading as a peer transcript");
    // The positive control: one peer that reads cleanly.
    transcript_under(
        &projects,
        slug,
        "dddddddd-4444",
        &[edit_line(&abs(&root, "src/peer_claimed.rs"))],
    );

    let peers = peer_edit_footprint_in(&projects, me);
    assert_eq!(
        peers,
        set(&[abs(&root, "src/peer_claimed.rs")]),
        "the peer footprint must contain exactly what the READABLE peer claimed. Anything extra \
         is evidence manufactured from bytes that could not be parsed, and every phantom entry is \
         a file dropped from the audit for no reason; anything missing means one unparseable \
         sibling silently destroyed the exclusions the readable ones had earned"
    );

    let changed = v(&["src/only_garbage_peers_nearby.rs", "src/peer_claimed.rs"]);
    let a = attribute_from_transcript(
        &root,
        my_transcript
            .to_str()
            .expect("transcript path is valid UTF-8"),
        &changed,
    );
    let (keep, excluded) = expect_narrowed(&a, "a tree mixing readable and unreadable peers");

    assert_eq!(
        keep,
        v(&["src/only_garbage_peers_nearby.rs"]),
        "a change that only UNREADABLE peer transcripts sit near must stay in the audit set. \
         Losing exclusion power is the restrictive direction and is always acceptable; \
         manufacturing an exclusion from an unparseable file removes a change from the audit with \
         nothing behind it"
    );
    assert_eq!(
        excluded,
        v(&["src/peer_claimed.rs"]),
        "the readable peer's claim must still be honoured despite its unparseable neighbours; if \
         one corrupt sibling voided the whole scan, this session would stay blocked on every peer \
         change in the tree"
    );
}

// ── E. the substring prefilter over the raw line ────────────────────────────

/// Pins the claim the `Edit`/`Write` substring prefilter makes about itself.
///
/// `files_edited_by_session` skips any line containing neither `"Edit"` nor
/// `"Write"` before parsing it, and the comment on that skip states the
/// invariant as an absolute: *"it only ever skips lines the parse would have
/// rejected anyway — so it cannot change the answer, only the time."*
///
/// That is a claim about RAW BYTES standing in for a claim about PARSED VALUES,
/// and JSON does not let the two be equated: `Edit` is a legal encoding of
/// the string `Edit`, so `serde_json` yields the tool name `Edit` from a line
/// whose bytes contain no `Edit` at all. The prefilter is a strict superset of
/// the real condition only for unescaped input.
///
/// The first assertion is the control: the same block with a literal name is
/// collected, so any failure below is the escaping and not the fixture shape.
///
/// This is deliberately reported rather than softened. Under the corrected
/// attribution contract a missed edit is safe in BOTH directions — a miss in my
/// own footprint leaves the file unattributed, hence KEPT; a miss in a peer's
/// footprint costs an exclusion, the restrictive side — and Claude Code's writer
/// does not escape ASCII letters, so this is not expected on a live transcript.
/// What is wrong is the prose: an absolute "cannot change the answer" is
/// stronger than the code delivers, and the next reader who extends the
/// prefilter to a tool whose omission is NOT safe will rely on it.
#[test]
fn prefilter_must_not_skip_a_unicode_escaped_edit_tool_name() {
    let tmp = Tmp::new("prefilter");
    let dir = tmp.path();

    // Control: an ordinary, unescaped `Edit` block is collected.
    let plain = write_transcript(dir, "plain", &[edit_line("/repo/src/plain.rs")]);
    match files_edited_by_session(plain.to_str().unwrap()) {
        Determination::Known(got) => assert_eq!(
            got,
            set(&["/repo/src/plain.rs".to_string()]),
            "the control fixture is malformed: an unescaped Edit tool_use was not collected, so \
             nothing below can be attributed to the prefilter"
        ),
        Determination::Undetermined(why) => panic!(
            "the control transcript was unreadable ({why}); the fixture, not the prefilter, is at \
             fault"
        ),
    }

    // The tool name is spelled with a JSON escape for the leading `E`. It parses
    // to exactly "Edit", but the raw bytes contain neither "Edit" nor "Write".
    // `ESC_E` is the two characters `\` and `u` followed by `0045dit`, i.e. the
    // JSON source text Edit. Built with a Rust escape so no literal
    // backslash-u has to survive being copied around.
    let esc_e = format!("{}u0045dit", '\\');
    let escaped_line = format!(
        "{}{}{}",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":""#,
        esc_e,
        r#"","input":{"file_path":"/repo/src/escaped.rs","old_string":"a","new_string":"b"}}]}}"#
    );
    let escaped_line = escaped_line.as_str();
    assert!(
        !escaped_line.contains("Edit") && !escaped_line.contains("Write"),
        "the fixture line does not exercise the prefilter: it still contains one of the literal \
         substrings, so the skip would never fire and this test would prove nothing"
    );

    let escaped = write_transcript(dir, "escaped", &[escaped_line.to_string()]);
    match files_edited_by_session(escaped.to_str().unwrap()) {
        Determination::Known(got) => assert_eq!(
            got,
            set(&["/repo/src/escaped.rs".to_string()]),
            "a tool_use whose name parses to `Edit` (written with a \\u0045 escape) was dropped, \
             so the prefilter DID change the answer. The comment on the skip claims it \"cannot \
             change the answer, only the time\" — that absolute is false, and a reader who extends \
             the prefilter to a tool whose omission is not safe will rely on it"
        ),
        Determination::Undetermined(why) => panic!(
            "the escaped-name transcript was reported Undetermined({why}); a readable file with \
             one parseable line must yield Known"
        ),
    }
}

// ── D. End-to-end through real transcript files ─────────────────────────────

/// Pins the whole path over a real `~/.claude/projects`-shaped fixture: locate
/// my transcript from a session id, derive peers from its sibling directory, and
/// split the working tree three ways.
///
/// Both halves of `attribute_from_session` are observed here over ONE fixture —
/// `locate_session_transcript_in` (session id -> path) and
/// `attribute_from_transcript` (path -> split, peers derived from the parent
/// directory). Setting `HOME` in-process is declined on purpose: it would make
/// every parallel test in this binary order-dependent.
///
/// The three-way outcome is the entire contract in one assertion set: mine
/// kept (positive evidence), peer-only excluded (positive evidence), and
/// unattributed KEPT (no evidence either way).
#[test]
fn end_to_end_keeps_mine_and_unattributed_and_excludes_only_the_peer_file() {
    let tmp = Tmp::new("e2e");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("create repo root");
    for rel in ["src/edited.rs", "src/peer_edited.rs", "src/nobody_saw.rs"] {
        touch(&root, rel);
    }

    let projects = tmp.path().join("home/.claude/projects");
    let slug = "-home-someone-src-repo";
    let session = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
    let want = transcript_under(
        &projects,
        slug,
        session,
        &[edit_line(&abs(&root, "src/edited.rs"))],
    );
    // A peer session sharing the same checkout, living beside mine.
    write_transcript(
        &projects.join(slug),
        "99999999-0000",
        &[edit_line(&abs(&root, "src/peer_edited.rs"))],
    );

    // Half one: the session id finds exactly this fixture.
    let located = match locate_session_transcript_in(&projects, session) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => panic!(
            "the fixture transcript at {want:?} was not located from its own session id \
             (Undetermined({why})); with the lookup broken, attribution can never run and the gate \
             always falls back to auditing a peer's changes too"
        ),
    };
    assert_eq!(
        located, want,
        "the session id must resolve to its own transcript; resolving to anything else attributes \
         another session's edits to this one"
    );

    // Half two: that transcript, plus its siblings, splits the working tree.
    let changed = v(&["src/edited.rs", "src/peer_edited.rs", "src/nobody_saw.rs"]);
    let a = attribute_from_transcript(
        &root,
        located.to_str().expect("transcript path is valid UTF-8"),
        &changed,
    );
    let (keep, excluded) = expect_narrowed(&a, "a real transcript beside a real peer transcript");

    assert_eq!(
        sorted(&keep),
        v(&["src/edited.rs", "src/nobody_saw.rs"]),
        "the audit set must hold both the file my own Edit tool_use proves is mine AND the file no \
         transcript mentions at all. Dropping the first lets my own change ship unexamined; \
         dropping the second hides exactly the `sed -i` and subagent-authored edits the live \
         measurement found MISSING from a real footprint"
    );
    assert_eq!(
        sorted(&excluded),
        v(&["src/peer_edited.rs"]),
        "only the change a sibling session's transcript positively claims — and mine does not — \
         may leave the audit set. Excluding nothing here leaves this session blocked on a peer's \
         work; excluding anything else removes a file from the audit without evidence"
    );
    assert_eq!(
        a.files(&changed),
        keep.as_slice(),
        "files() must hand back `keep` for a Narrowed attribution"
    );
}

/// Pins rule 5 through the real entry point: an empty session id through
/// `attribute_from_session` is `Undetermined`, and `.files(full)` is the full
/// change list.
///
/// This is the plain-terminal `git commit` case — no session id at all — and it
/// is `$HOME`-independent, so it exercises the real entry point without touching
/// the environment. If it narrowed, a commit made outside any session could have
/// files removed from its audit on peer evidence while holding none of its own.
#[test]
fn empty_session_id_through_attribute_from_session_is_undetermined() {
    let tmp = Tmp::new("session-blank");
    let root = tmp.path();
    for rel in ["src/a.rs", "src/b.rs"] {
        touch(root, rel);
    }
    let changed = v(&["src/a.rs", "src/b.rs"]);

    let a = attribute_from_session(root, "", &changed);

    expect_undetermined(
        &a,
        "a commit with no session id (a plain terminal `git commit`)",
    );
    assert_eq!(
        a.files(&changed),
        changed.as_slice(),
        "with no session identity there is no evidence that any change is mine, so the audit must \
         cover every one of them; narrowing here would let a commit made outside any session drop \
         files it never looked at and still report clean"
    );
}
