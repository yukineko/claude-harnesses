//! `webqa weaken-check` — detect test-weakening edits to Playwright specs in a
//! unified diff.
//!
//! Three-valued outcome (never a bool): [`Outcome::Clean`] (exit 0),
//! [`Outcome::Findings`] (exit 1), [`Outcome::Undetermined`] (exit 2). Every
//! error path (unreadable file, `git` failure, non-empty input without any
//! parseable diff header) is `Undetermined`; nothing maps an error to clean.

use std::path::Path;
use std::process::Command;

/// One detected weakening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub rule: &'static str,
    pub detail: String,
}

#[derive(Debug)]
pub enum Outcome {
    Clean,
    Findings(Vec<Finding>),
    Undetermined(String),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Clean => 0,
            Outcome::Findings(_) => 1,
            Outcome::Undetermined(_) => 2,
        }
    }
}

/// Run the check against a diff file, or `git diff --cached` when `None`.
pub fn run(diff_file: Option<&Path>) -> Outcome {
    let text = match diff_file {
        Some(p) => match std::fs::read(p) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => return Outcome::Undetermined(format!("cannot read {}: {e}", p.display())),
        },
        None => match Command::new("git").args(["diff", "--cached"]).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(o) => {
                return Outcome::Undetermined(format!(
                    "`git diff --cached` exited with {:?}: {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
            Err(e) => return Outcome::Undetermined(format!("cannot run git: {e}")),
        },
    };
    check_diff(&text)
}

/// Is this path a Playwright spec we inspect?
pub fn is_spec_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    ["spec", "test"].iter().any(|k| {
        ["ts", "js", "tsx", "mjs"]
            .iter()
            .any(|e| name.ends_with(&format!(".{k}.{e}")))
    })
}

#[derive(Default)]
struct Hunk {
    added: Vec<String>,
    removed: Vec<String>,
}

struct FileDiff {
    path: String,
    hunks: Vec<Hunk>,
}

fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut in_hunk = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            in_hunk = false;
            let path = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest);
            files.push(FileDiff {
                path: path.trim_matches('"').to_string(),
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(cur) = files.last_mut() else {
            continue;
        };
        if line.starts_with("@@") {
            in_hunk = true;
            cur.hunks.push(Hunk::default());
            continue;
        }
        if !in_hunk {
            if let Some(p) = line.strip_prefix("+++ b/") {
                cur.path = p.to_string();
            }
            continue;
        }
        let Some(h) = cur.hunks.last_mut() else {
            continue;
        };
        if let Some(a) = line.strip_prefix('+') {
            h.added.push(a.to_string());
        } else if let Some(r) = line.strip_prefix('-') {
            h.removed.push(r.to_string());
        }
    }
    files
}

pub fn check_diff(text: &str) -> Outcome {
    if text.trim().is_empty() {
        return Outcome::Clean;
    }
    let files = parse(text);
    if files.is_empty() {
        return Outcome::Undetermined(
            "input is non-empty but contains no `diff --git` header; not a parseable unified diff"
                .to_string(),
        );
    }
    let mut findings = Vec::new();
    for f in files.iter().filter(|f| is_spec_path(&f.path)) {
        for h in &f.hunks {
            check_hunk(&f.path, h, &mut findings);
        }
    }
    if findings.is_empty() {
        Outcome::Clean
    } else {
        Outcome::Findings(findings)
    }
}

/// Numbers following `timeout:` or `setTimeout(` on a line.
fn timeouts(line: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for key in ["timeout:", "setTimeout("] {
        let mut rest = line;
        while let Some(i) = rest.find(key) {
            rest = &rest[i + key.len()..];
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '_')
                .filter(|c| *c != '_')
                .collect();
            if let Ok(n) = digits.parse::<u64>() {
                out.push(n);
            }
        }
    }
    out
}

fn count(lines: &[String], needle: &str) -> usize {
    lines.iter().map(|l| l.matches(needle).count()).sum()
}

const WEAK_MATCHERS: [&str; 4] = [
    ".toBeTruthy(",
    ".toBeDefined(",
    ".not.toBeNull(",
    ".not.toBeUndefined(",
];

fn check_hunk(path: &str, h: &Hunk, out: &mut Vec<Finding>) {
    let mut push = |rule: &'static str, detail: String| {
        out.push(Finding {
            path: path.to_string(),
            rule,
            detail,
        })
    };
    // A line that merely moved (identical in removed and added) is not new.
    let is_new = |l: &String| !h.removed.iter().any(|r| r.trim() == l.trim());

    for l in h.added.iter().filter(|l| is_new(l)) {
        if [".skip(", ".skip.", ".fixme(", ".fixme."]
            .iter()
            .any(|k| l.contains(k))
        {
            push("skip-added", format!("added: {}", l.trim()));
        }
        if l.contains(".only(") || l.contains(".only.") {
            push("only-added", format!("added: {}", l.trim()));
        }
        if l.contains("waitForTimeout(") {
            push("wait-timeout-added", format!("added: {}", l.trim()));
        }
    }

    let (exp_r, exp_a) = (count(&h.removed, "expect("), count(&h.added, "expect("));
    if exp_r > exp_a {
        push(
            "expect-removed",
            format!(
                "{} expect() call(s) removed, {} added in this hunk",
                exp_r, exp_a
            ),
        );
    }

    let rem_t: Vec<u64> = h.removed.iter().flat_map(|l| timeouts(l)).collect();
    let add_t: Vec<u64> = h.added.iter().flat_map(|l| timeouts(l)).collect();
    if let (Some(r), Some(a)) = (rem_t.iter().max(), add_t.iter().max()) {
        if a > r {
            push("timeout-raised", format!("timeout raised {r} -> {a}"));
        }
    }

    if h.removed.iter().any(|l| l.contains("expect(")) {
        for l in &h.added {
            if let Some(w) = WEAK_MATCHERS.iter().find(|w| l.contains(**w)) {
                if !h.removed.iter().any(|r| r.contains(*w)) {
                    push(
                        "matcher-weakened",
                        format!(
                            "weak matcher `{}` replaces stronger assertion",
                            w.trim_matches(['.', '('])
                        ),
                    );
                }
            }
        }
    }
}

/// Print findings / reason and return the process exit code.
pub fn report(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Clean => {}
        Outcome::Findings(fs) => {
            for f in fs {
                println!("{}: {}: {}", f.path, f.rule, f.detail);
            }
        }
        Outcome::Undetermined(why) => {
            eprintln!("webqa weaken-check: cannot determine: {why}");
        }
    }
    outcome.exit_code()
}
