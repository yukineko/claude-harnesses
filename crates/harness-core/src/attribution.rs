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
///   (`git diff a.js`, `grep … a.js | head`, `tdd skip --reason "… a.js …"`)
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
/// Anything this parser cannot follow keeps the WHOLE command (the old,
/// over-attributing rule): command substitution (`$(…)`, backticks), a
/// heredoc (`<<`), an unterminated quote. Being unable to tell is not "read
/// only" (CLAUDE.md 第3節).
fn writing_statements(command: &str) -> Vec<&str> {
    match statements(command) {
        Some(stmts) => stmts
            .into_iter()
            .filter(|s| !s.stages.iter().all(read_only_stage))
            .map(|s| &command[s.start..s.end])
            .collect(),
        None => vec![command],
    }
}

#[derive(Default)]
struct Stage {
    words: Vec<String>,
    /// An output redirect to something other than `/dev/null` or an fd.
    writes: bool,
}

struct Statement {
    start: usize,
    end: usize,
    stages: Vec<Stage>,
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
                let mut j = i + 1;
                loop {
                    match b.get(j)? {
                        b'"' => break,
                        b'\\' => j += 2,
                        b'`' => return None,
                        b'$' if b.get(j + 1) == Some(&b'(') => return None,
                        _ => j += 1,
                    }
                }
                word.push_str(&command[i + 1..j]);
                in_word = true;
                i = j + 1;
                continue;
            }
            b'`' => return None,
            b'$' if b.get(i + 1) == Some(&b'(') => return None,
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
                    while b.get(j).is_some_and(|d| !b" \t\n;&|()<>".contains(d)) {
                        j += 1;
                    }
                    if &command[t..j] != "/dev/null" {
                        stage.writes = true;
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
                out.push(Statement {
                    start,
                    end: i,
                    stages: std::mem::take(&mut stages),
                });
                let mut j = i + 1;
                if (c == b'&' || c == b'|') && b.get(j) == Some(&c) {
                    j += 1;
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
    });
    Some(out)
}

/// A pipeline stage that cannot write a file. Conservative: anything not on
/// this list — `sed`, `awk`, `cp`, `xargs`, `find`, `sort`, an interpreter, a
/// bare `VAR=value` — may write and is NOT read-only.
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
        "tdd" | "reviewgate" => rest.get(1) == Some(&"skip"),
        _ => false,
    }
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
    let mut edited = match crate::transcript::files_edited_by_session(transcript_path) {
        Determination::Known(v) => v,
        Determination::Undetermined(why) => {
            return Determination::undetermined(why.as_str().to_string())
        }
    };
    let mut bash = match crate::transcript::bash_commands_by_session(transcript_path) {
        Determination::Known(v) => v,
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
        let s = side.to_string_lossy();
        match crate::transcript::files_edited_by_session(&s) {
            Determination::Known(v) => edited.extend(v),
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "subagent transcript: {}",
                    why.as_str()
                ))
            }
        }
        match crate::transcript::bash_commands_by_session(&s) {
            Determination::Known(v) => bash.extend(v),
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "subagent transcript: {}",
                    why.as_str()
                ))
            }
        }
    }
    let mut bash_paths = BTreeSet::new();
    for call in &bash {
        let cwd = call.cwd.as_deref().map(Path::new);
        for text in writing_statements(&call.command) {
            for raw in text.split_whitespace() {
                let w = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | '>' | ';' | '(' | ')'));
                if w.is_empty() || w.starts_with('-') {
                    continue;
                }
                let p = Path::new(w);
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else if let Some(c) = cwd.filter(|_| w.contains('/') || w.contains('.')) {
                    c.join(p)
                } else {
                    continue;
                };
                if abs.exists() {
                    bash_paths.insert(resolve(&abs));
                }
            }
        }
    }
    Determination::known(SessionWrites {
        edited: resolved(&edited),
        bash: bash_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
