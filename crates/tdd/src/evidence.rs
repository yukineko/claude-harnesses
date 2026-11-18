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

    let tree = content_tree(&top, &specs);
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

/// `git add -A` of the work tree (untracked included, `.gitignore` honoured,
/// `specs` as pathspecs) into a throwaway index, then `git write-tree`. The
/// real index is never written. A gitlink (submodule / nested repo) records
/// only its HEAD commit, so the id would not see edits inside it: undetermined.
fn content_tree(top: &Path, specs: &[String]) -> Result<String, String> {
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
    run(&["add", "-A"], specs)?;
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

/// Per-test results and counts of one run, from its full log.
///
/// * `tests`: name -> `"pass"` / `"fail"` / `"ignored"` from libtest
///   `test <name> ... ok|FAILED|ignored` lines.
/// * `counts`: the sum of every libtest `test result:` line (one per test
///   binary). `determined: false` with a reason when no such line could be
///   parsed or the log could not be read: "could not count" is not `0 passed`.
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
    let mut tests: BTreeMap<String, T> = BTreeMap::new();
    let (mut p, mut f, mut i, mut results) = (0u64, 0u64, 0u64, 0u64);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                let tests: serde_json::Map<String, Value> = tests
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v.as_str())))
                    .collect();
                return (
                    Value::Object(tests),
                    json!({"determined": false, "reason": format!("run log read failed midway: {e}")}),
                );
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some((name, t)) = parse_test_line(line) {
            tests
                .entry(name)
                .and_modify(|old| {
                    if t.rank() > old.rank() {
                        *old = t;
                    }
                })
                .or_insert(t);
        } else if let Some((a, b, c)) = parse_result_line(line) {
            p = p.saturating_add(a);
            f = f.saturating_add(b);
            i = i.saturating_add(c);
            results += 1;
        }
    }
    let tests: serde_json::Map<String, Value> = tests
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.as_str())))
        .collect();
    let counts = if results == 0 {
        json!({
            "determined": false,
            "reason": "no libtest `test result:` line in the run log (not a cargo test run, or it \
                       died before printing one); this is NOT 0 passed",
        })
    } else {
        json!({
            "determined": true,
            "passed": p,
            "failed": f,
            "ignored": i,
            "result_lines": results,
        })
    };
    (Value::Object(tests), counts)
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
