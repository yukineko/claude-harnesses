//! Streaming transcript (JSONL) reader.
//!
//! Per project policy we NEVER load a whole transcript into memory. Everything
//! here is a single forward streaming pass with O(1) or bounded memory:
//!   * `estimate_tokens` keeps only the last seen `usage` value.
//!   * `recent_turns` keeps a bounded ring buffer of the most recent turns.

use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::verdict::Determination;

/// Live window occupancy at the turn that wrote this usage block.
#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation
    }
}

fn usage_from_value(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input: g("input_tokens"),
        cache_read: g("cache_read_input_tokens"),
        cache_creation: g("cache_creation_input_tokens"),
    }
}

/// Find the usage object inside a transcript line (either `message.usage` or a
/// top-level `usage`).
fn usage_in_line(o: &Value) -> Option<Usage> {
    if let Some(u) = o.get("message").and_then(|m| m.get("usage")) {
        if u.is_object() {
            return Some(usage_from_value(u));
        }
    }
    if let Some(u) = o.get("usage") {
        if u.is_object() {
            return Some(usage_from_value(u));
        }
    }
    None
}

/// Real context occupancy from the LAST usage block in the transcript.
///
/// The last usage reflects current state and *drops after `/compact`* (cache_read
/// collapses), which a bytes-based proxy cannot capture. Streams forward keeping
/// only the most recent value.
pub fn last_usage_tokens(path: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last: Option<u64> = None;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        // cheap pre-filter: only json-parse lines that mention usage
        if !line.contains("\"usage\"") {
            continue;
        }
        if let Ok(o) = serde_json::from_str::<Value>(&line) {
            if let Some(u) = usage_in_line(&o) {
                last = Some(u.total());
            }
        }
    }
    last
}

/// `(tokens, source)` where source is "usage" (real) or "bytes" (fallback proxy).
pub fn estimate_tokens(path: &str) -> Option<(u64, &'static str)> {
    if let Some(t) = last_usage_tokens(path) {
        return Some((t, "usage"));
    }
    let size = std::fs::metadata(path).ok()?.len();
    Some((size / 4, "bytes"))
}

/// A distilled conversational turn (role + flattened text).
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: String,
    pub text: String,
}

/// Flatten a message `content` (string OR array of blocks) into plain text,
/// keeping only human-meaningful blocks (text). Tool noise is summarized, not
/// inlined, so the extraction stays small.
fn flatten_content(content: &Value, max_chars: usize) -> String {
    let mut out = String::new();
    match content {
        Value::String(s) => out.push_str(s),
        Value::Array(blocks) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                        out.push_str(&format!("[tool_use: {name}]\n"));
                    }
                    Some("tool_result") => {
                        out.push_str("[tool_result]\n");
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let out = out.trim().to_string();
    truncate_chars(&out, max_chars)
}

/// Truncate to at most `max` chars on a char boundary, appending an ellipsis marker.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push_str(" …[truncated]");
    t
}

/// The most recent `max_turns` user/assistant turns, each capped at `max_chars`.
/// Bounded memory via a ring buffer.
///
/// Returns a [`Determination`] rather than a bare `Vec` because "this transcript
/// held no user/assistant turns" and "this transcript could not be read" are
/// different facts that an empty `Vec` cannot tell apart — and every caller
/// treats an empty result as "nothing to do" (CLAUDE.md §3: never return an
/// empty collection on error). Concretely: ctxrot's rescue hook exists to save
/// the conversation before compaction, and with the old signature an unreadable
/// transcript wrote no rescue note and told nobody, which is byte-for-byte
/// indistinguishable from "there was nothing worth saving".
///
/// **Where the line sits, deliberately.** `Undetermined` means *the bytes could
/// not be obtained*: the file would not open, or a read/decode error occurred
/// mid-stream. A line whose bytes WERE obtained but which is not a turn record
/// stays a skip — including a line that is not valid JSON, exactly like a line
/// with no `message` field or a non-turn role. That is not a softening: a live
/// transcript is appended to concurrently, so its final line is routinely a
/// torn, half-written JSON record, and calling that "undetermined" would make
/// every rescue during a live session report a failure it did not have.
///
/// The case left unresolved is a *partially* readable transcript (real turns
/// plus an unreadable region). Today the read error wins and the whole answer is
/// undetermined — the restricted side — rather than returning the turns that
/// were recovered. See backlog `809cf00f`.
pub fn recent_turns(path: &str, max_turns: usize, max_chars: usize) -> Determination<Vec<Turn>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Determination::undetermined(format!("cannot open transcript {path}: {e}"));
        }
    };
    let reader = BufReader::new(file);
    let mut ring: VecDeque<Turn> = VecDeque::with_capacity(max_turns + 1);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            // The bytes could not be read or decoded. That is not "this line
            // held no turn": we do not know what it held, nor what follows it.
            Err(e) => {
                return Determination::undetermined(format!("cannot read transcript {path}: {e}"));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // Read successfully, but not a turn record. Same category as a line with
        // no `message` field or a non-turn role — see the fn docstring for why a
        // torn final line must not become an undetermined answer.
        let o = match serde_json::from_str::<Value>(&line) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let msg = match o.get("message") {
            Some(m) => m,
            None => continue,
        };
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_else(|| o.get("type").and_then(Value::as_str).unwrap_or(""));
        if role != "user" && role != "assistant" {
            continue;
        }
        let content = match msg.get("content") {
            Some(c) => c,
            None => continue,
        };
        let text = flatten_content(content, max_chars);
        if text.is_empty() {
            continue;
        }
        ring.push_back(Turn {
            role: role.to_string(),
            text,
        });
        if ring.len() > max_turns {
            ring.pop_front();
        }
    }
    Determination::known(ring.into_iter().collect())
}

/// The tool names whose invocation means "this session wrote to a file".
///
/// `Read` / `Grep` / `Bash` are deliberately absent: reading a peer session's
/// file is *exactly* what happens when two sessions share a working tree, so an
/// implementation that merely looked for a `file_path` argument would attribute
/// the peer's file to us and re-open the bug this exists to close.
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Pull the written path out of one `tool_use` block, if it is an edit.
///
/// `NotebookEdit` carries its path under `notebook_path` rather than
/// `file_path` — the same split [`crate::hook::HookInput::target`] already makes
/// for the PostToolUse payload.
fn edited_path(block: &Value) -> Option<&str> {
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    let name = block.get("name").and_then(Value::as_str)?;
    if !EDIT_TOOLS.contains(&name) {
        return None;
    }
    let input = block.get("input")?;
    let p = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str)?;
    if p.is_empty() {
        return None;
    }
    Some(p)
}

/// The set of files THIS session edited, read from its own transcript.
///
/// `path` is the hook payload's [`crate::hook::HookInput::transcript_path`].
///
/// # Why this exists
///
/// A git working tree carries no session identity. `git diff --name-only` in a
/// shared checkout returns every concurrent session's uncommitted edits, so a
/// gate that scans the tree presents a peer's work as yours (backlog
/// `1e44bfd9`, observed 2026-07-21 session c6a1fdbf and 2026-07-23 session
/// 143f3d21). The transcript is the one artefact that *does* carry session
/// identity, so intersecting the two is what makes attribution possible at all.
///
/// # Three-valued, and the difference is load-bearing
///
/// * `Known(set)` — the transcript was read. An EMPTY set here is a real
///   observation: this session edited nothing.
/// * `Undetermined(why)` — the transcript could not be read (empty path,
///   missing file, an IO error part-way through). We did not observe "nothing
///   was edited", we observed nothing at all.
///
/// Collapsing the second into `Known(∅)` is the fail-open this signature
/// forbids: a consumer narrows its scope to this set, so `Known(∅)` means
/// "nothing is mine" — one transcript hiccup would silently disable the
/// consuming gate entirely (CLAUDE.md 第3節).
///
/// # Decisions a reader should not have to reverse-engineer
///
/// * **Paths come back verbatim**, absolute, exactly as the transcript recorded
///   them. This function has no repo root to resolve against, and guessing one
///   (`current_dir`) mis-resolves under a `git worktree` — which is the very
///   situation 第8節 pushes every session into. Normalisation belongs to the
///   caller, which knows its root.
/// * **An unparseable LINE is skipped and the answer stays `Known`**, because
///   the file itself was readable. A transcript is appended to live, so a torn
///   final line is routine; giving up on the whole file would make this dead
///   code while looking safe. An **IO error** while reading is different — that
///   is `Undetermined`, since we can no longer say we saw the whole transcript.
/// * **Sidechain (subagent) edits COUNT as this session's.** Real transcripts
///   mark subagent turns `isSidechain: true`; this function does not filter on
///   it. A subagent edits because this session dispatched it, so the edit is
///   this session's responsibility — and the tie-break is the direction of the
///   error: including them widens the caller's scope (more gets reviewed),
///   excluding them narrows it, and 第3節 resolves an undecided case to the
///   restrictive side. If this ever needs to change, it needs its own test.
///
/// Streams the file forward one line at a time, per this module's standing
/// policy (see the module docstring); only the accumulated path set is held.
/// Find a session's transcript under `projects_dir`
/// (`<projects_dir>/<project-slug>/<session_id>.jsonl`, newest match wins).
///
/// For callers that are handed a **session id** but no payload — a pre-commit
/// hook gets `CLAUDE_CODE_SESSION_ID` from the environment and nothing else,
/// while a Stop hook is given `transcript_path` directly.
///
/// Every failure is `Undetermined`, never a made-up path: "I could not find
/// this session's transcript" must not become "this session edited nothing",
/// which is what an empty answer means to
/// [`files_edited_by_session`]'s callers.
///
/// Split from [`locate_session_transcript`] so it is testable against a fixture
/// directory without mutating `$HOME`.
pub fn locate_session_transcript_in(
    projects_dir: &Path,
    session_id: &str,
) -> Determination<PathBuf> {
    if session_id.trim().is_empty() {
        return Determination::undetermined(
            "empty session id: there is no transcript to locate, so this session's edit \
             footprint was not observed",
        );
    }
    let target = format!("{}.jsonl", session_id.trim());
    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(e) => {
            return Determination::undetermined(format!(
                "projects dir {} unreadable: {e}",
                projects_dir.display()
            ))
        }
    };
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(&target);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, candidate));
            }
        }
    }
    match best {
        Some((_, p)) => Determination::Known(p),
        None => Determination::undetermined(format!(
            "no transcript for session {session_id} under {}",
            projects_dir.display()
        )),
    }
}

/// [`locate_session_transcript_in`] against the real
/// `~/.claude/projects` tree.
pub fn locate_session_transcript(session_id: &str) -> Determination<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Determination::undetermined(
            "HOME unset: cannot locate the session transcript, so the edit footprint was not \
             observed",
        );
    };
    let projects = PathBuf::from(home).join(".claude").join("projects");
    locate_session_transcript_in(&projects, session_id)
}

/// The sidechain transcripts of the subagents `transcript_path` spawned, at
/// `<dir>/<session-stem>/subagents/*.jsonl`.
///
/// Returns an empty vector when that directory does not exist or cannot be
/// read. Unlike the session transcript itself this degradation is safe **in
/// this direction only**: every use below folds these paths into the set of
/// files the session is credited with, and a missing subagent transcript can
/// therefore only ever *under*-credit — which leaves a file unattributed
/// rather than attributing it to someone else.
fn subagent_transcripts(transcript_path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (transcript_path.parent(), transcript_path.file_stem()) else {
        return Vec::new();
    };
    let sidechains = dir.join(stem).join("subagents");
    let Ok(entries) = std::fs::read_dir(&sidechains) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

/// [`files_edited_by_session`] widened to include the session's **subagents**.
///
/// A subagent's `Edit`/`Write` blocks are recorded in its own sidechain
/// transcript, not the parent's, so the parent transcript alone under-reports
/// what the session wrote. Measured 2026-09-09 against this repository's own
/// session `e9ebfcb6`: `crates/harness-core/tests/gate_giveup_concession.rs`
/// was authored by a subagent and appeared in none of the parent's 90 recorded
/// paths.
///
/// The parent transcript is authoritative — if IT cannot be read the answer is
/// `Undetermined`. A sidechain that cannot be read is skipped, because the
/// union only ever grows the set of files credited to this session, and 第3節's
/// restrictive side here is "credit fewer files to me", not "credit none".
///
/// Sidechains carry no liveness or freshness check of their own; a peer's are
/// folded in once its PARENT session holds a live claim-registry entry and its
/// transcript clears [`PEER_ACTIVE_WINDOW`]. That asymmetry is deliberate, not
/// an oversight: a live parent implies live subagents, and the converse case —
/// a dead parent with a fresh sidechain — drops a real peer, which costs an
/// exclusion rather than safety.
pub fn files_edited_by_session_and_subagents(path: &str) -> Determination<BTreeSet<String>> {
    let mut out = match files_edited_by_session(path) {
        Determination::Known(v) => v,
        undetermined => return undetermined,
    };
    for side in subagent_transcripts(Path::new(path)) {
        if let Determination::Known(v) = files_edited_by_session(&side.to_string_lossy()) {
            out.extend(v);
        }
    }
    Determination::Known(out)
}

/// The union of every **live peer** session's edit footprint under `projects_dir`.
///
/// `projects_dir` is the projects ROOT (`~/.claude/projects`), not a project
/// slug directory: transcripts live two levels down, at
/// `<projects_dir>/<project-slug>/<session-id>.jsonl`, the same shape
/// [`locate_session_transcript_in`] walks. Handed a slug directory by mistake
/// this returns an empty set rather than guessing — which costs exclusions, not
/// safety, per the paragraph below.
///
/// This is the only evidence that can justify excluding a changed file from a
/// gate's working set: a file's *absence* from this session's footprint means
/// "unattributed", not "someone else's" (see [`Attribution`] in
/// `crate::attribution`), so an exclusion has to be a positive observation
/// about a peer.
///
/// # Who counts as a peer (backlog `873a2621`, user ruling 2026-10-04, design D)
///
/// A session is a peer only if it holds a **live entry in condukt's claim
/// registry** — see [`live_claim_sessions`]. The transcript supplies the peer's
/// FOOTPRINT, never evidence that the peer is running: a transcript mtime says
/// when a file was last written, not that its session shares this tree now, and
/// the previous mtime-as-liveness rule let a session that ended hours ago
/// exclude my files. Here the registry is keyed by the process cwd;
/// [`peer_edit_footprint_for`] takes the repo root explicitly.
///
/// If the registry cannot be read or parsed, liveness is UNDETERMINED and the
/// answer is an empty footprint — **no exclusion**. That is an explicit arm in
/// [`peer_edit_footprint_for`], not a side effect of an empty set.
///
/// Returns a plain `BTreeSet`, **not** a `Determination`, and this is
/// deliberate: everything that can go wrong here — an unreadable projects dir,
/// an unreadable peer transcript, a peer whose transcript is half-written, an
/// unreadable claim registry — shrinks the returned set, and a smaller peer
/// footprint means *fewer* exclusions, which is the restrictive direction. An
/// empty set is the safe answer, so there is nothing for a third value to
/// express. Do not "fix" this into a `Determination`; doing so would invite a
/// caller to treat a read failure as a reason to exclude.
///
/// [`Attribution`]: crate::attribution::Attribution
pub fn peer_edit_footprint_in(projects_dir: &Path, my_session_id: &str) -> BTreeSet<String> {
    peer_edit_footprint_within(projects_dir, my_session_id, PEER_ACTIVE_WINDOW)
}

/// An upper bound on how recently a live peer's transcript must have been
/// written for its footprint to be read at all.
///
/// This is NOT the liveness test — the claim registry is (see
/// [`peer_edit_footprint_in`]). An earlier version used this window AS liveness,
/// which let a session that ended two hours ago exclude my files (backlog
/// `873a2621`). It stays as an additional, independent filter: both conditions
/// must hold, so it can only remove peers, never add them. Erring long is the
/// unsafe direction (a stale footprint can cause a false exclusion), so this is
/// a knob to tighten, never to widen for comfort.
///
/// Known limit: mtime is a property of the FILE, not of the session. `rsync`, a
/// restore or a plain `touch` re-dates the whole store to "now". Since the
/// registry decides liveness, that can only let a LIVE peer's older transcript
/// contribute, not resurrect a dead session as a peer.
pub(crate) const PEER_ACTIVE_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// Heartbeat TTL of condukt's claim registry, in seconds: a claim whose
/// `heartbeat_at` is more than this far in the past belongs to a holder that
/// went quiet, and is not evidence of a live session.
///
/// Mirrors condukt's DEFAULT `stuck_ttl_secs` (1800), which condukt's
/// `claim.rs::is_stale` uses. harness-core cannot depend on condukt (dependency
/// direction), so the value is restated here.
///
/// **Limitation:** the user's condukt config is NOT read. A configured
/// `stuck_ttl_secs` is ignored — a LONGER one leaves this bound tighter (fewer
/// peers, fewer exclusions: restrictive), but a SHORTER one keeps a claim live
/// here that condukt already treats as stale (more exclusions: permissive). A
/// configured `state_dir` is ignored too; the registry is looked for under the
/// default `~/.condukt/state`, so a relocated registry reads as absent (no live
/// peers, no exclusions: restrictive).
pub const CLAIM_HEARTBEAT_TTL_SECS: i64 = 1800;

/// The session ids holding a **live** claim in condukt's claim registry for the
/// repository containing `root`.
///
/// The registry is `<HOME>/.condukt/state/<key>/claims.json`, where `<key>` is
/// `project_key(main_worktree_root(root))` — the same path condukt's `claim.rs`
/// derives (keyed by the MAIN worktree root, so every linked worktree of a repo
/// shares one registry). Its format is condukt's `Registry`: file claims
/// flattened at the top level as `"<path>": Claim`, plus a `"task_claims"` map
/// of `hashkey: Claim`. Both tables carry the same `Claim` (`session_id`,
/// `heartbeat_at`, ...), and BOTH count as liveness: a `/flow` session usually
/// holds only task claims, and a fresh heartbeat on either proves the session is
/// alive.
///
/// - No registry file: `Known(empty)` — nobody holds a claim, so nobody is a
///   live peer.
/// - A claim with no `session_id` is skipped: it cannot name a session.
/// - Unresolvable root, unreadable file, unparseable JSON, a malformed entry or
///   a clock before the epoch: `Undetermined`. Callers must read that as
///   "liveness unknown" and exclude nothing.
pub fn live_claim_sessions(root: &Path) -> Determination<BTreeSet<String>> {
    let main_root = match crate::projkey::main_worktree_root(root) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let path = crate::config::base_dir("condukt")
        .join("state")
        .join(crate::projkey::project_key(&main_root))
        .join("claims.json");
    let text = match crate::boundary::read_to_string(&path) {
        Determination::Known(Some(t)) => t,
        Determination::Known(None) => return Determination::known(BTreeSet::new()),
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => {
            return Determination::undetermined(format!(
                "claim-registry: clock-before-epoch: cannot age heartbeats in {} ({e})",
                path.display()
            ))
        }
    };
    let unparseable = |what: &str| {
        Determination::undetermined(format!(
            "claim-registry: unparseable: {} ({what}); peer liveness cannot be decided",
            path.display()
        ))
    };
    let top = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => m,
        Ok(_) => return unparseable("top level is not a JSON object"),
        Err(e) => return unparseable(&format!("invalid JSON: {e}")),
    };
    let mut claims: Vec<(&str, &Value)> = Vec::new();
    for (k, v) in &top {
        if k == "task_claims" {
            let Value::Object(tasks) = v else {
                return unparseable("`task_claims` is not an object");
            };
            claims.extend(tasks.iter().map(|(k, v)| (k.as_str(), v)));
        } else {
            claims.push((k.as_str(), v));
        }
    }
    let mut live = BTreeSet::new();
    for (key, c) in claims {
        let Some(hb) = c.get("heartbeat_at").and_then(Value::as_i64) else {
            return unparseable(&format!("claim `{key}` has no integer heartbeat_at"));
        };
        let session = match c.get("session_id") {
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) if s.trim().is_empty() => continue,
            Some(Value::String(s)) => s.trim(),
            Some(_) => return unparseable(&format!("claim `{key}` has a non-string session_id")),
        };
        // condukt's `is_stale` is `now - heartbeat_at > ttl`; live is its negation.
        if now.saturating_sub(hb) <= CLAIM_HEARTBEAT_TTL_SECS {
            live.insert(session.to_string());
        }
    }
    Determination::known(live)
}

/// [`peer_edit_footprint_in`] with an explicit activity window, so the bound is
/// testable without waiting a day or back-dating the system clock. The claim
/// registry is keyed by the process cwd; an unreadable cwd excludes nothing.
pub fn peer_edit_footprint_within(
    projects_dir: &Path,
    my_session_id: &str,
    window: std::time::Duration,
) -> BTreeSet<String> {
    match std::env::current_dir() {
        Ok(cwd) => peer_edit_footprint_for(projects_dir, my_session_id, window, &cwd),
        // No cwd, no registry key: liveness undetermined, so no exclusion.
        Err(_) => BTreeSet::new(),
    }
}

/// The live-peer footprint, with the claim registry keyed by the repository
/// containing `registry_root` (a repo root or any path inside it).
pub fn peer_edit_footprint_for(
    projects_dir: &Path,
    my_session_id: &str,
    window: std::time::Duration,
    registry_root: &Path,
) -> BTreeSet<String> {
    let live = match live_claim_sessions(registry_root) {
        Determination::Known(live) => live,
        // Ruling D (backlog 873a2621): liveness cannot be decided, so NO
        // exclusion. This arm IS the decision, stated on purpose; it does not
        // rely on a later loop happening to find nothing.
        Determination::Undetermined(_) => return BTreeSet::new(),
    };
    let me = my_session_id.trim();
    let mut out = BTreeSet::new();
    if live.iter().all(|s| s == me) {
        // No live session other than (possibly) me: nobody can claim my files.
        return out;
    }
    let cutoff = std::time::SystemTime::now().checked_sub(window);
    let Ok(slugs) = std::fs::read_dir(projects_dir) else {
        return out;
    };
    for slug in slugs.flatten() {
        let Ok(entries) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Some(session) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            // Skip THIS session under every slug: the same id can appear under
            // more than one project directory, and crediting my own edits to a
            // peer would exclude my own files from my own gate.
            if session == me {
                continue;
            }
            // Liveness comes from the registry, never from the transcript.
            if !live.contains(&session) {
                continue;
            }
            // Independent upper bound (see PEER_ACTIVE_WINDOW). An unreadable
            // mtime drops the transcript rather than admitting it: dropping
            // costs an exclusion, never safety.
            let fresh = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .zip(cutoff)
                .is_some_and(|(mtime, cutoff)| mtime >= cutoff);
            if !fresh {
                continue;
            }
            if let Determination::Known(v) =
                files_edited_by_session_and_subagents(&p.to_string_lossy())
            {
                out.extend(v);
            }
        }
    }
    out
}

pub fn files_edited_by_session(path: &str) -> Determination<BTreeSet<String>> {
    if path.trim().is_empty() {
        return Determination::undetermined(
            "transcript_path is empty: the hook payload carried no transcript to read, so this \
             session's edit footprint was not observed",
        );
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot open transcript {path}: {e} — the edit footprint was not observed"
            ))
        }
    };

    let mut out = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        // An IO error mid-stream means the rest of the transcript was never
        // seen. Skipping it here would hand back a SILENTLY PARTIAL set, which
        // reads downstream as "this session did not edit those files" — the
        // same collapse the signature exists to prevent.
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                return Determination::undetermined(format!(
                    "transcript {path} became unreadable part-way through: {e} — the edit \
                     footprint is partial, not empty"
                ))
            }
        };
        // Cheap prefilter before the parse. Every tool this function collects
        // is named `Edit`, `Write`, `MultiEdit` or `NotebookEdit`, and the last
        // two contain `Edit`, so a block `edited_path` accepts must have one of
        // those two substrings in its PARSED name.
        //
        // Parsed, not raw — and the gap between the two is why the third test
        // below exists. A JSON string's value can differ from its bytes only
        // through a backslash escape, and of the escapes JSON defines only
        // `\uXXXX` can yield a letter (`\"`, `\\`, `\/`, `\b`, `\f`, `\n`,
        // `\r`, `\t` each produce a fixed non-letter). So `"Edit"` parses
        // to `Edit` from bytes containing neither substring. Admitting any line
        // that carries `\u` closes that hole and makes the skip a strict
        // superset of the real condition: it can then only ever drop lines the
        // parse would have rejected anyway.
        //
        // The `\u` arm is close to free. Measured 2026-09-09 over the live
        // 93 MB store: 65 of 37,543 lines contain `\u` (0.17%), against 1,470
        // that already match on `Edit`/`Write`. And the prefilter itself is not
        // a micro-optimisation — without it this scan cost 3.9s on every
        // `git commit` and every Stop hook (2.0s with, 0.37s in release), and a
        // gate slow enough to resent is a gate someone switches off.
        //
        // Do NOT drop the `\u` arm to buy that 0.17% back. It is the only
        // reason the sentence above is true rather than merely usually true,
        // and a later reader extending this prefilter to a tool whose omission
        // is NOT safe would be relying on it.
        if !line.contains("Edit") && !line.contains("Write") && !line.contains("\\u") {
            continue;
        }
        // A line that is not JSON is skipped; see the docstring for why this is
        // NOT the same call as the IO failure above.
        let o = match serde_json::from_str::<Value>(&line) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let content = match o.get("message").and_then(|m| m.get("content")) {
            Some(Value::Array(c)) => c,
            _ => continue,
        };
        for block in content {
            if let Some(p) = edited_path(block) {
                out.insert(p.to_string());
            }
        }
    }
    Determination::Known(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "tests/fixtures/transcript.jsonl";

    #[test]
    fn reads_last_usage() {
        let t = last_usage_tokens(FIXTURE).expect("usage");
        assert_eq!(t, 1200 + 182000 + 1000);
    }

    #[test]
    fn reads_turns() {
        let Determination::Known(turns) = recent_turns(FIXTURE, 60, 1200) else {
            panic!("the checked-in fixture must be readable");
        };
        assert!(turns.len() >= 2);
        assert_eq!(turns[0].role, "user");
    }

    #[test]
    fn truncate_chars_is_cjk_safe() {
        // Counts CHARS not bytes (each kana is 3 bytes in UTF-8) and never splits
        // a multi-byte char — the harness CJK invariant.
        assert_eq!(truncate_chars("あいう", 5), "あいう"); // under the cap → unchanged
        let t = truncate_chars("あいうえお", 3);
        assert!(t.starts_with("あいう"), "{t}");
        assert!(t.ends_with("[truncated]"), "{t}");
        // Exactly at the cap keeps the whole string (boundary, no ellipsis).
        assert_eq!(truncate_chars("あいうえお", 5), "あいうえお");
    }
}
