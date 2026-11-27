//! Which of a working tree's changed files a gate may **stop looking at**.
//!
//! A working tree carries no session identity. `git diff --name-only` in a
//! checkout two sessions share answers for BOTH of them, so a gate that reads
//! it presents a peer's edits as yours and blocks you on them (backlog
//! `1e44bfd9`, observed 2026-07-21 session c6a1fdbf and 2026-07-23 session
//! 143f3d21). That is not merely noisy: the shortest way out of a block you did
//! not earn is to create a skip marker, and under parallel sessions that is the
//! shared one-shot hatch CLAUDE.md 第5節 forbids. The gate's own pressure pushes
//! the operator toward disabling the gate.
//!
//! The session's **transcript** is the artefact that does carry identity. The
//! obvious rule — keep the intersection of "changed" and "what my transcript
//! records me editing" — is WRONG, and this module exists to not implement it.
//!
//! # The measurement
//!
//! [`crate::transcript::files_edited_by_session`] collects paths from `Edit` /
//! `Write` / `MultiEdit` / `NotebookEdit` tool_use blocks. Run over this
//! repository's own session `e9ebfcb6` on 2026-09-09 — a 12.6 MB transcript,
//! 90 recorded paths — against the working tree that same session produced:
//!
//! ```text
//! working tree changed: 10 path(s)
//!   IN footprint  crates/donegate/src/main.rs
//!   ...
//!   MISSING       crates/harness-core/src/lib.rs
//!   MISSING       crates/harness-core/tests/gate_giveup_concession.rs
//! ```
//!
//! Both MISSING files were written by that very session. `lib.rs` was edited
//! with `sed -i`, which is a `Bash` tool_use and leaves no edit block at all;
//! the test file was authored by a subagent, whose blocks live in its own
//! sidechain transcript. So the footprint is a strict UNDER-approximation:
//!
//! ```text
//! f in my footprint      ==> f IS mine     (sound)
//! f not in my footprint  ==> UNKNOWN       (NOT "f is not mine")
//! ```
//!
//! Intersecting would read the second line as the third, and drop shell-driven
//! and subagent-authored edits from a gate that blocks on hard-coded secrets
//! and missing tests — a fail-open introduced by the fix itself, which is the
//! class 第3節 exists to forbid.
//!
//! # The rule
//!
//! An exclusion must be a **positive observation**, never the absence of one. A
//! file leaves the working set only when some *other* session's footprint
//! claims it and this session's does not. A file no transcript claims is
//! unattributed, and unattributed stays in.
//!
//! [`crate::transcript::files_edited_by_session_and_subagents`] narrows the gap
//! for subagents (that half is observable); nothing can close the `sed -i` half,
//! which is why the rule above is structural rather than a stopgap.
//!
//! # Residual hole — stated because a limit nobody is told about is a lie
//!
//! One case is still permissive and this module cannot fix it: a file **I**
//! edited through the shell and a **peer** edited through `Edit` is absent from
//! my footprint and present in theirs, so it is excluded from my gate even
//! though I wrote to it. Both halves of that coincidence are needed, and under
//! CLAUDE.md 第8節 (one worktree per session) it cannot arise at all — but 第8節
//! is exactly the invariant already broken whenever this module matters, so the
//! hole is real rather than theoretical. Closing it would need shell-driven
//! writes to be observable, which would mean interpreting arbitrary `Bash`
//! command lines; a heuristic there would fail permissively in a *new* way. The
//! honest state is: narrowed, bounded, and disclosed — never silently widened.
//!
//! # Two rules, chosen per consumer
//!
//! The subtraction rule above ([`attribute_from_transcript`] /
//! [`attribute_from_session`]) is what `precommit-audit` (a pre-commit hook,
//! which receives only `CLAUDE_CODE_SESSION_ID` and must locate the transcript
//! itself) uses.
//!
//! The Stop gates `tdd` and `reviewgate` use [`attribute_own_writes`] instead:
//! on 2026-10-17 the user ruled that those gates judge only what the stopping
//! session wrote (「検証テストの範囲は書いた文だけでいい」). That rule IS the
//! intersection the measurement above warns about, narrowed only partly: it
//! also credits files a `Bash` command of the session names, which covers the
//! `sed -i` / heredoc half, and it reads subagent sidechains strictly. What it
//! cannot see — writes by commands that never name the file — is excluded, and
//! that residual hole is stated on the function and must be surfaced by the
//! callers (they list the excluded files). It is a deliberate, user-chosen
//! trade, not a claim that the hole is closed.
//!
//! Both live here because more than one gate needs each; copies of a rule
//! whose failure mode is permissive drift in the permissive direction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::verdict::Determination;

/// What a gate should do with each file a working-tree scan returned.
#[derive(Debug)]
#[must_use]
pub enum Attribution {
    /// This session's footprint was read.
    ///
    /// `keep` is what the gate must still act on. Under the subtraction rule
    /// ([`attribute_from_transcript`]) that is files this session is known to
    /// have edited **plus** files no transcript claims at all, and `excluded`
    /// is the narrow set positively attributed to a different session. Under
    /// [`attribute_own_writes`] `keep` is only what this session's transcript
    /// shows it writing, and `excluded` is everything else. Either way it is kept as a
    /// list so the exclusion can be NAMED rather than silently applied — an
    /// exclusion nobody is told about is the same defect as an inclusion nobody
    /// is told about.
    Narrowed {
        keep: Vec<String>,
        excluded: Vec<String>,
    },
    /// This session's own footprint could not be read, or (subtraction rule)
    /// peer liveness could not be decided. **Do not narrow.** Report the
    /// reason and keep the full set.
    Undetermined { why: String },
}

impl Attribution {
    /// The file list the caller should act on: `keep` when attribution
    /// succeeded, and — deliberately — the FULL set when it did not.
    pub fn files<'a>(&'a self, full: &'a [String]) -> &'a [String] {
        match self {
            Attribution::Narrowed { keep, .. } => keep,
            Attribution::Undetermined { .. } => full,
        }
    }
}

/// Resolve `p` as far as the filesystem allows, so an absolute path recorded by
/// the transcript and a repo-relative path from `git` compare equal.
///
/// `canonicalize` also resolves the symlinked temp dirs some hosts use, which
/// is why it is applied to BOTH sides rather than just joining the root. A
/// deleted file cannot be canonicalised; falling back to the unresolved path is
/// correct there — worst case the two spellings differ, the file matches
/// neither footprint, and it stays in `keep`, which is the restrictive
/// direction.
pub fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn resolved(paths: &BTreeSet<String>) -> BTreeSet<PathBuf> {
    paths.iter().map(|p| resolve(Path::new(p))).collect()
}

/// Split `changed` (repo-relative paths under `root`) using already-computed
/// footprints. Pure apart from `canonicalize`, so the rule itself is testable
/// without a transcript on disk.
///
/// `peers` is an already-`Known` peer footprint: a caller holding an
/// `Undetermined` one must not call this, but return
/// `Attribution::Undetermined` itself (as [`attribute_from_transcript`] does).
pub fn attribute_with_footprints(
    root: &Path,
    mine: Determination<BTreeSet<String>>,
    peers: &BTreeSet<String>,
    changed: &[String],
) -> Attribution {
    let mine = match mine {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Attribution::Undetermined {
                why: why.as_str().to_string(),
            }
        }
    };
    let mine = resolved(&mine);
    let peers = resolved(peers);

    let mut keep = Vec::new();
    let mut excluded = Vec::new();
    for c in changed {
        let abs = resolve(&root.join(c));
        // Order matters: my own evidence wins over a peer's. An overlap means
        // we both touched the file, and a file I touched is never someone
        // else's problem to review.
        if mine.contains(&abs) {
            keep.push(c.clone());
        } else if peers.contains(&abs) {
            excluded.push(c.clone());
        } else {
            // Unattributed. NOT mine-by-absence: see the module docstring's
            // measurement — `sed -i` edits appear in no footprint at all.
            keep.push(c.clone());
        }
    }
    Attribution::Narrowed { keep, excluded }
}

/// Split `changed` by reading the transcript at `transcript_path` — the Stop
/// hook's entry point, where the payload names the file directly.
///
/// Peers are looked for in the transcript's own projects tree
/// (`<transcript>/../..`), so the Stop hook needs no extra configuration to
/// find them — but only when that grandparent really is a `projects` directory.
/// Handed a transcript from anywhere else, the peer set would become whatever
/// `.jsonl` files happen to sit two levels up, and a WRONG peer set produces
/// FALSE EXCLUSIONS: files dropped from the gate on the strength of a stranger's
/// footprint. That failure is permissive, so the shape is checked rather than
/// assumed, and a mismatch yields no peers at all.
///
/// A transcript there is only a peer's FOOTPRINT. Whether its session is a peer
/// at all is decided by condukt's claim registry for the repository at `root`
/// (a live heartbeat). An unreadable or unparseable registry is
/// `Attribution::Undetermined` (nothing narrowed, reason surfaced), not a
/// `Narrowed` with no exclusions — see
/// [`crate::transcript::peer_edit_footprint_in`] (backlog `eef58891`).
pub fn attribute_from_transcript(
    root: &Path,
    transcript_path: &str,
    changed: &[String],
) -> Attribution {
    let mine = crate::transcript::files_edited_by_session_and_subagents(transcript_path);
    // Compute peers only if they can still matter. On the `Undetermined` arm
    // the result is discarded, and building it first meant walking the whole
    // transcript store — measured at 93 MB — to throw the answer away on every
    // Stop hook and every `git commit`.
    if matches!(mine, Determination::Undetermined(_)) {
        return attribute_with_footprints(root, mine, &BTreeSet::new(), changed);
    }
    let path = Path::new(transcript_path);
    let my_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let peers = match path.parent().and_then(Path::parent) {
        Some(projects) if projects.file_name().is_some_and(|n| n == "projects") => {
            // Liveness comes from condukt's claim registry for the repo at
            // `root` (backlog 873a2621); transcripts supply only footprints.
            crate::transcript::peer_edit_footprint_for(
                projects,
                &my_id,
                crate::transcript::PEER_ACTIVE_WINDOW,
                root,
            )
        }
        _ => Determination::known(BTreeSet::new()),
    };
    // Peer liveness could not be decided (claim registry unreadable or
    // unparseable): do not narrow, and say why (backlog eef58891). An empty
    // peer set here would produce a `Narrowed` that reads as "checked: no
    // peers", indistinguishable from a real observation.
    let peers = match peers {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => {
            return Attribution::Undetermined {
                why: format!("peer liveness: {}", why.as_str()),
            }
        }
    };
    attribute_with_footprints(root, mine, &peers, changed)
}

/// Scope `changed` to **what this session wrote** — the Stop-gate rule chosen
/// by the user on 2026-10-17 (「検証テストの範囲は書いた文だけでいい」) for `tdd` and
/// `reviewgate`. It supersedes, for those two gates, the subtraction rule of
/// [`attribute_from_transcript`] (which `precommit-audit` still uses).
///
/// A changed file is kept iff this session's transcript (parent **and** every
/// subagent sidechain) shows it being written:
///
/// * an `Edit` / `Write` / `MultiEdit` / `NotebookEdit` tool_use names it, or
/// * a `Bash` tool_use command contains its absolute path (as `root` spells
///   it, or canonicalised), its repo-relative path, or its path relative to
///   the `cwd` that transcript line recorded, in a statement that is not
///   provably read-only ([`writing_statements`]). The test is a substring
///   test, so it OVER-attributes (`src/a.rs` also matches a mention of
///   `crates/x/src/a.rs`); over-attribution keeps more files in the gate, the
///   restrictive side. A statement made only of read-only commands
///   (`git diff a.js`, `grep … a.js | head`, `tdd skip --reason "… a.js …"`, `donegate skip --reason "… a.js …"`)
///   does not count: before this rule, reading a peer's dirty file — or
///   naming it in the very skip reason that says "not mine" — made the file
///   this session's and blocked every later stop (observed 2026-10-20,
///   session 0402317c, `src/services/api.js`).
///
/// Every other changed file goes to `excluded` — no peer evidence is needed.
///
/// # Fail-closed
///
/// If the parent transcript, any sidechain, or the sidechain directory listing
/// cannot be read, the answer is `Undetermined` and the caller judges the whole
/// checkout. Unlike the subtraction rule, a missing sidechain here would turn a
/// subagent-written file into "not mine", so sidechains are read strictly.
///
/// # Residual hole — disclosed, not closed
///
/// A write made by a command that never names the file — `cargo fmt`, a code
/// generator, a script, a shell glob (`sed -i … src/*.rs`), `git checkout` /
/// `git stash pop` of other paths — is attributed to nobody and is EXCLUDED.
/// Callers must list `excluded` visibly so such a file can be noticed; they
/// must not drop the list silently.
pub fn attribute_own_writes(root: &Path, transcript_path: &str, changed: &[String]) -> Attribution {
    let undetermined = |why: String| Attribution::Undetermined { why };
    let mut edited = match crate::transcript::files_edited_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => return undetermined(why.as_str().to_string()),
    };
    let mut bash = match crate::transcript::bash_commands_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => return undetermined(why.as_str().to_string()),
    };
    let sides = match crate::transcript::subagent_transcripts_strict(Path::new(transcript_path)) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => return undetermined(why.as_str().to_string()),
    };
    for side in sides {
        let s = side.to_string_lossy();
        match crate::transcript::files_edited_by_session(&s) {
            Determination::Known(v) => edited.extend(v),
            Determination::Undetermined(why) => {
                return undetermined(format!("subagent transcript: {}", why.as_str()))
            }
        }
        match crate::transcript::bash_commands_by_session(&s) {
            Determination::Known(v) => bash.extend(v),
            Determination::Undetermined(why) => {
                return undetermined(format!("subagent transcript: {}", why.as_str()))
            }
        }
    }
    let edited = resolved(&edited);
    let canon_root = resolve(root);

    let mut keep = Vec::new();
    let mut excluded = Vec::new();
    for c in changed {
        let joined = root.join(c);
        let abs = resolve(&joined);
        let mut needles: Vec<String> = vec![
            c.clone(),
            joined.to_string_lossy().into_owned(),
            abs.to_string_lossy().into_owned(),
            canon_root.join(c).to_string_lossy().into_owned(),
        ];
        needles.retain(|n| !n.is_empty());
        let named = |call: &crate::transcript::BashCall| {
            // A path relative to the directory the command ran in.
            let rel = call.cwd.as_deref().and_then(|cwd| {
                let cwd = resolve(Path::new(cwd));
                abs.strip_prefix(&cwd)
                    .ok()
                    .map(|r| r.to_string_lossy().into_owned())
                    .filter(|r| !r.is_empty())
            });
            writing_statements(&call.command).iter().any(|text| {
                needles.iter().any(|n| text.contains(n.as_str()))
                    || rel.as_deref().is_some_and(|r| text.contains(r))
            })
        };
        if edited.contains(&abs) || bash.iter().any(named) {
            keep.push(c.clone());
        } else {
            excluded.push(c.clone());
        }
    }
    Attribution::Narrowed { keep, excluded }
}

/// The parts of a shell `command` that may have written the files they name.
///
/// The command is split into statements at unquoted `;` `&&` `||` `&`
/// newline `(` `)`; a statement is a pipeline. A pipeline is dropped only when
/// EVERY stage is provably read-only ([`read_only_stage`]): a writing stage
/// anywhere in it keeps the whole pipeline, because `grep -l x a.js | xargs
/// sed -i …` writes a file only the read-only stage names.
///
/// A command substitution (`$(…)`, backticks, `<(…)`, `>(…)`) is followed to
/// its balanced end and makes its WHOLE enclosing statement opaque: that
/// statement is always kept (its text includes the substitution body), since
/// neither the inner command nor what the outer command does with its output
/// is judged. Only the OTHER statements of the command may still be dropped
/// (GH #1530: one read-only `c=$(readlink …)` used to keep every `cd <wt> &&
/// git status` of the same command and made peers' worktrees Stop roots).
///
/// A `cd` statement is kept when any LATER statement is kept: the directory it
/// enters is where that later statement's relative paths land.
///
/// Anything this parser cannot follow keeps the WHOLE command (the old,
/// over-attributing rule): an unterminated heredoc, an unterminated quote or
/// substitution, and a substitution holding `case`, a comment, a heredoc or a
/// nested backtick. Being unable to tell is not "read only" (CLAUDE.md 第3節).
/// A heredoc is parsed by [`statements`] (body skipped as data), but a command
/// holding one still keeps the whole command here, since the body may name files.
fn writing_statements(command: &str) -> Vec<&str> {
    let Some(stmts) = statements(command) else {
        return vec![command];
    };
    // A heredoc keeps the WHOLE command (its body may name files).
    if stmts.iter().any(|s| s.heredoc) {
        return vec![command];
    }
    let mut keep: Vec<bool> = stmts
        .iter()
        .map(|s| s.opaque || !s.stages.iter().all(read_only_stage))
        .collect();
    let mut later_kept = false;
    for (k, s) in stmts.iter().enumerate().rev() {
        if later_kept && s.stages.iter().any(|st| program(st) == Some("cd")) {
            keep[k] = true;
        }
        later_kept |= keep[k];
    }
    stmts
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(s, _)| &command[s.start..s.end])
        .collect()
}

#[derive(Default)]
struct Stage {
    words: Vec<String>,
    /// An output redirect to something other than `/dev/null` or an fd.
    writes: bool,
    /// The raw target text of each such redirect (quotes/escapes unprocessed).
    redirects: Vec<String>,
}

struct Statement {
    start: usize,
    end: usize,
    stages: Vec<Stage>,
    /// Holds a command substitution: kept whatever its stages are.
    opaque: bool,
    /// Holds a heredoc (`<<`); its body was skipped as data.
    heredoc: bool,
    /// The separator that ENDS this statement.
    post: Sep,
}

/// What ended a [`Statement`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// End of the command.
    End,
    /// `;` or a newline.
    Seq,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `&` (a background job).
    Bg,
    /// `(`: a subshell opens after this statement.
    Open,
    /// `)`: the subshell closes after this statement.
    Close,
}

/// End (exclusive) of the `$(…)` / `<(…)` / `>(…)` whose `(` is at `open`.
/// `None` when it is unterminated or holds what a paren count cannot follow:
/// `case` (unbalanced `)` patterns), a comment, a heredoc.
fn paren_subst_end(command: &str, open: usize) -> Option<usize> {
    let b = command.as_bytes();
    let mut depth = 0usize;
    let mut j = open;
    loop {
        match *b.get(j)? {
            b'\\' => j += 2,
            b'\'' => j = command.get(j + 1..)?.find('\'')? + j + 2,
            b'"' => j = dquote_end(command, j)? + 1,
            b'`' => j = backtick_end(command, j)?,
            b'<' if b.get(j + 1) == Some(&b'<') => return None,
            b'#' if b" \t\n;&|(".contains(&b[j - 1]) => return None,
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth -= 1;
                j += 1;
                if depth == 0 {
                    return (!command[open..j].contains("case")).then_some(j);
                }
            }
            _ => j += 1,
        }
    }
}

/// End (exclusive) of the backtick substitution opened at `open`. `None` when
/// unterminated, nested (an escaped backtick inside) or holding a heredoc.
fn backtick_end(command: &str, open: usize) -> Option<usize> {
    let b = command.as_bytes();
    let mut j = open + 1;
    loop {
        match *b.get(j)? {
            b'\\' if b.get(j + 1) == Some(&b'`') => return None,
            b'\\' => j += 2,
            b'`' => return Some(j + 1),
            b'<' if b.get(j + 1) == Some(&b'<') => return None,
            _ => j += 1,
        }
    }
}

/// Index of the `"` closing the double quote opened at `open`, stepping over
/// the substitutions inside it.
fn dquote_end(command: &str, open: usize) -> Option<usize> {
    let b = command.as_bytes();
    let mut j = open + 1;
    loop {
        match *b.get(j)? {
            b'"' => return Some(j),
            b'\\' => j += 2,
            b'`' => j = backtick_end(command, j)?,
            b'$' if b.get(j + 1) == Some(&b'(') => j = paren_subst_end(command, j + 1)?,
            _ => j += 1,
        }
    }
}

/// Quote-aware split of `command` into pipelines. `None` = a construct this
/// parser does not follow (see [`writing_statements`]).
fn statements(command: &str) -> Option<Vec<Statement>> {
    let b = command.as_bytes();
    let mut out = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut stage = Stage::default();
    let mut word = String::new();
    let mut in_word = false;
    let mut start = 0;
    let mut opaque = false;
    let mut heredoc = false;
    let mut bodysubst = false;
    // `(delimiter, quoted, strip_tabs)` of heredocs whose body starts after
    // the next newline.
    let mut pending: Vec<(String, bool, bool)> = Vec::new();
    let mut i = 0;

    fn end_word(stage: &mut Stage, word: &mut String, in_word: &mut bool) {
        if *in_word {
            stage.words.push(std::mem::take(word));
            *in_word = false;
        }
    }

    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' => {
                // `\x` is a literal x; byte-indexing is fine: `\` is ASCII and
                // the char after it is copied whole below.
                let ch = command[i + 1..].chars().next();
                if let Some(ch) = ch {
                    word.push(ch);
                    i += 1 + ch.len_utf8();
                } else {
                    i += 1;
                }
                in_word = true;
                continue;
            }
            b'\'' => {
                let close = command[i + 1..].find('\'')? + i + 1;
                word.push_str(&command[i + 1..close]);
                in_word = true;
                i = close + 1;
                continue;
            }
            b'"' => {
                let j = dquote_end(command, i)?;
                let inner = &command[i + 1..j];
                opaque |= inner.contains("$(") || inner.contains('`');
                word.push_str(inner);
                in_word = true;
                i = j + 1;
                continue;
            }
            b'`' | b'$' | b'<' | b'>' if c == b'`' || b.get(i + 1) == Some(&b'(') => {
                let end = if c == b'`' {
                    backtick_end(command, i)?
                } else {
                    paren_subst_end(command, i + 1)?
                };
                word.push_str(&command[i..end]);
                in_word = true;
                opaque = true;
                i = end;
                continue;
            }
            b'<' if b.get(i + 1) == Some(&b'<') && b.get(i + 2) != Some(&b'<') => {
                // Heredoc: stdin, not a word. The body is data, skipped at
                // the next newline.
                end_word(&mut stage, &mut word, &mut in_word);
                let mut j = i + 2;
                let strip = b.get(j) == Some(&b'-');
                if strip {
                    j += 1;
                }
                while b.get(j).is_some_and(|d| *d == b' ' || *d == b'\t') {
                    j += 1;
                }
                let mut delim = String::new();
                let mut quoted = false;
                while let Some(&d) = b.get(j) {
                    match d {
                        b'\'' => {
                            let e = command.get(j + 1..)?.find('\'')? + j + 1;
                            delim.push_str(&command[j + 1..e]);
                            quoted = true;
                            j = e + 1;
                        }
                        b'"' => {
                            let e = command.get(j + 1..)?.find('"')? + j + 1;
                            delim.push_str(&command[j + 1..e]);
                            quoted = true;
                            j = e + 1;
                        }
                        b'\\' => {
                            let ch = command.get(j + 1..)?.chars().next()?;
                            delim.push(ch);
                            quoted = true;
                            j += 1 + ch.len_utf8();
                        }
                        // Expansion in the delimiter word: not followed.
                        b'$' | b'`' => return None,
                        _ if b" \t\n;&|()<>".contains(&d) => break,
                        _ => {
                            let ch = command[j..].chars().next()?;
                            delim.push(ch);
                            j += ch.len_utf8();
                        }
                    }
                }
                if delim.is_empty() {
                    return None;
                }
                pending.push((delim, quoted, strip));
                heredoc = true;
                i = j;
                continue;
            }
            b'<' if b.get(i + 1) == Some(&b'<') => return None,
            b'>' => {
                end_word(&mut stage, &mut word, &mut in_word);
                // A leading fd number (`2>`) was already pushed as a word.
                if matches!(stage.words.last(), Some(w) if w.bytes().all(|d| d.is_ascii_digit())) {
                    stage.words.pop();
                }
                let mut j = i + 1;
                if b.get(j) == Some(&b'>') {
                    j += 1;
                }
                if b.get(j) == Some(&b'&') {
                    // `>&2`: to a file descriptor, not a file.
                    j += 1;
                    while b.get(j).is_some_and(|d| d.is_ascii_digit() || *d == b'-') {
                        j += 1;
                    }
                } else {
                    while b.get(j).is_some_and(|d| *d == b' ' || *d == b'\t') {
                        j += 1;
                    }
                    let t = j;
                    // The target word, stepping over quotes and substitutions
                    // (`> "$(f)"`, `> out$(date)`) so their bodies are not
                    // split off as statements of their own.
                    while let Some(&d) = b.get(j) {
                        match d {
                            b'`' => {
                                j = backtick_end(command, j)?;
                                opaque = true;
                            }
                            b'$' if b.get(j + 1) == Some(&b'(') => {
                                j = paren_subst_end(command, j + 1)?;
                                opaque = true;
                            }
                            b'"' => {
                                let e = dquote_end(command, j)?;
                                let inner = &command[j + 1..e];
                                opaque |= inner.contains("$(") || inner.contains('`');
                                j = e + 1;
                            }
                            b'\'' => j = command.get(j + 1..)?.find('\'')? + j + 2,
                            b'\\' => j += 2,
                            _ if b" \t\n;&|()<>".contains(&d) => break,
                            _ => j += 1,
                        }
                    }
                    j = j.min(b.len());
                    if &command[t..j] != "/dev/null" {
                        stage.writes = true;
                        stage.redirects.push(command[t..j].to_string());
                    }
                }
                i = j;
                continue;
            }
            b'&' if b.get(i + 1) == Some(&b'>') => {
                // `&>file`: same as `>file` for this purpose.
                end_word(&mut stage, &mut word, &mut in_word);
                i += 1;
                continue;
            }
            b'|' if b.get(i + 1) != Some(&b'|') => {
                end_word(&mut stage, &mut word, &mut in_word);
                stages.push(std::mem::take(&mut stage));
                i += 1;
                continue;
            }
            b';' | b'\n' | b'&' | b'|' | b'(' | b')' => {
                end_word(&mut stage, &mut word, &mut in_word);
                stages.push(std::mem::take(&mut stage));
                let doubled = (c == b'&' || c == b'|') && b.get(i + 1) == Some(&c);
                out.push(Statement {
                    start,
                    end: i,
                    stages: std::mem::take(&mut stages),
                    opaque: std::mem::take(&mut opaque),
                    heredoc: std::mem::take(&mut heredoc),
                    post: match (c, doubled) {
                        (b'&', true) => Sep::And,
                        (b'|', _) => Sep::Or,
                        (b'&', false) => Sep::Bg,
                        (b'(', _) => Sep::Open,
                        (b')', _) => Sep::Close,
                        _ => Sep::Seq,
                    },
                });
                let mut j = i + 1;
                if (c == b'&' || c == b'|') && b.get(j) == Some(&c) {
                    j += 1;
                }
                if c == b'\n' {
                    for (delim, quoted, strip) in pending.drain(..) {
                        // Body: up to a line equal to the delimiter. A
                        // missing terminator is a construct not followed.
                        let mut found = false;
                        while j <= b.len() {
                            let nl = command[j..].find('\n').map(|n| n + j);
                            let end = nl.unwrap_or(b.len());
                            let line = &command[j..end];
                            let line = if strip {
                                line.trim_start_matches('\t')
                            } else {
                                line
                            };
                            if line == delim {
                                j = nl.map_or(b.len(), |n| n + 1);
                                found = true;
                                break;
                            }
                            // An unquoted body expands: a substitution runs.
                            if !quoted && (line.contains("$(") || line.contains('`')) {
                                bodysubst = true;
                            }
                            match nl {
                                Some(n) => j = n + 1,
                                None => break,
                            }
                        }
                        if !found {
                            return None;
                        }
                    }
                    if bodysubst {
                        bodysubst = false;
                        if let Some(last) = out.last_mut() {
                            last.opaque = true;
                        }
                    }
                }
                start = j;
                i = j;
                continue;
            }
            b' ' | b'\t' => {
                end_word(&mut stage, &mut word, &mut in_word);
            }
            _ => {
                let ch = command[i..].chars().next()?;
                word.push(ch);
                in_word = true;
                i += ch.len_utf8();
                continue;
            }
        }
        i += 1;
    }
    end_word(&mut stage, &mut word, &mut in_word);
    stages.push(stage);
    out.push(Statement {
        start,
        end: b.len(),
        stages,
        opaque,
        heredoc,
        post: Sep::End,
    });
    if !pending.is_empty() {
        // Heredoc whose body never started (no newline after it).
        return None;
    }
    Some(out)
}

/// A pipeline stage that cannot write a file. Conservative: anything not on
/// this list — `awk`, `cp`, `xargs`, `find`, `sort`, an interpreter, a bare
/// `VAR=value` — may write and is NOT read-only. `sed` is read-only only when
/// [`sed_read_only`] can prove it (print-only, no in-place, no script file).
fn read_only_stage(stage: &Stage) -> bool {
    if stage.writes {
        return false;
    }
    let mut words = stage.words.iter().map(String::as_str).peekable();
    // Shell keywords that only introduce the real command.
    while let Some(w) = words.peek() {
        if matches!(
            *w,
            "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time"
        ) {
            words.next();
        } else {
            break;
        }
    }
    let rest: Vec<&str> = words.collect();
    let Some(first) = rest.first() else {
        // Nothing runs: an empty stage (a trailing `;`, a leading `(`) or
        // only introducing keywords.
        return true;
    };
    if is_keyword(first) {
        return rest.iter().all(|w| is_keyword(w));
    }
    let prog = first.rsplit('/').next().unwrap_or(first);
    match prog {
        "cat" | "head" | "tail" | "less" | "more" | "grep" | "egrep" | "fgrep" | "rg" | "ls"
        | "wc" | "file" | "stat" | "diff" | "cmp" | "echo" | "printf" | "test" | "["
        | "realpath" | "readlink" | "dirname" | "basename" | "nl" | "cd" | "pwd" | "true"
        | "false" | "which" | "md5sum" | "sha1sum" | "sha256sum" | "du" | "df" | "tree" | "cut"
        | "tr" | "jq" | "column" => true,
        "git" => {
            let mut args = rest[1..].iter();
            let sub = loop {
                match args.next() {
                    Some(&("-C" | "-c")) => {
                        args.next();
                    }
                    Some(a) if a.starts_with('-') => {}
                    other => break other,
                }
            };
            matches!(
                sub.copied(),
                Some(
                    "diff"
                        | "show"
                        | "log"
                        | "status"
                        | "blame"
                        | "grep"
                        | "ls-files"
                        | "cat-file"
                        | "rev-parse"
                        | "shortlog"
                        | "describe"
                )
            ) && !rest.iter().any(|a| a.starts_with("--output"))
        }
        "sed" => sed_read_only(&rest[1..]),
        "tdd" | "reviewgate" | "donegate" => rest.get(1) == Some(&"skip"),
        _ => false,
    }
}

/// `sed` arguments (program name excluded) that provably cannot write a file.
///
/// Fail-closed whitelist: only the flags `-n -s -E -r -z -u` (singly or
/// clustered), `--quiet/--silent/--regexp-extended/--null-data/--unbuffered/
/// --separate/--posix`, `-e SCRIPT`, `--expression=SCRIPT` and `--` are
/// understood. Anything else starting with `-` (`-i`, `-ni`, `--in-place`,
/// `-f`, `--file`, `-l`, a cluster containing an unknown letter) is "may
/// write", as is a script outside [`sed_script_print_only`].
fn sed_read_only(args: &[&str]) -> bool {
    let mut scripts: Vec<&str> = Vec::new();
    let mut positional: Vec<&str> = Vec::new();
    let mut it = args.iter().copied();
    let mut opts_done = false;
    while let Some(a) = it.next() {
        if opts_done || a == "-" || !a.starts_with('-') {
            positional.push(a);
        } else if a == "--" {
            opts_done = true;
        } else if a == "-e" {
            match it.next() {
                Some(sc) => scripts.push(sc),
                None => return false,
            }
        } else if let Some(sc) = a.strip_prefix("--expression=") {
            scripts.push(sc);
        } else if matches!(
            a,
            "--quiet"
                | "--silent"
                | "--regexp-extended"
                | "--null-data"
                | "--unbuffered"
                | "--separate"
                | "--posix"
        ) {
            // harmless long flag
        } else if !a.starts_with("--") && a[1..].chars().all(|c| "nsErzu".contains(c)) {
            // harmless short-flag cluster (no `i`, `e`, `f`)
        } else {
            return false;
        }
    }
    if scripts.is_empty() {
        if positional.is_empty() {
            return false;
        }
        scripts.push(positional.remove(0));
    }
    scripts.iter().all(|sc| sed_script_print_only(sc))
}

/// A sed script made only of addresses and the commands `p n d q l =` plus
/// separators and `{ }` grouping. Regex addresses `/re/` are allowed when they
/// hold no backslash or newline (so the closing `/` is unambiguous). No `w`,
/// `W`, `e`, `s`, `y`, `r`, `a`, `i`, `c`, `b`, `t` ... — anything unlisted is
/// "may write".
fn sed_script_print_only(script: &str) -> bool {
    if script.trim().is_empty() {
        return false;
    }
    let mut chars = script.chars();
    while let Some(c) = chars.next() {
        match c {
            '0'..='9'
            | ','
            | '$'
            | '!'
            | '~'
            | '+'
            | ' '
            | '\t'
            | ';'
            | '\n'
            | '{'
            | '}'
            | 'p'
            | 'n'
            | 'd'
            | 'q'
            | 'l'
            | '=' => {}
            '/' => {
                let mut closed = false;
                for r in chars.by_ref() {
                    match r {
                        '/' => {
                            closed = true;
                            break;
                        }
                        '\\' | '\n' => return false,
                        _ => {}
                    }
                }
                if !closed {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Basename of the program a stage runs, past the introducing keywords.
fn program(stage: &Stage) -> Option<&str> {
    let first = stage.words.iter().map(String::as_str).find(|w| {
        !matches!(
            *w,
            "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time"
        )
    })?;
    first.rsplit('/').next()
}

fn is_keyword(w: &str) -> bool {
    matches!(w, "done" | "fi" | "esac" | "{" | "}")
}

/// Split `changed` for a caller that knows only a **session id** — a pre-commit
/// hook, which gets no Stop payload and so must locate the transcript itself.
///
/// Every failure on the way (no session id, no `$HOME`, no transcript file) is
/// `Undetermined`, never an empty footprint: an empty footprint plus a peer set
/// would exclude files this session actually wrote.
pub fn attribute_from_session(root: &Path, session_id: &str, changed: &[String]) -> Attribution {
    match crate::transcript::locate_session_transcript(session_id) {
        Determination::Known(p) => attribute_from_transcript(root, &p.to_string_lossy(), changed),
        Determination::Undetermined(why) => Attribution::Undetermined {
            why: why.as_str().to_string(),
        },
    }
}

/// Absolute paths the session wrote, split by how sure we are they are paths.
pub struct SessionWrites {
    /// Targets of Edit / Write / MultiEdit / NotebookEdit (parent session and
    /// every subagent sidechain). Each is definitely a path the session wrote.
    pub edited: BTreeSet<PathBuf>,
    /// Path-looking words (absolute, or relative to the command's cwd) in the
    /// statements of recorded Bash calls that [`attribute_own_writes`] would
    /// treat as possibly writing. Heuristic: a word that is not an existing
    /// path is not a candidate. A write through a glob / variable / generator
    /// that never names its target is not seen (same disclosed hole as
    /// [`attribute_own_writes`]).
    pub bash: BTreeSet<PathBuf>,
}

/// Every file the session (and its subagents) is recorded as having written.
/// An unreadable transcript or sidechain is `Undetermined`, never an empty set.
pub fn session_written_paths(transcript_path: &str) -> Determination<SessionWrites> {
    let mut all = match own_written_paths(transcript_path) {
        Determination::Known(w) => w,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    let sides = match crate::transcript::subagent_transcripts_strict(Path::new(transcript_path)) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    for side in sides {
        match own_written_paths(&side.to_string_lossy()) {
            Determination::Known(w) => {
                all.edited.extend(w.edited);
                all.bash.extend(w.bash);
            }
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "subagent transcript: {}",
                    why.as_str()
                ))
            }
        }
    }
    Determination::known(all)
}

const DIR_CMDS: [&str; 3] = ["cd", "pushd", "popd"];
/// Words that start a compound command; a directory change after one of them
/// is conditional or grouped, never a plain statement.
const COMPOUND_WORDS: [&str; 8] = ["{", "if", "then", "else", "elif", "do", "while", "until"];
/// Commands that run another command: a directory command among their
/// arguments is not followed.
const WRAPPERS: [&str; 11] = [
    "builtin", "command", "env", "xargs", "sudo", "nohup", "nice", "exec", "eval", "source", ".",
];

/// Where the shell is, as a set of candidate directories while a statement
/// runs. One element is the exact answer; several mean the shell may be in
/// any of them (over-attribution: the restrictive side). Empty = unknown (the
/// transcript line recorded no cwd).
struct Walk {
    /// Every statement's candidate directories, in statement order.
    bases: Vec<Vec<PathBuf>>,
    /// Existing directories a followed `cd` entered, by statement index.
    entered: Vec<(usize, PathBuf)>,
}

/// `(after a compound keyword, command word, arguments)` of a directory command.
type DirCmd<'a> = (bool, &'a str, Vec<&'a str>);

/// The directory command (`cd`/`pushd`/`popd`) a stage runs in command-word
/// position, with its arguments. `Err` when one hides behind a wrapper
/// (`builtin cd`, `eval "cd x"`, `xargs cd`): not followed, so undetermined.
/// Matches PARSED words only, so `git commit -m 'make cd work'` and heredoc
/// bodies (skipped by [`statements`]) never match.
fn stage_dir_cmd(stage: &Stage) -> Result<Option<DirCmd<'_>>, String> {
    let words: Vec<&str> = stage.words.iter().map(String::as_str).collect();
    let mut i = 0;
    let mut compound = false;
    while let Some(w) = words.get(i) {
        let assignment = w.split_once('=').is_some_and(|(n, _)| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if COMPOUND_WORDS.contains(w) {
            compound = true;
        } else if !(assignment || matches!(*w, "!" | "time")) {
            break;
        }
        i += 1;
    }
    let Some(&cmd) = words.get(i) else {
        return Ok(None);
    };
    if DIR_CMDS.contains(&cmd) {
        return Ok(Some((compound, cmd, words[i + 1..].to_vec())));
    }
    if WRAPPERS.contains(&cmd)
        && words[i + 1..]
            .iter()
            .any(|a| a.split_whitespace().any(|t| DIR_CMDS.contains(&t)))
    {
        return Err(format!(
            "`{cmd}` runs a directory command that is not followed"
        ));
    }
    Ok(None)
}

/// Literal `CDPATH` entries set on the line (`CDPATH=a:b`), `Ok(None)` when
/// the line never mentions it, `Err` when it is mentioned in a form that is not
/// a literal assignment (`$CDPATH`, `CDPATH=$X`).
fn cdpath_entries(stmts: &[Statement], command: &str) -> Result<Option<Vec<String>>, String> {
    if !command.contains("CDPATH") {
        return Ok(None);
    }
    let mut entries = Vec::new();
    let mut seen = false;
    for s in stmts {
        for w in s.stages.iter().flat_map(|st| st.words.iter()) {
            if let Some(v) = w.strip_prefix("CDPATH=") {
                if !plain_word_loose(v) {
                    return Err("CDPATH is set to a value that is not a literal".to_string());
                }
                seen = true;
                entries.extend(v.split(':').map(str::to_string));
            } else if w.contains("CDPATH") {
                return Err("CDPATH is used in a form that is not a literal assignment".to_string());
            }
        }
    }
    if seen {
        Ok(Some(entries))
    } else {
        Err("CDPATH is mentioned but not assigned a literal value".to_string())
    }
}

/// [`plain_word`] but a `:`-list is allowed (and an empty value).
fn plain_word_loose(v: &str) -> bool {
    !v.chars()
        .any(|c| "*?[]{}$~!`\\\"'<>|&;()#".contains(c) || c.is_whitespace())
}

/// Follow the shell's directory through ONE recorded command, statement by
/// statement, so a relative path is joined with where the shell IS when that
/// statement runs, not with the transcript line's cwd (a sidechain line
/// records the PARENT root).
///
/// Which directory changes MOVE the cwd (replace it): a plain top-level
/// `cd <literal>` / `pushd <literal>` that is a statement of its own (one
/// pipeline stage, no compound keyword before it, not backgrounded, not
/// followed by `||`), reached unconditionally (first, after `;`/newline, at
/// the start of a subshell, or after `&&` with only `&&` after it), whose
/// target is an existing directory. Inside `( ... )` the change lasts to the
/// closing `)`.
///
/// Every other directory change whose target IS a literal (a conditional or
/// background `cd`, one in a pipeline or after `if`/`{`, one whose target does
/// not exist, `CDPATH` candidates) does NOT replace the cwd: the shell may or
/// may not be there, so the candidates are ADDED to the cwd set (the
/// restrictive side). `popd`, `pushd +N`, `cd -` may return to any directory
/// the command was in, so the cwd set becomes all of them. A `cd` whose
/// target is not a literal (`cd`, `cd $X`, `cd ~`), or a directory command
/// behind a wrapper, is `Err`: undetermined.
fn walk_cwd(command: &str, stmts: &[Statement], init: Option<&Path>) -> Result<Walk, String> {
    let cdpath = cdpath_entries(stmts, command)?;
    let mut cur: Vec<PathBuf> = init.map(|p| vec![p.to_path_buf()]).unwrap_or_default();
    let mut history: Vec<PathBuf> = cur.clone();
    let mut stack: Vec<Vec<PathBuf>> = Vec::new();
    let mut walk = Walk {
        bases: Vec::new(),
        entered: Vec::new(),
    };
    // rest_and[k]: every separator from statement k on is `&&` (or the end).
    let rest_and: Vec<bool> = (0..stmts.len())
        .map(|k| {
            stmts[k..stmts.len().saturating_sub(1)]
                .iter()
                .all(|s| s.post == Sep::And)
        })
        .collect();
    for (k, s) in stmts.iter().enumerate() {
        walk.bases.push(cur.clone());
        let pre = if k == 0 { Sep::Seq } else { stmts[k - 1].post };
        if s.opaque {
            // A substitution is not parsed: a directory command inside it is
            // not followed.
            let text = &command[s.start..s.end];
            if text
                .split_whitespace()
                .any(|t| DIR_CMDS.contains(&t.trim_matches(|c: char| "()`;\"'{}".contains(c))))
            {
                return Err(format!(
                    "`{}` holds a directory command inside a substitution",
                    text.trim()
                ));
            }
        } else {
            for stage in &s.stages {
                let Some((compound, cmd, args)) = stage_dir_cmd(stage)? else {
                    continue;
                };
                let single = s.stages.len() == 1 && !compound;
                let reached = matches!(pre, Sep::Seq | Sep::Open | Sep::Close)
                    || (pre == Sep::And && rest_and[k]);
                let certain = single && reached && !matches!(s.post, Sep::Bg | Sep::Or);
                // Arguments: skip `--` / `-P` / `-L`; `-` and `+N`/`-N` and
                // popd go back to a directory seen earlier.
                let mut args = args
                    .into_iter()
                    .filter(|a| !matches!(*a, "--" | "-P" | "-L"));
                let target = args.next();
                if args.next().is_some() {
                    return Err(format!(
                        "`{cmd}` with more than one argument is not followed"
                    ));
                }
                let rotate = cmd == "popd"
                    || target == Some("-")
                    || (cmd == "pushd"
                        && target.is_none_or(|t| {
                            (t.starts_with('+') || t.starts_with('-'))
                                && t.len() > 1
                                && t[1..].chars().all(|c| c.is_ascii_digit())
                        }));
                if rotate {
                    for h in &history {
                        if !cur.contains(h) {
                            cur.push(h.clone());
                        }
                    }
                    continue;
                }
                let Some(t) = target else {
                    return Err(format!(
                        "`{cmd}` without a target goes to the home directory"
                    ));
                };
                if t.starts_with('-') || !plain_word(t) {
                    return Err(format!("`{cmd} {t}`: the target is not a literal path"));
                }
                let p = Path::new(t);
                let mut cands: Vec<PathBuf> = Vec::new();
                if p.is_absolute() {
                    cands.push(p.to_path_buf());
                } else if cur.is_empty() {
                    // No known cwd: the target stays relative; callers count
                    // a relative write path as inside every root.
                    cands.push(p.to_path_buf());
                } else {
                    for b in &cur {
                        cands.push(b.join(p));
                        if let Some(es) = &cdpath {
                            for e in es {
                                let e = Path::new(e);
                                let root = if e.is_absolute() || e.as_os_str().is_empty() {
                                    if e.as_os_str().is_empty() {
                                        b.clone()
                                    } else {
                                        e.to_path_buf()
                                    }
                                } else {
                                    b.join(e)
                                };
                                cands.push(root.join(p));
                            }
                        }
                    }
                }
                let existing: Vec<PathBuf> = cands
                    .iter()
                    .filter(|c| c.is_relative() || c.is_dir())
                    .cloned()
                    .collect();
                for c in &existing {
                    walk.entered.push((k, c.clone()));
                }
                if certain && !existing.is_empty() {
                    cur = existing.clone();
                } else {
                    for c in cands {
                        if !cur.contains(&c) {
                            cur.push(c);
                        }
                    }
                }
                for c in &cur {
                    if !history.contains(c) {
                        history.push(c.clone());
                    }
                }
            }
        }
        match s.post {
            Sep::Open => stack.push(cur.clone()),
            Sep::Close => {
                cur = stack
                    .pop()
                    .ok_or_else(|| "a `)` with no matching `(`".to_string())?;
            }
            _ => {}
        }
    }
    Ok(walk)
}

/// Words of a statement that may name a path: the parsed words and redirect
/// targets (quotes removed, a heredoc body is data and absent). A statement
/// holding a command substitution is not parsed into words, so its raw text is
/// split on whitespace (over-attribution).
fn statement_path_words(command: &str, s: &Statement) -> Vec<String> {
    if s.opaque {
        return command[s.start..s.end]
            .split_whitespace()
            .map(|r| {
                r.trim_matches(|c: char| matches!(c, '"' | '\'' | '>' | ';' | '(' | ')'))
                    .to_string()
            })
            .collect();
    }
    s.stages
        .iter()
        .flat_map(|st| st.words.iter().chain(st.redirects.iter()))
        .cloned()
        .collect()
}

/// The `keep` flag of each statement of [`writing_statements`] (before its
/// heredoc rule): not provably read-only, or a `cd` that a kept statement
/// follows.
fn keep_flags(stmts: &[Statement]) -> Vec<bool> {
    let mut keep: Vec<bool> = stmts
        .iter()
        .map(|s| s.opaque || !s.stages.iter().all(read_only_stage))
        .collect();
    let mut later_kept = false;
    for (k, s) in stmts.iter().enumerate().rev() {
        if later_kept && s.stages.iter().any(|st| program(st) == Some("cd")) {
            keep[k] = true;
        }
        later_kept |= keep[k];
    }
    keep
}

/// The writing statements of one recorded Bash call and the existing paths
/// they name. `(false, _, _)` = the call is provably read-only. A writing call
/// whose set is empty wrote through something that names no file (`cargo fmt`,
/// a glob, a generator, `git checkout`): it is unattributable.
///
/// A relative word is joined with the directory the shell is in when its
/// statement runs, as [`walk_cwd`] defines it: the transcript line's cwd,
/// replaced only by a plain top-level `cd <literal dir>` the shell definitely
/// stays in, widened (never replaced) by a `cd` that may not have run or
/// whose directory does not exist. The third field is `Some(reason)` only
/// when a `cd` cannot be resolved at all (`cd $X`, `cd`, a wrapper); the set
/// is then not complete and the caller must not read it as such.
fn call_named_paths(
    call: &crate::transcript::BashCall,
) -> (bool, BTreeSet<PathBuf>, Option<String>) {
    let mut out = BTreeSet::new();
    let Some(stmts) = statements(&call.command) else {
        // Not parsed: the whole command is kept, whitespace-split. A
        // directory command anywhere in it is not followed.
        let undetermined = call
            .command
            .split_whitespace()
            .any(|t| DIR_CMDS.contains(&t))
            .then(|| "an unparsed command holds a directory command".to_string());
        let bases: Vec<PathBuf> = call.cwd.iter().map(PathBuf::from).collect();
        for raw in call.command.split_whitespace() {
            let w = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | '>' | ';' | '(' | ')'));
            insert_named(&mut out, w, &bases);
        }
        return (true, out, undetermined);
    };
    let walk = match walk_cwd(&call.command, &stmts, call.cwd.as_deref().map(Path::new)) {
        Ok(w) => w,
        Err(why) => return (true, out, Some(why)),
    };
    let keep = if stmts.iter().any(|s| s.heredoc) {
        vec![true; stmts.len()]
    } else {
        keep_flags(&stmts)
    };
    for (k, s) in stmts.iter().enumerate() {
        if !keep[k] {
            continue;
        }
        for w in statement_path_words(&call.command, s) {
            insert_named(&mut out, &w, &walk.bases[k]);
        }
    }
    for (k, dir) in &walk.entered {
        if keep[*k] {
            out.insert(if dir.is_relative() {
                dir.clone()
            } else {
                resolve(dir)
            });
        }
    }
    (keep.iter().any(|k| *k), out, None)
}

/// Add the path `w` names to `out`: absolute, or joined with each candidate
/// directory in `bases` (a relative word with no known directory is skipped,
/// as before). A relative result cannot be placed and is kept as is.
fn insert_named(out: &mut BTreeSet<PathBuf>, w: &str, bases: &[PathBuf]) {
    if w.is_empty() || w.starts_with('-') {
        return;
    }
    let p = Path::new(w);
    let mut put = |abs: PathBuf| {
        if abs.is_relative() {
            out.insert(abs);
        } else if abs.exists() {
            out.insert(resolve(&abs));
        }
    };
    if p.is_absolute() {
        put(p.to_path_buf());
    } else if w.contains('/') || w.contains('.') {
        for b in bases {
            put(b.join(p));
        }
    }
}

/// Whether this session (parent transcript and every subagent sidechain)
/// ran any Bash call with a possibly-writing stage outside the closed writer
/// allowlist of [`stage_attributable`] (a writer whose targets are not exactly
/// named files). Such a write cannot be attributed to a file, so a
/// caller that narrows to "files this session wrote" must NOT narrow when
/// this is `true` (CLAUDE.md 3). An unreadable transcript or sidechain is
/// `Undetermined`, never `false`.
pub fn has_unattributable_write(transcript_path: &str) -> Determination<bool> {
    let mut all = match crate::transcript::bash_commands_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    match crate::transcript::subagent_transcripts_strict(Path::new(transcript_path)) {
        Determination::Known(sides) => {
            for side in sides {
                match crate::transcript::bash_commands_by_session(&side.to_string_lossy()) {
                    Determination::Known(v) => all.extend(v),
                    Determination::Undetermined(why) => {
                        return Determination::undetermined(format!(
                            "subagent transcript: {}",
                            why.as_str()
                        ))
                    }
                }
            }
        }
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    }
    Determination::known(all.iter().any(call_is_unattributable))
}

/// Per STAGE, not per call: a call is unattributable when any stage that may
/// write (not provably read-only, not a bare `cd`/`pushd`) is not
/// [`stage_attributable`] — i.e. its program is outside the closed writer
/// allowlist (`sed -i`, `tee`, `touch`, `rm`, `cp`, `mv`, file redirects) or
/// its targets are not exactly named files. Merely naming some existing file
/// does NOT make a stage attributable (`cargo fmt -- a.rs`, `python3 gen.py`).
/// A `cd` target or directory argument is not a written file, and a path named
/// by a DIFFERENT stage does not cover another writer. A command the parser
/// cannot follow, or holding a substitution, is unattributable.
fn call_is_unattributable(call: &crate::transcript::BashCall) -> bool {
    let Some(stmts) = statements(&call.command) else {
        return true;
    };
    // A directory command this cannot place makes the call unattributable.
    let Ok(walk) = walk_cwd(&call.command, &stmts, call.cwd.as_deref().map(Path::new)) else {
        return true;
    };
    for (k, s) in stmts.iter().enumerate() {
        if s.opaque {
            return true;
        }
        let bases = &walk.bases[k];
        for stage in &s.stages {
            if read_only_stage(stage) || matches!(program(stage), Some("cd" | "pushd")) {
                continue;
            }
            // Attributable only if it is, whichever directory the shell is in.
            let ok = if bases.is_empty() {
                stage_attributable(stage, None)
            } else {
                bases.iter().all(|b| stage_attributable(stage, Some(b)))
            };
            if !ok {
                return true;
            }
        }
    }
    false
}

/// A word that names exactly one path: no glob, expansion, brace, tilde,
/// history or escape character survives in it (the parser has already removed
/// quotes, so a quoted glob is also refused: when in doubt, it is not named).
fn plain_word(w: &str) -> bool {
    !w.is_empty()
        && !w
            .chars()
            .any(|c| "*?[]{}$~!`\\\"'<>|&;()#".contains(c) || c.is_whitespace())
}

/// `w` as a write target: a plain word that resolves (against `cwd` when
/// relative) to something that is not a directory. A relative path with no
/// known `cwd`, a trailing `/`, an existing directory: not a file target.
/// A symlink (checked with `symlink_metadata` on the cwd-resolved path) is not
/// a named file either: the write lands on the link's target. A nonexistent
/// path stays a file target; any `symlink_metadata` error other than
/// `NotFound` is undetermined and fails closed (not a file target).
fn file_target(w: &str, cwd: Option<&Path>) -> bool {
    if !plain_word(w) || w.ends_with('/') {
        return false;
    }
    let p = Path::new(w);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(c) = cwd {
        c.join(p)
    } else {
        return false;
    };
    match std::fs::symlink_metadata(&abs) {
        Ok(m) if m.file_type().is_symlink() => false,
        Ok(_) => !abs.is_dir(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// `sed` script of exactly one `s` command whose flags cannot write or run
/// anything (`g`, `p`, `i`, `I`, digits; never `w`, `e`, `m`/`M` ambiguity
/// aside). No backslash or newline, so the delimiters are unambiguous.
fn sed_subst_only(script: &str) -> bool {
    let mut chars = script.chars();
    if chars.next() != Some('s') {
        return false;
    }
    let Some(delim) = chars.next() else {
        return false;
    };
    if delim.is_alphanumeric() || delim.is_whitespace() || matches!(delim, '\\' | ';' | '{' | '}') {
        return false;
    }
    if script.contains(['\\', '\n']) {
        return false;
    }
    let rest: Vec<&str> = chars.as_str().split(delim).collect();
    // `pattern`, `replacement`, `flags`
    rest.len() == 3 && rest[2].chars().all(|c| "gpiI0123456789".contains(c))
}

/// Operands (non-flag words) of a table writer, or `None` when it carries a
/// flag outside `allowed_flags` (an unknown flag may redirect the write).
fn operands<'a>(args: &[&'a str], allowed_flags: &[&str]) -> Option<Vec<&'a str>> {
    let mut out = Vec::new();
    let mut opts_done = false;
    for &a in args {
        if opts_done || a == "-" || !a.starts_with('-') {
            out.push(a);
        } else if a == "--" {
            opts_done = true;
        } else if allowed_flags.contains(&a) {
        } else {
            return None;
        }
    }
    Some(out)
}

/// Whether a possibly-writing stage's write targets are exactly determinable.
///
/// CLOSED allowlist, fail-closed: a stage is attributable only when
/// 1. every output redirect (`> f`, `>> f`) names a [`file_target`], AND
/// 2. its program word (bare, containing no `/`; a path-qualified program is
///    never in the table) is in this table and its arguments match the form:
///    - `sed -i|--in-place [-E|-r|-n] [-e] 's/…/…/flags' FILE...` — only a single
///      plain `s` command ([`sed_subst_only`]), no backup suffix, no script file.
///      The BSD/macOS `-i ''` (separate, exactly empty argument = no backup) is
///      accepted like `-i`; `-i .bak`, `-i.bak`, `--in-place=.bak` are not;
///    - a read-only program (`cat`) with a plain-file output redirect and a
///      heredoc stdin (`cat > f <<'EOF'`): the heredoc body is data, not a
///      stage ([`statements`]); an unquoted-delimiter body holding `$(` or a
///      backtick marks the statement opaque (the substitution runs);
///    - `tee [-a] FILE...`, `touch FILE...`, `rm [-f] FILE...` — every operand
///      is the written (or removed) file;
///    - `cp SRC DEST`, `mv SRC DEST` — exactly two file operands, no flags: DEST
///      (and for `mv`, SRC, which is removed) is the target; a directory DEST is
///      refused because the written path would be `DEST/<name>`, not named;
///    - a stage with no program (only redirects) — covered by (1).
///
/// Everything else is unattributable: interpreters (`python`, `bash`, `sh`,
/// `node`), formatters (`cargo fmt`, `rustfmt`), build tools, `git apply`/
/// `stash`/`checkout`, `patch`, `xargs`, any unknown flag, glob or directory
/// target. Naming SOME existing file on the command line does not make an
/// arbitrary program's writes attributable.
fn stage_attributable(stage: &Stage, cwd: Option<&Path>) -> bool {
    if !stage.redirects.iter().all(|t| file_target(t, cwd)) {
        return false;
    }
    let words: Vec<&str> = stage
        .words
        .iter()
        .map(String::as_str)
        .skip_while(|w| {
            matches!(
                *w,
                "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time"
            )
        })
        .collect();
    let Some(first) = words.first() else {
        return true;
    };
    // A read-only program whose only effect is its (already validated) output
    // redirect: `cat a > b.txt`.
    if !stage.redirects.is_empty()
        && read_only_stage(&Stage {
            words: stage.words.clone(),
            writes: false,
            redirects: Vec::new(),
        })
    {
        return true;
    }
    let args = &words[1..];
    let all_files = |ops: &[&str]| !ops.is_empty() && ops.iter().all(|o| file_target(o, cwd));
    // A program word containing '/' is never in the table: `./touch`,
    // `/tmp/x/tee` may be any binary named like a writer.
    match *first {
        "tee" => operands(args, &["-a"]).is_some_and(|o| all_files(&o)),
        "touch" => operands(args, &[]).is_some_and(|o| all_files(&o)),
        "rm" => operands(args, &["-f"]).is_some_and(|o| all_files(&o)),
        "cp" | "mv" => operands(args, &[]).is_some_and(|o| o.len() == 2 && all_files(&o)),
        "sed" => {
            let mut in_place = false;
            let mut script: Option<&str> = None;
            let mut files: Vec<&str> = Vec::new();
            let mut it = args.iter().copied().peekable();
            while let Some(a) = it.next() {
                match a {
                    "-i" => {
                        in_place = true;
                        // BSD/macOS `-i ''`: the separate empty argument is
                        // an empty backup suffix = no backup file.
                        if it.peek() == Some(&"") {
                            it.next();
                        }
                    }
                    "--in-place" => in_place = true,
                    "-E" | "-r" | "-n" => {}
                    "-e" => match (script, it.next()) {
                        (None, Some(s)) => script = Some(s),
                        _ => return false,
                    },
                    _ if a.starts_with('-') => return false,
                    _ if script.is_none() => script = Some(a),
                    _ => files.push(a),
                }
            }
            in_place && script.is_some_and(sed_subst_only) && all_files(&files)
        }
        _ => false,
    }
}

/// The files ONE transcript records as written: the transcript at
/// `transcript_path` only, without its subagent sidechains ([`session_written_paths`]
/// is the union of this over the parent and every sidechain). Lets a caller
/// tell WHICH transcript wrote a path. Same failure contract: an unreadable
/// transcript is `Undetermined`, never an empty set.
pub fn own_written_paths(transcript_path: &str) -> Determination<SessionWrites> {
    let edited = match crate::transcript::files_edited_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    let bash = match crate::transcript::bash_commands_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    let mut bash_paths = BTreeSet::new();
    for call in &bash {
        let (_, named, undetermined) = call_named_paths(call);
        if let Some(why) = undetermined {
            // Not an empty or partial set: callers read a set as "everything
            // this session wrote", so an unresolvable `cd` makes it unknown.
            return Determination::undetermined(format!(
                "a Bash write follows a `cd` that cannot be resolved statically ({why})"
            ));
        }
        bash_paths.extend(named);
    }
    Determination::known(SessionWrites {
        edited: resolved(&edited),
        bash: bash_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sed_stage(cmd: &str) -> bool {
        let stmts = statements(cmd).expect("parses");
        stmts
            .iter()
            .all(|s| !s.opaque && s.stages.iter().all(read_only_stage))
    }

    #[test]
    fn sed_print_only_is_read_only() {
        assert!(sed_stage("sed -n 1,5p f"));
        assert!(sed_stage("sed -n '/foo/,/bar/p' f"));
        assert!(sed_stage("sed -n -e 1p -e 3p f"));
    }

    #[test]
    fn sed_that_may_write_is_not_read_only() {
        for c in [
            "sed -i s/a/b/ f",
            "sed -ni p f",
            "sed --in-place s/a/b/ f",
            "sed -n 'w out' f",
            "sed 's/a/b/w out' f",
            "sed -f script f",
            "sed -e 1e f",
            "sed s/a/b/ f",
            "sed -n 1,5p f > out",
            "sed",
        ] {
            assert!(!sed_stage(c), "{c} must stay may-write");
        }
    }

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| (*s).to_string()).collect()
    }

    fn known(paths: &[&str]) -> Determination<BTreeSet<String>> {
        Determination::Known(set(paths))
    }

    #[test]
    fn an_unreadable_transcript_does_not_narrow_anything() {
        let root = Path::new("/nonexistent-root");
        let changed = vec!["a.rs".to_string(), "b.rs".to_string()];
        let a = attribute_with_footprints(
            root,
            Determination::undetermined("transcript unreadable"),
            &set(&[]),
            &changed,
        );
        match &a {
            Attribution::Undetermined { why } => assert!(why.contains("unreadable"), "{why}"),
            other => panic!("must not narrow on an unreadable transcript, got {other:?}"),
        }
        assert_eq!(
            a.files(&changed),
            changed.as_slice(),
            "判定不能 must keep the FULL set: narrowing here would turn `cannot tell whose \
             these are` into `none of these are mine`, silently switching the gate off"
        );
    }

    #[test]
    fn a_file_no_transcript_claims_is_kept_not_excluded() {
        let dir = tempdir("unattributed");
        let changed = vec!["a.rs".to_string(), "b.rs".to_string()];
        let a = attribute_with_footprints(&dir, known(&[]), &set(&[]), &changed);
        match a {
            Attribution::Narrowed { keep, excluded } => {
                assert_eq!(
                    keep, changed,
                    "an empty footprint on both sides means UNATTRIBUTED, not `nobody's`. \
                     Measured: a `sed -i` edit and a subagent-authored file appear in no \
                     footprint at all, so excluding here drops real edits from a gate that \
                     blocks on secrets and missing tests"
                );
                assert!(
                    excluded.is_empty(),
                    "an exclusion requires positive evidence of a peer, and there is none here"
                );
            }
            other => panic!("a READ transcript must narrow, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_peers_own_files_are_excluded() {
        let dir = tempdir("split");
        for f in ["mine.rs", "theirs.rs", "nobodys.rs", "both.rs"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let abs = |f: &str| resolve(&dir.join(f)).to_string_lossy().into_owned();
        let mine = abs("mine.rs");
        let both_m = abs("both.rs");
        let theirs = abs("theirs.rs");
        let both_p = abs("both.rs");
        let changed = vec![
            "mine.rs".to_string(),
            "theirs.rs".to_string(),
            "nobodys.rs".to_string(),
            "both.rs".to_string(),
        ];
        let a = attribute_with_footprints(
            &dir,
            known(&[&mine, &both_m]),
            &set(&[&theirs, &both_p]),
            &changed,
        );
        match a {
            Attribution::Narrowed { keep, excluded } => {
                assert_eq!(
                    excluded,
                    vec!["theirs.rs".to_string()],
                    "only a file a PEER claims and I do not may be excluded"
                );
                assert_eq!(
                    keep,
                    vec![
                        "mine.rs".to_string(),
                        "nobodys.rs".to_string(),
                        "both.rs".to_string()
                    ],
                    "`nobodys.rs` is unattributed and `both.rs` is an overlap; both stay in"
                );
            }
            other => panic!("expected a narrowed split, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_session_id_is_undetermined_not_an_empty_footprint() {
        let changed = vec!["a.rs".to_string()];
        let a = attribute_from_session(Path::new("/tmp"), "", &changed);
        assert!(
            matches!(a, Attribution::Undetermined { .. }),
            "no session id means the footprint was NOT observed; an empty footprint alongside \
             a non-empty peer set would exclude files this session actually wrote"
        );
        assert_eq!(a.files(&changed), changed.as_slice());
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hc-attr-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
