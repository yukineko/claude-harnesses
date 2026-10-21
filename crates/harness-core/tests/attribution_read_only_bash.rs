#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `attribute_own_writes`: a file named in a Bash command counts as written by
//! the session UNLESS every mention sits in a statement made only of read-only
//! commands. Unparseable constructs stay conservative (attributed).
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)); only the public
//! contract of `attribute_own_writes` was read.

use harness_core::attribution::{attribute_own_writes, Attribution};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);
const F: &str = "src/a.js";

struct Fx {
    root: PathBuf,
    transcript: PathBuf,
}

type Use = (&'static str, serde_json::Value);

/// Build a repo dir with `src/a.js` and a transcript whose tool_uses come from
/// `uses(root)`. No `<stem>/subagents` dir exists (a missing dir is "no
/// sidechains", checked by the sanity assertion in each verdict).
fn fx(uses: impl Fn(&Path) -> Vec<Use>) -> Fx {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("attr-ro-bash-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(F), "x\n").unwrap();
    let root = root.canonicalize().unwrap();
    let tdir = base.join("transcripts");
    std::fs::create_dir_all(&tdir).unwrap();
    let transcript = tdir.join("sess.jsonl");
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    for (name, input) in uses(&root) {
        body.push_str(
            &serde_json::json!({
                "type": "assistant",
                "cwd": root.to_string_lossy(),
                "isSidechain": false,
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": name, "input": input}
                ]}
            })
            .to_string(),
        );
        body.push('\n');
    }
    std::fs::write(&transcript, body).unwrap();
    Fx { root, transcript }
}

/// Ok(true) = kept (attributed), Ok(false) = excluded, Err = anything else
/// (including Undetermined, which would make a test vacuous).
fn verdict(f: &Fx) -> Result<bool, String> {
    match attribute_own_writes(&f.root, f.transcript.to_str().unwrap(), &[F.to_string()]) {
        Attribution::Narrowed { keep, excluded } => {
            if keep.iter().any(|k| k == F) && !excluded.iter().any(|k| k == F) {
                Ok(true)
            } else if excluded.iter().any(|k| k == F) && keep.is_empty() {
                Ok(false)
            } else {
                Err(format!("inconsistent keep={keep:?} excluded={excluded:?}"))
            }
        }
        Attribution::Undetermined { why } => Err(format!("Undetermined: {why}")),
    }
}

fn kept(cmd: &str) -> Result<bool, String> {
    let cmd = cmd.to_string();
    verdict(&fx(move |_| {
        vec![("Bash", serde_json::json!({ "command": cmd }))]
    }))
}

fn assert_all(cmds: &[&str], want_kept: bool) {
    let bad: Vec<String> = cmds
        .iter()
        .filter(|c| kept(c) != Ok(want_kept))
        .map(|c| format!("{c:?} -> {:?}", kept(c)))
        .collect();
    assert!(
        bad.is_empty(),
        "expected kept={want_kept}; mismatches: {bad:#?}"
    );
}

#[test]
fn group1_read_only_mentions_do_not_attribute() {
    assert_all(
        &[
            "git diff src/a.js",
            "grep -n foo src/a.js | head -3",
            "cat src/a.js 2>/dev/null",
            "ls src/a.js >/dev/null 2>&1",
            "cd /some/dir && cat src/a.js",
            "tdd skip --reason \"src/a.js predates this session\"",
            "reviewgate skip --reason \"src/a.js not mine\" && tdd skip --reason \"src/a.js\"",
            "~/.claude/plugins/cache/yukineko/tdd/0.1.44/bin/tdd skip --reason \"src/a.js predates this session\"",
            "grep x src/a.js; curl -sI https://example.com | grep -i location",
            "sed -i s/x/y/ other.md; grep -n x src/a.js",
        ],
        false,
    );
}

#[test]
fn group2_writing_mentions_attribute() {
    assert_all(
        &[
            "sed -i s/x/y/ src/a.js",
            "echo hi > src/a.js",
            "cat x >> src/a.js",
            "cp b src/a.js",
            "grep -l foo src/a.js | xargs sed -i s/x/y/",
            "python3 script.py src/a.js",
            "for f in src/a.js; do sed -i s/x/y/ \"$f\"; done",
            "F=src/a.js; sed -i s/x/y/ $F",
        ],
        true,
    );
}

#[test]
fn group3_unparseable_constructs_stay_conservative() {
    assert_all(
        &[
            "cat $(echo src/a.js)",
            "cat `echo src/a.js`",
            "python3 - <<'EOF'\nopen(\"src/a.js\",\"w\")\nEOF",
            "cat \"src/a.js",
        ],
        true,
    );
}

#[test]
fn group4_edit_and_write_tool_use_still_attribute() {
    for tool in ["Edit", "Write"] {
        let f = fx(|root| {
            vec![(
                tool,
                serde_json::json!({ "file_path": root.join(F).to_string_lossy() }),
            )]
        });
        assert_eq!(verdict(&f), Ok(true), "{tool} must attribute");
    }
}

#[test]
fn control_no_mention_is_excluded_not_undetermined() {
    // Guards the fixture: a transcript that never names the file must be
    // Narrowed/excluded, proving group 1's `false` is not an Undetermined.
    assert_eq!(kept("echo unrelated"), Ok(false));
}
