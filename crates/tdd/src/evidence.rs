//! Evidence recorded inside a RED / GREEN proof beyond the bare exit code:
//!
//! * [`tree_identity`] — which tree the run saw: the HEAD commit, a hash of
//!   the uncommitted diff, and a content-based tree id (untracked files
//!   included, the proof dir excluded).
//! * [`parse_test_log`] — the per-test results and counts, read from the FULL
//!   run log (not from the bounded `output_tail`).
//!
//! Neither may fabricate a value it could not observe (CLAUDE.md section 3):
//! a git failure is an explicit `null` component plus a reason under
//! `undetermined` and `determined: false`, and a log with no libtest
//! `test result:` line is `counts.determined: false`, never `0 passed`.
//!
//! `tdd oracle` does NOT compare trees (user ruling 2026-11-17). The
//! identity is only recorded here; the comparison is `condukt merge`'s job.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(dir);
    c
}

/// Stdout of a git run that exited 0, else the reason it did not.
fn git_stdout(mut c: Command) -> Result<Vec<u8>, String> {
    let o = c.output().map_err(|e| format!("could not run git: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "git exited {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(o.stdout)
}

fn hex_line(bytes: Vec<u8>, what: &str) -> Result<String, String> {
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("git printed no usable {what}: {s:?}"));
    }
    Ok(s)
}

/// `git hash-object --stdin` of `data` (a SHA-1 of the bytes; no new crate).
fn hash_bytes(dir: &Path, data: &[u8]) -> Result<String, String> {
    let mut child = git(dir)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git hash-object: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git hash-object has no stdin")?;
    let payload = data.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&payload));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git hash-object failed: {e}"))?;
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("writing to git hash-object failed: {e}")),
        Err(_) => return Err("the git hash-object writer panicked".to_string()),
    }
    if !out.status.success() {
        return Err(format!(
            "git hash-object exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    hex_line(out.stdout, "object id")
}

/// The proof dir as a path relative to the repo top, when it lies inside it.
fn proof_dir_in_repo(top: &Path, root: &Path, proof_dir: &str) -> Option<PathBuf> {
    let abs = if Path::new(proof_dir).is_absolute() {
        PathBuf::from(proof_dir)
    } else {
        root.join(proof_dir)
    };
    let top = std::fs::canonicalize(top).ok()?;
    // The proof dir may not exist yet: canonicalize its nearest existing parent.
    let parent = abs.parent()?;
    let name = abs.file_name()?;
    let abs = std::fs::canonicalize(parent).ok()?.join(name);
    abs.strip_prefix(&top).ok().map(Path::to_path_buf)
}

/// The identity components, each `Ok(hex)` or `Err(reason)`.
struct Identity {
    head: Result<String, String>,
    diff_hash: Result<String, String>,
    tree: Result<String, String>,
}

fn compute(root: &Path, proof_dir: &str) -> Identity {
    let fail = |why: String| Identity {
        head: Err(why.clone()),
        diff_hash: Err(why.clone()),
        tree: Err(why),
    };
    let top = match git_stdout({
        let mut c = git(root);
        c.args(["rev-parse", "--show-toplevel"]);
        c
    }) {
        Ok(o) => PathBuf::from(String::from_utf8_lossy(&o).trim()),
        Err(e) => return fail(format!("not a usable git work tree: {e}")),
    };
    if top.as_os_str().is_empty() {
        return fail("git reported an empty top-level path".to_string());
    }
    let head = git_stdout({
        let mut c = git(&top);
        c.args(["rev-parse", "--verify", "-q", "HEAD^{commit}"]);
        c
    })
    .map_err(|e| format!("no HEAD commit (unborn HEAD or git failure): {e}"))
    .and_then(|o| hex_line(o, "HEAD sha"));

    // Pathspecs: everything under the repo top except the proof dir (and the
    // default `.tdd/`), which this very run writes.
    let mut excludes: Vec<String> = vec![".tdd".to_string()];
    if let Some(rel) = proof_dir_in_repo(&top, root, proof_dir) {
        let rel = rel.to_string_lossy().to_string();
        if !rel.is_empty() && !excludes.contains(&rel) {
            excludes.push(rel);
        }
    }
    let specs: Vec<String> = std::iter::once(".".to_string())
        .chain(excludes.iter().map(|e| format!(":(exclude){e}")))
        .collect();

    let tree = content_tree(&top, &excludes);
    let diff_hash = match (&head, &tree) {
        (Ok(h), Ok(t)) => git_stdout({
            let mut c = git(&top);
            c.args([
                "diff-tree",
                "-p",
                "--binary",
                "-r",
                "--no-color",
                h,
                t,
                "--",
            ]);
            c.args(&specs);
            c
        })
        .and_then(|d| hash_bytes(&top, &d)),
        (Err(e), _) => Err(format!("no diff base: {e}")),
        (_, Err(e)) => Err(format!("no content tree to diff: {e}")),
    };
    Identity {
        head,
        diff_hash,
        tree,
    }
}

/// `git add -A -- .` of the work tree (untracked included, `.gitignore`
/// honoured) into a throwaway index, then `excludes` are dropped from that
/// index with `git rm --cached --ignore-unmatch`, then `git write-tree`. The
/// excludes are NOT passed to `git add` as `:(exclude)` pathspecs: git exits 1
/// ("paths are ignored by one of your .gitignore files") when an exclude names
/// an existing ignored path such as `.tdd`, which left `tree` undetermined.
/// Any genuine git failure still propagates as `Err`. The real index is never
/// written. A gitlink (submodule / nested repo) records
/// only its HEAD commit, so the id would not see edits inside it: undetermined.
fn content_tree(top: &Path, excludes: &[String]) -> Result<String, String> {
    let tmp = tempfile::Builder::new()
        .prefix("tdd-tree-index-")
        .tempdir()
        .map_err(|e| format!("no temp dir for the throwaway index: {e}"))?;
    let index = tmp.path().join("index");
    let run = |args: &[&str], extra: &[String]| -> Result<Vec<u8>, String> {
        let mut c = git(top);
        c.env("GIT_INDEX_FILE", &index).args(args);
        if !extra.is_empty() {
            c.arg("--").args(extra);
        }
        git_stdout(c)
    };
    run(&["add", "-A"], &[".".to_string()])?;
    run(
        &["rm", "-r", "--cached", "-q", "--ignore-unmatch"],
        excludes,
    )?;
    let staged = run(&["ls-files", "-s"], &[])?;
    if String::from_utf8_lossy(&staged)
        .lines()
        .any(|l| l.starts_with("160000 "))
    {
        return Err(
            "a gitlink (submodule / nested repo) is present; its working content is not \
             captured by a tree id"
                .to_string(),
        );
    }
    hex_line(run(&["write-tree"], &[])?, "tree id")
}

/// The `tree_identity` object of a proof.
///
/// Determined components are hex strings; an undetermined one is `null` with
/// its reason under `undetermined`, and `determined` is `false` unless all
/// three were observed. `head` = HEAD commit sha; `diff_hash` = hash of the
/// uncommitted diff (HEAD to the working content, untracked files included);
/// `tree` = content-based tree id (`git write-tree` of the working content,
/// untracked included). The proof dir is excluded from `diff_hash` and `tree`,
/// so the same content gives the same `tree` before and after a commit.
pub fn tree_identity(root: &Path, proof_dir: &str) -> Value {
    let id = compute(root, proof_dir);
    let mut bad = serde_json::Map::new();
    let mut take = |name: &str, r: Result<String, String>| -> Value {
        match r {
            Ok(v) => Value::String(v),
            Err(e) => {
                bad.insert(name.to_string(), Value::String(e));
                Value::Null
            }
        }
    };
    let head = take("head", id.head);
    let diff_hash = take("diff_hash", id.diff_hash);
    let tree = take("tree", id.tree);
    let mut v = json!({
        "determined": bad.is_empty(),
        "head": head,
        "diff_hash": diff_hash,
        "tree": tree,
    });
    if !bad.is_empty() {
        v["undetermined"] = Value::Object(bad);
    }
    v
}

/// Result of one `test <name> ... <status>` line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum T {
    Pass,
    Fail,
    Ignored,
}

impl T {
    fn as_str(self) -> &'static str {
        match self {
            T::Pass => "pass",
            T::Fail => "fail",
            T::Ignored => "ignored",
        }
    }
    /// Least favourable to "passed" wins when one name is reported twice
    /// (the same test name in two binaries): fail > ignored > pass.
    fn rank(self) -> u8 {
        match self {
            T::Pass => 0,
            T::Ignored => 1,
            T::Fail => 2,
        }
    }
}

/// libtest's `running 1 test` / `running 5 tests` header (line start):
/// the number of per-test lines the block will print.
fn parse_running_line(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("running ")?;
    let (n, word) = rest.split_once(' ')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !matches!(word.trim(), "test" | "tests") {
        return None;
    }
    n.parse().ok()
}

fn parse_test_line(line: &str) -> Option<(String, T)> {
    let rest = line.strip_prefix("test ")?;
    let (name, status) = rest.split_once(" ... ")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let status = status.trim();
    let t = if status == "ok" {
        T::Pass
    } else if status == "FAILED" {
        T::Fail
    } else if status == "ignored" || status.starts_with("ignored,") {
        T::Ignored
    } else {
        return None;
    };
    Some((name.to_string(), t))
}

/// `test result: ok. 5 passed; 0 failed; 4 ignored; ...` -> (passed, failed, ignored).
fn parse_result_line(line: &str) -> Option<(u64, u64, u64)> {
    let rest = line.trim_start().strip_prefix("test result: ")?;
    let (_, nums) = rest.split_once(". ")?;
    let (mut p, mut f, mut i) = (None, None, None);
    for part in nums.split(';') {
        let part = part.trim();
        // Non-count segments (`finished in 0.01s`) are skipped; a segment
        // labelled passed/failed/ignored whose number is unparseable is
        // undetermined (-> None), never silently 0.
        let Some((n, label)) = part.split_once(' ') else {
            continue;
        };
        let label = label.trim();
        let counted = matches!(label, "passed" | "failed" | "ignored");
        let n: u64 = match n.parse() {
            Ok(n) => n,
            Err(_) if counted => return None,
            Err(_) => continue,
        };
        match label {
            "passed" => p = Some(n),
            "failed" => f = Some(n),
            "ignored" => i = Some(n),
            _ => {}
        }
    }
    Some((p?, f?, i?))
}

/// Merge one per-test result into the map: least favourable wins.
fn record(tests: &mut BTreeMap<String, T>, name: String, t: T) {
    tests
        .entry(name)
        .and_modify(|old| {
            if t.rank() > old.rank() {
                *old = t;
            }
        })
        .or_insert(t);
}

/// Summed `test result:` counts: (passed, failed, ignored, result lines).
type Counts = (u64, u64, u64, u64);

/// What a log yields: the per-test map and the summed counts (`None` = no
/// result line could be counted).
type Parsed = (BTreeMap<String, T>, Option<Counts>);

/// The conservative line scan: EVERY line-start `test <name> ... <status>`
/// line is a result and every `test result:` line is summed. A nested line
/// printed inside a failing test's captured output becomes a phantom name;
/// that direction is restrictive (a phantom RED failure blocks a merge), so
/// this is the fallback whenever the libtest framing cannot be reconciled.
fn scan_conservative(lines: &[String]) -> Parsed {
    let mut tests = BTreeMap::new();
    let (mut p, mut f, mut i, mut results) = (0u64, 0u64, 0u64, 0u64);
    for line in lines {
        if let Some((name, t)) = parse_test_line(line) {
            record(&mut tests, name, t);
        } else if let Some((a, b, c)) = parse_result_line(line) {
            p = p.saturating_add(a);
            f = f.saturating_add(b);
            i = i.saturating_add(c);
            results += 1;
        }
    }
    let counts = (results > 0).then_some((p, f, i, results));
    (tests, counts)
}

/// Does the `failures:` name list printed just before a result line name
/// exactly the block's failing tests? libtest lists the bare name, while the
/// per-test line may carry a mode suffix (`x - should panic`,
/// `src/lib.rs - f (line 3) - compile fail`).
fn failure_list_matches(list: &[String], failed: &[&str]) -> bool {
    if list.len() != failed.len() {
        return false;
    }
    let mut used = vec![false; list.len()];
    failed.iter().all(|m| {
        let hit = list.iter().enumerate().position(|(k, n)| {
            !used[k]
                && (*m == n.as_str()
                    || m.strip_prefix(n.as_str())
                        .is_some_and(|r| r.starts_with(" - ")))
        });
        match hit {
            Some(k) => {
                used[k] = true;
                true
            }
            None => false,
        }
    })
}

/// (passed, failed, ignored) of a block's per-test lines.
fn tally(got: &[(String, T)]) -> (u64, u64, u64) {
    got.iter().fold((0, 0, 0), |(p, f, i), (_, t)| match t {
        T::Pass => (p + 1, f, i),
        T::Fail => (p, f + 1, i),
        T::Ignored => (p, f, i + 1),
    })
}

/// Where [`scan_framed`] is inside the libtest structure.
enum Frame {
    /// Between blocks.
    Outside,
    /// After `running N tests`, before the N-th per-test line.
    Tests { n: u64, got: Vec<(String, T)> },
    /// After the N-th per-test line, before the block's terminator.
    Trailer {
        got: Vec<(String, T)>,
        /// A `failures:` / `successes:` header has been seen.
        in_section: bool,
        /// (is a `failures:` list, names) after the latest section header,
        /// while every line since then has been a list entry or blank.
        list: Option<(bool, Vec<String>)>,
    },
}

/// Parse the log by libtest's structure. Each binary prints
/// `running N tests`, then exactly N per-test lines, then optional
/// `failures:` / `successes:` sections (captured output, which may itself
/// contain libtest-shaped lines from nested runs), then ONE
/// `test result: ...` whose passed/failed/ignored equal the N lines' tallies
/// and, when anything failed, is preceded by a `failures:` list naming
/// exactly the failed tests. Only those N lines are results and only that
/// terminator is counted; everything between the N-th line and the
/// terminator is captured output and is ignored.
///
/// `None` = the framing does not reconcile (a per-test or result line
/// outside a block or before any section header, a `running` header among
/// the per-test lines, a block never closed): the caller falls back to
/// [`scan_conservative`]. A block is accepted only when its terminator's
/// counts and failure list match the lines taken as its results, so a real
/// failing line displaced by a nested one makes the block not reconcile
/// (fallback, which records every matching line) rather than vanish.
fn scan_framed(lines: &[String]) -> Option<Parsed> {
    let mut tests = BTreeMap::new();
    let (mut p, mut f, mut i, mut results) = (0u64, 0u64, 0u64, 0u64);
    let mut s = Frame::Outside;
    for line in lines {
        let line = line.as_str();
        s = match s {
            Frame::Outside => {
                if let Some(n) = parse_running_line(line) {
                    if n == 0 {
                        Frame::Trailer {
                            got: Vec::new(),
                            in_section: false,
                            list: None,
                        }
                    } else {
                        Frame::Tests { n, got: Vec::new() }
                    }
                } else if parse_test_line(line).is_some() || parse_result_line(line).is_some() {
                    return None;
                } else {
                    Frame::Outside
                }
            }
            Frame::Tests { n, mut got } => {
                if parse_running_line(line).is_some() {
                    return None;
                }
                // A `test result:` here is a nested run's (inherited stdout):
                // the real terminator cannot precede the N-th per-test line.
                if let Some(r) = parse_test_line(line) {
                    got.push(r);
                }
                if got.len() as u64 == n {
                    Frame::Trailer {
                        got,
                        in_section: false,
                        list: None,
                    }
                } else {
                    Frame::Tests { n, got }
                }
            }
            Frame::Trailer {
                got,
                in_section,
                list,
            } => {
                if let Some((a, b, c)) = parse_result_line(line) {
                    let failed: Vec<&str> = got
                        .iter()
                        .filter(|(_, t)| *t == T::Fail)
                        .map(|(k, _)| k.as_str())
                        .collect();
                    let listed = match &list {
                        Some((true, names)) => failure_list_matches(names, &failed),
                        _ => false,
                    };
                    if (a, b, c) == tally(&got) && (b == 0 || listed) {
                        for (name, t) in got {
                            record(&mut tests, name, t);
                        }
                        p = p.saturating_add(a);
                        f = f.saturating_add(b);
                        i = i.saturating_add(c);
                        results += 1;
                        Frame::Outside
                    } else if !in_section {
                        return None;
                    } else {
                        Frame::Trailer {
                            got,
                            in_section,
                            list: None,
                        }
                    }
                } else if line == "failures:" || line == "successes:" {
                    Frame::Trailer {
                        got,
                        in_section: true,
                        list: Some((line == "failures:", Vec::new())),
                    }
                } else if !in_section
                    && (parse_test_line(line).is_some() || parse_running_line(line).is_some())
                {
                    return None;
                } else if line.trim().is_empty() {
                    Frame::Trailer {
                        got,
                        in_section,
                        list,
                    }
                } else {
                    let entry = line
                        .strip_prefix("    ")
                        .filter(|n| !n.is_empty() && !n.starts_with(' '));
                    let list = match (list, entry) {
                        (Some((kind, mut names)), Some(n)) => {
                            names.push(n.to_string());
                            Some((kind, names))
                        }
                        _ => None,
                    };
                    Frame::Trailer {
                        got,
                        in_section,
                        list,
                    }
                }
            }
        };
    }
    if !matches!(s, Frame::Outside) {
        return None;
    }
    let counts = (results > 0).then_some((p, f, i, results));
    Some((tests, counts))
}

fn tests_json(tests: &BTreeMap<String, T>) -> Value {
    Value::Object(
        tests
            .iter()
            .map(|(k, v)| (k.clone(), json!(v.as_str())))
            .collect(),
    )
}

const NO_RESULT_LINE: &str = "no libtest `test result:` line in the run log (not a cargo test \
                              run, or it died before printing one); this is NOT 0 passed";

const UNRECONCILED: &str = "the libtest framing of the run log did not reconcile (a result line \
                            outside a `running N tests` block, a block whose per-test lines or \
                            `test result:` counts do not match, or a block never closed); every \
                            matching line was taken as a result and the pass count is unknown";

/// Per-test results and counts of one run, from its full log.
///
/// * `tests`: name -> `"pass"` / `"fail"` / `"ignored"` from libtest
///   `test <name> ... ok|FAILED|ignored` lines.
/// * `counts`: the sum of the libtest `test result:` lines (one per test
///   binary). `determined: false` with a reason when no such line could be
///   parsed or the log could not be read: "could not count" is not `0 passed`.
///
/// A log with `running N tests` headers is read by libtest's structure
/// ([`scan_framed`]): lines inside a failing test's captured output are not
/// results and a nested `test result:` is not counted. When that structure
/// does not reconcile, `tests` comes from the conservative scan (every
/// matching line, phantoms included — restrictive) and `counts` is
/// `determined: false`: a nested `test result:` could have inflated the sum,
/// so the pass count is unknown. A log with no `running` header at all (not
/// libtest's pretty format) is read by the conservative scan for both.
pub fn parse_test_log(log: &Path) -> (Value, Value) {
    let file = match std::fs::File::open(log) {
        Ok(f) => f,
        Err(e) => {
            return (
                json!({}),
                json!({"determined": false, "reason": format!("run log could not be read: {e}")}),
            )
        }
    };
    let mut reader = std::io::BufReader::new(file);
    let mut lines: Vec<String> = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                let (tests, _) = scan_conservative(&lines);
                return (
                    tests_json(&tests),
                    json!({"determined": false, "reason": format!("run log read failed midway: {e}")}),
                );
            }
        }
        let line = String::from_utf8_lossy(&buf);
        lines.push(line.trim_end_matches(['\n', '\r']).to_string());
    }
    let framed = lines.iter().any(|l| parse_running_line(l).is_some());
    let (tests, counts) = if !framed {
        let (tests, counts) = scan_conservative(&lines);
        (tests, counts.ok_or(NO_RESULT_LINE))
    } else {
        match scan_framed(&lines) {
            Some((tests, counts)) => (tests, counts.ok_or(NO_RESULT_LINE)),
            None => (scan_conservative(&lines).0, Err(UNRECONCILED)),
        }
    };
    let counts = match counts {
        Err(reason) => json!({"determined": false, "reason": reason}),
        Ok((p, f, i, results)) => json!({
            "determined": true,
            "passed": p,
            "failed": f,
            "ignored": i,
            "result_lines": results,
        }),
    };
    (tests_json(&tests), counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lines_parse_with_ignore_reason() {
        assert_eq!(
            parse_test_line("test a::b ... ok"),
            Some(("a::b".into(), T::Pass))
        );
        assert_eq!(
            parse_test_line("test a::c ... FAILED"),
            Some(("a::c".into(), T::Fail))
        );
        assert_eq!(
            parse_test_line("test a::d ... ignored, RED: why ... here"),
            Some(("a::d".into(), T::Ignored))
        );
        assert_eq!(parse_test_line("test result: ok. 1 passed"), None);
        assert_eq!(parse_test_line("testing ... ok"), None);
    }

    #[test]
    fn result_line_needs_all_three_counts() {
        assert_eq!(
            parse_result_line(
                "test result: FAILED. 2 passed; 3 failed; 4 ignored; 0 measured; 1 filtered out; finished in 0.01s"
            ),
            Some((2, 3, 4))
        );
        assert_eq!(parse_result_line("test result: ok. 2 passed"), None);
    }

    #[test]
    fn result_line_real_libtest_tail_with_finished_in() {
        assert_eq!(
            parse_result_line(
                "test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s"
            ),
            Some((3, 0, 1))
        );
        // A count label with an unparseable number is undetermined, not 0.
        assert_eq!(
            parse_result_line("test result: ok. x passed; 0 failed; 1 ignored"),
            None
        );
    }

    #[test]
    fn unparseable_log_is_undetermined_not_zero() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(&p, "no libtest here\n").unwrap();
        let (tests, counts) = parse_test_log(&p);
        assert_eq!(tests, json!({}));
        assert_eq!(counts["determined"], json!(false));
        assert!(counts.get("passed").is_none());
    }

    #[test]
    fn counts_sum_over_binaries_and_collision_keeps_the_worst() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(
            &p,
            "test x ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored\n\
             test x ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 2 ignored\n",
        )
        .unwrap();
        let (tests, counts) = parse_test_log(&p);
        assert_eq!(tests["x"], json!("fail"));
        assert_eq!(counts["passed"], json!(1));
        assert_eq!(counts["failed"], json!(1));
        assert_eq!(counts["ignored"], json!(2));
        assert_eq!(counts["determined"], json!(true));
    }

    fn parse_str(log: &str) -> (Value, Value) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(&p, log).unwrap();
        parse_test_log(&p)
    }

    #[test]
    fn framed_single_nested_block_in_capture_is_not_a_result() {
        let (tests, counts) = parse_str(
            "running 1 test\ntest real ... FAILED\n\nfailures:\n\n---- real stdout ----\n\
             nested fixture said:\ntest t ... FAILED\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n\nfailures:\n    real\n\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(tests, json!({"real": "fail"}));
        assert_eq!(counts["determined"], json!(true));
        assert_eq!(counts["failed"], json!(1));
        assert_eq!(counts["result_lines"], json!(1));
    }

    #[test]
    fn framed_two_nested_blocks_do_not_reopen() {
        let (tests, counts) = parse_str(
            "running 1 test\ntest g ... FAILED\n\nfailures:\n\n---- g stdout ----\n\
             running 1 test\ntest t ... FAILED\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n\
             running 2 tests\ntest u ... FAILED\ntest v ... ok\n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored\n\n\
             failures:\n    g\n\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(tests, json!({"g": "fail"}));
        assert_eq!(counts["passed"], json!(0));
        assert_eq!(counts["failed"], json!(1));
        assert_eq!(counts["result_lines"], json!(1));
    }

    #[test]
    fn framed_nested_result_before_the_real_test_line_keeps_the_real_failure() {
        let (tests, counts) = parse_str(
            "running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored\n\
             test g ... FAILED\n\nfailures:\n\n---- g stdout ----\nboom\n\n\
             failures:\n    g\n\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(tests, json!({"g": "fail"}));
        assert_eq!(counts["passed"], json!(0));
        assert_eq!(counts["failed"], json!(1));
    }

    #[test]
    fn framed_multiple_real_blocks_lib_integration_doc() {
        let (tests, counts) = parse_str(
            "     Running unittests src/lib.rs (target/debug/deps/x-1)\n\n\
             running 2 tests\ntest a ... ok\ntest b ... ignored, slow\n\n\
             test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n\n\
             \x20    Running tests/it.rs (target/debug/deps/it-2)\n\n\
             running 2 tests\ntest c ... ok\ntest p - should panic ... FAILED\n\n\
             failures:\n\n---- p stdout ----\nnote: test did not panic as expected\n\
             test nested ... FAILED\n\nfailures:\n    p\n\n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n\n\
             \x20  Doc-tests x\n\n\
             running 1 test\ntest src/lib.rs - f (line 3) ... ok\n\n\
             test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\n\
             running 0 tests\n\n\
             test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(
            tests,
            json!({"a": "pass", "b": "ignored", "c": "pass", "p - should panic": "fail",
                   "src/lib.rs - f (line 3)": "pass"})
        );
        assert_eq!(counts["determined"], json!(true));
        assert_eq!(counts["passed"], json!(3));
        assert_eq!(counts["failed"], json!(1));
        assert_eq!(counts["ignored"], json!(1));
        assert_eq!(counts["result_lines"], json!(4));
    }

    #[test]
    fn unframed_log_keeps_the_conservative_scan() {
        let (tests, counts) = parse_str(
            "test real ... FAILED\nfailures:\ntest t ... FAILED\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n",
        );
        assert_eq!(tests, json!({"real": "fail", "t": "fail"}));
        assert_eq!(counts["determined"], json!(true));
        assert_eq!(counts["failed"], json!(2));
    }

    #[test]
    fn non_reconciling_counts_fall_back_to_conservative_scan() {
        // `running 2 tests` but the terminator counts disagree with the lines.
        let (tests, counts) = parse_str(
            "running 2 tests\ntest a ... ok\ntest b ... FAILED\n\n\
             test result: ok. 2 passed; 0 failed; 0 ignored\n",
        );
        assert_eq!(tests, json!({"a": "pass", "b": "fail"}));
        assert_eq!(counts["determined"], json!(false));
        assert!(counts.get("passed").is_none());
    }

    #[test]
    fn displaced_real_failure_falls_back_and_keeps_both_names() {
        // A nested line took the only per-test slot; the real failure comes
        // after it. The failure list names `g`, not `t`: no reconcile, so the
        // conservative scan keeps g (and the phantom t, restrictive).
        let (tests, counts) = parse_str(
            "running 1 test\ntest t ... FAILED\ntest g ... FAILED\n\nfailures:\n    g\n\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n",
        );
        assert_eq!(tests, json!({"g": "fail", "t": "fail"}));
        assert_eq!(counts["determined"], json!(false));
    }

    #[test]
    fn unclosed_block_falls_back() {
        let (tests, counts) = parse_str("running 2 tests\ntest a ... ok\ntest b ... FAILED\n");
        assert_eq!(tests, json!({"a": "pass", "b": "fail"}));
        assert_eq!(counts["determined"], json!(false));
    }

    #[test]
    fn missing_log_is_undetermined() {
        let (_, counts) = parse_test_log(Path::new("/nonexistent/tdd-log"));
        assert_eq!(counts["determined"], json!(false));
    }

    #[test]
    fn outside_a_git_repo_every_component_is_undetermined() {
        let d = tempfile::tempdir().unwrap();
        let v = tree_identity(d.path(), ".tdd");
        assert_eq!(v["determined"], json!(false));
        for k in ["head", "diff_hash", "tree"] {
            assert!(v[k].is_null(), "{k}: {v}");
            assert!(v["undetermined"][k].is_string(), "{k}: {v}");
        }
    }
}
