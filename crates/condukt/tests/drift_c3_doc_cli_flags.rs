// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Drift audit C3: instruction text (skills/**/*.md, agents/*.md) must not
//! document `condukt ... --flag` invocations whose flag the real binary's
//! `<subcommand> --help` does not list.
//!
//! General scanner, not a one-off: every `condukt <sub...> --flag` found in a
//! code span or fenced block is resolved against the built binary's help.
//! Known defect it must find: condukt-loop SKILL.md documents `--max-iters`
//! and `--run` for `condukt loop`, which only takes
//! --module/--iteration/--prev-failures.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn help_of(
    path: &[String],
    home: &Path,
    cache: &mut HashMap<Vec<String>, Option<String>>,
) -> Option<String> {
    if let Some(v) = cache.get(path) {
        return v.clone();
    }
    let out = Command::new(bin())
        .args(path)
        .arg("--help")
        .env("HOME", home)
        .output()
        .expect("spawn condukt --help");
    let v = if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    };
    cache.insert(path.to_vec(), v.clone());
    v
}

/// Names under the `Commands:` section of clap help.
fn subcommands(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_cmds = false;
    for l in help.lines() {
        if l.trim_end() == "Commands:" {
            in_cmds = true;
            continue;
        }
        if in_cmds {
            if l.starts_with("  ") && !l.starts_with("   ") {
                if let Some(n) = l.split_whitespace().next() {
                    out.insert(n.to_string());
                }
            } else if !l.trim().is_empty() && !l.starts_with(' ') {
                in_cmds = false;
            }
        }
    }
    out
}

/// Every `--long` flag spelled in a help text.
fn flags_in(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in help.lines() {
        // option rows start with whitespace then `-x, --long` or `--long`
        let t = l.trim_start();
        if !t.starts_with('-') {
            continue;
        }
        for tok in t.split(|c: char| c == ',' || c.is_whitespace()) {
            if let Some(rest) = tok.strip_prefix("--") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !name.is_empty() {
                    out.insert(format!("--{name}"));
                }
            }
        }
    }
    out.insert("--help".into());
    out.insert("--version".into());
    out
}

fn md_files(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                md_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
}

/// Candidate command strings with their 1-based start line: fenced-block
/// lines (continuations joined) and inline `code spans`.
fn candidates(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut pending: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            if let Some(p) = pending.take() {
                out.push(p);
            }
            continue;
        }
        if fenced {
            let l = line.trim();
            let l = l.strip_prefix("$ ").unwrap_or(l);
            let (start, mut acc) = pending.take().unwrap_or((n, String::new()));
            if !acc.is_empty() {
                acc.push(' ');
            }
            if let Some(stripped) = l.strip_suffix('\\') {
                acc.push_str(stripped.trim_end());
                pending = Some((start, acc));
            } else {
                acc.push_str(l);
                out.push((start, acc));
            }
        } else {
            // inline code spans (single backtick pairs)
            let mut parts = line.split('`');
            parts.next();
            let mut inside = true;
            for seg in parts {
                if inside && !seg.is_empty() {
                    out.push((n, seg.to_string()));
                }
                inside = !inside;
            }
        }
    }
    out
}

/// `<server|client|e2e>` is a placeholder, not a pipe: blank the `|` inside `<...>`.
fn mask_placeholders(s: &str) -> String {
    let mut depth = 0u32;
    s.chars()
        .map(|c| match c {
            '<' => {
                depth += 1;
                c
            }
            '>' if depth > 0 => {
                depth -= 1;
                c
            }
            '|' if depth > 0 => '/',
            _ => c,
        })
        .collect()
}

struct Finding {
    loc: String,
    invocation: String,
    flag: String,
}

#[test]
fn documented_condukt_flags_exist_in_the_binary() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = std::env::temp_dir().join(format!("condukt-c3-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut files = Vec::new();
    md_files(&crate_dir.join("skills"), &mut files);
    md_files(&crate_dir.join("agents"), &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no instruction files found (scanner is blind)"
    );

    let mut cache = HashMap::new();
    let top = help_of(&[], &home, &mut cache).expect("condukt --help must work");
    let top_cmds = subcommands(&top);
    assert!(
        top_cmds.contains("loop"),
        "help parser broken: no `loop` in {top_cmds:?}"
    );

    let mut findings: Vec<Finding> = Vec::new();
    let mut checked = 0usize;
    for f in &files {
        let rel = f.strip_prefix(crate_dir).unwrap().display().to_string();
        let text = std::fs::read_to_string(f).unwrap();
        for (line, cand) in candidates(&text) {
            // split on shell operators so each segment is one command
            let cand = mask_placeholders(&cand)
                .replace("&&", "\u{1}")
                .replace("||", "\u{1}");
            for seg in cand.split(['|', ';', '\u{1}']) {
                let toks: Vec<&str> = seg.split_whitespace().collect();
                for (ti, t) in toks.iter().enumerate() {
                    if *t != "condukt" {
                        continue;
                    }
                    // the word before must not make this prose/path (`plugin:condukt`)
                    let rest = &toks[ti + 1..];
                    let mut path: Vec<String> = Vec::new();
                    let mut help = top.clone();
                    let mut k = 0;
                    while k < rest.len() {
                        let w = rest[k];
                        let subs = subcommands(&help);
                        if subs.contains(w) {
                            path.push(w.to_string());
                            match help_of(&path, &home, &mut cache) {
                                Some(h) => help = h,
                                None => break,
                            }
                            k += 1;
                        } else {
                            break;
                        }
                    }
                    if path.is_empty() {
                        continue; // not a recognised subcommand: prose, not an invocation
                    }
                    checked += 1;
                    let valid = flags_in(&help);
                    for w in &rest[k..] {
                        let w = w.trim_matches(|c| {
                            c == '['
                                || c == ']'
                                || c == '('
                                || c == ')'
                                || c == ','
                                || c == '"'
                                || c == '\''
                        });
                        if !w.starts_with("--") || w.len() < 3 {
                            continue;
                        }
                        let name: String = w
                            .chars()
                            .take_while(|c| *c == '-' || c.is_ascii_alphanumeric())
                            .collect();
                        if name.len() < 3
                            || !name.chars().nth(2).is_some_and(|c| c.is_ascii_alphabetic())
                        {
                            continue;
                        }
                        if !valid.contains(&name) {
                            findings.push(Finding {
                                loc: format!("{rel}:{line}"),
                                invocation: format!("condukt {} ...", path.join(" ")),
                                flag: name,
                            });
                        }
                    }
                }
            }
        }
    }
    assert!(
        checked > 20,
        "scanner checked only {checked} invocations: it is blind"
    );
    if !findings.is_empty() {
        let mut msg = format!(
            "{} documented flag(s) the binary does not accept ({checked} invocations checked):\n",
            findings.len()
        );
        for f in &findings {
            msg.push_str(&format!(
                "  {}  `{}`  unknown flag {}\n",
                f.loc, f.invocation, f.flag
            ));
        }
        panic!("{msg}");
    }
}

/// The binary's `condukt loop` JSON carries `"module"` (src/main.rs); the
/// SKILL's documented output example must list every key the binary emits.
#[test]
fn loop_skill_output_example_lists_every_emitted_key() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(crate_dir.join("skills/condukt-loop/SKILL.md")).unwrap();
    let src = std::fs::read_to_string(crate_dir.join("src/main.rs")).unwrap();
    let emitted = src.find("Command::Loop {").and_then(|i| {
        let tail = &src[i..];
        tail.find("serde_json::json!({").map(|j| &tail[j..])
    });
    let block = emitted.expect("loop json! block found in main.rs");
    let block = &block[..block.find("});").expect("json! end")];
    let keys: Vec<&str> = block
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix('"')?.split('"').next()
        })
        .collect();
    assert!(keys.len() >= 6, "key extraction broke: {keys:?}");
    let missing: Vec<&&str> = keys
        .iter()
        .filter(|k| !skill.contains(&format!("\"{k}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "skills/condukt-loop/SKILL.md output example omits keys the binary emits: {missing:?}"
    );
}
