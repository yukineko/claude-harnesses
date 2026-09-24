#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression test for CLAUDE.md §6 ("反証にも同じ立証責任を課す"): no adversarial
//! verifier/skeptic prompt shipped in a skill or agent doc may instruct a
//! default-REFUTED verdict, and the condukt skeptic-panel prompt specifically
//! must not fold "could not break it" into `pass`. "I could not trace a
//! permissive path" is UNDETERMINED, not a verdict; it must resolve to
//! UNVERIFIED (finding verifiers) or `abstain` (condukt skeptic ballots),
//! never to REFUTED-the-finding or to `pass`-the-implementation.
//!
//! It was observed RED against the pre-fix text of
//! `crates/condukt/skills/condukt/SKILL.md` (the skeptic-panel step, which
//! instructed "既定 REFUTED ... 崩せなければ pass"; backlog 2a3b033c) and pins
//! the anti-pattern so a corrected prompt cannot silently regress.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Recursively collect every `*.md` file under `dir`. Fails closed: any I/O
/// error while walking (unreadable dir entry, broken symlink, permission
/// denied, ...) is propagated rather than silently skipped, so a scan that
/// cannot see part of the tree cannot masquerade as a scan that saw nothing
/// wrong there.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_md_files(&path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scan every `crates/*/skills/**/*.md` and `crates/*/agents/**/*.md` file in
/// the workspace. FAIL CLOSED: an empty result (zero files found) is treated
/// as a scan failure by the caller, never as "nothing to flag".
fn scan_skill_and_agent_docs() -> Vec<PathBuf> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve (../.. from CARGO_MANIFEST_DIR)");
    let crates_dir = workspace_root.join("crates");

    let mut files = Vec::new();
    let crate_entries = fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", crates_dir.display()));
    for crate_entry in crate_entries {
        let crate_entry = crate_entry.expect("failed to read a crates/* dir entry");
        if !crate_entry
            .file_type()
            .expect("failed to stat crates/* entry")
            .is_dir()
        {
            continue;
        }
        let crate_path = crate_entry.path();

        collect_md_files(&crate_path.join("skills"), &mut files)
            .unwrap_or_else(|e| panic!("I/O error scanning {}/skills: {e}", crate_path.display()));
        collect_md_files(&crate_path.join("agents"), &mut files)
            .unwrap_or_else(|e| panic!("I/O error scanning {}/agents: {e}", crate_path.display()));
    }

    assert!(
        !files.is_empty(),
        "scan_skill_and_agent_docs found ZERO files under crates/*/skills or crates/*/agents; \
         an empty scan must not be treated as a passing scan (fail closed, CLAUDE.md §3)"
    );

    files
}

/// Read a file's content, failing the test (not silently skipping it) on any
/// read error.
fn read_to_string_or_fail(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// A single offending "default REFUTED"-shaped instruction.
struct Offense {
    file: PathBuf,
    line_no: usize,
    line: String,
}

/// Regex matching the various "既定 REFUTED" / "default REFUTED" spellings
/// called out in CLAUDE.md §6, case-insensitively (the `既定`/`は` side has no
/// case to fold, but this also covers `default`/`DEFAULT`/`Default`).
fn default_refuted_pattern() -> Regex {
    Regex::new(r"(?i)(既定\s*は?\s*REFUTED|default\s*は?\s*REFUTED|default\s+is\s+REFUTED)")
        .expect("static regex must compile")
}

/// Decide whether a matched "default REFUTED" span, found at byte offset
/// `match_end` in `line`, is an ALLOWED mention (negated, or quoted purely as
/// the cited anti-pattern) rather than an actual instruction to default to
/// REFUTED.
///
/// Rule (kept deliberately simple, matches CLAUDE.md §6's own two citation
/// styles):
/// - immediately followed (after optional whitespace) by `ではない` / `not` /
///   `never` → the sentence is negating the pattern, allowed.
/// - immediately followed by a closing quote (`」`, `"`, `'`) → the phrase is
///   being quoted as a short noun phrase (the thing being named and rejected,
///   e.g. `「既定 REFUTED」は ... fail-open と同じ形`), allowed.
///
/// A match that is instead followed by more instruction text (e.g. a `。`
/// that continues into "...反証できたら refute、崩せなければ pass...") is a
/// live instruction, not a citation, and is NOT allowed.
fn is_allowed_mention(line: &str, match_end: usize) -> bool {
    let after = line[match_end..].trim_start();
    let after_lower = after.to_lowercase();
    after.starts_with("ではない")
        || after.starts_with('」')
        || after.starts_with('"')
        || after.starts_with('\'')
        || after_lower.starts_with("not")
        || after_lower.starts_with("never")
}

#[test]
fn no_skill_or_agent_doc_instructs_default_refuted() {
    let files = scan_skill_and_agent_docs();

    let condukt_skill = files
        .iter()
        .any(|p| p.ends_with("crates/condukt/skills/condukt/SKILL.md"));
    let continuous_audit_skill = files
        .iter()
        .any(|p| p.ends_with("crates/overwatch/skills/continuous-audit/SKILL.md"));
    assert!(
        condukt_skill,
        "expected crates/condukt/skills/condukt/SKILL.md to be among the scanned files"
    );
    assert!(
        continuous_audit_skill,
        "expected crates/overwatch/skills/continuous-audit/SKILL.md to be among the scanned files"
    );

    let pattern = default_refuted_pattern();
    let mut offenses: Vec<Offense> = Vec::new();

    for file in &files {
        let content = read_to_string_or_fail(file);
        for (idx, line) in content.lines().enumerate() {
            for mat in pattern.find_iter(line) {
                if !is_allowed_mention(line, mat.end()) {
                    offenses.push(Offense {
                        file: file.clone(),
                        line_no: idx + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
    }

    assert!(
        offenses.is_empty(),
        "found {} skill/agent doc line(s) instructing a default-REFUTED verdict \
         (CLAUDE.md §6: \"default は REFUTED\" is the SAME fail-open shape as the \
         gates this repo audits; \"could not trace a permissive path\" is \
         UNDETERMINED, must resolve to UNVERIFIED/abstain, never to REFUTED):\n{}",
        offenses.len(),
        offenses
            .iter()
            .map(|o| format!("  {}:{}: {}", o.file.display(), o.line_no, o.line.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn condukt_skeptic_panel_does_not_map_unbroken_to_pass() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve");
    let skill_path = workspace_root.join("crates/condukt/skills/condukt/SKILL.md");
    let content = read_to_string_or_fail(&skill_path);
    let lines: Vec<&str> = content.lines().collect();

    // (a) No line may map "could not break it" to `pass`, in either spacing.
    let mut break_to_pass_offenses = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.contains("崩せなければ pass") || line.contains("崩せなければpass") {
            break_to_pass_offenses.push(format!(
                "  {}:{}: {}",
                skill_path.display(),
                idx + 1,
                line.trim()
            ));
        }
    }
    assert!(
        break_to_pass_offenses.is_empty(),
        "{} maps \"could not break it\" (崩せなければ) directly to `pass`; that is \
         UNDETERMINED, not a verdict, and must resolve to `abstain`, not `pass`:\n{}",
        skill_path.display(),
        break_to_pass_offenses.join("\n")
    );

    // (b) The skeptic ballot JSON line must exist, and somewhere in the ~6
    // lines immediately before it, abstain must be stated as the DEFAULT
    // outcome (a line mentioning both `abstain` and `既定`), not merely one
    // of three options with no default posture.
    let ballot_needle = r#""ballot":"refute|pass|abstain""#;
    let ballot_line_idx = lines
        .iter()
        .position(|line| line.contains(ballot_needle))
        .unwrap_or_else(|| {
            panic!(
                "{} does not contain the expected skeptic ballot JSON line ({ballot_needle:?}); \
                 cannot verify the abstain-as-default contract",
                skill_path.display()
            )
        });

    let window_start = ballot_line_idx.saturating_sub(6);
    let window = &lines[window_start..ballot_line_idx];
    let states_abstain_as_default = window.iter().any(|line| {
        let lower = line.to_lowercase();
        lower.contains("abstain") && line.contains("既定")
    });

    assert!(
        states_abstain_as_default,
        "{}: none of the {} line(s) immediately before the skeptic ballot JSON line \
         (line {}) state `abstain` as the 既定 (default) outcome when neither refute \
         nor pass can be grounded in code; \"判断不能なら abstain\" listed as a third \
         option is not the same as abstain being the stated default:\n{}",
        skill_path.display(),
        window.len(),
        ballot_line_idx + 1,
        window
            .iter()
            .enumerate()
            .map(|(i, l)| format!(
                "  {}:{}: {}",
                skill_path.display(),
                window_start + i + 1,
                l.trim()
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
