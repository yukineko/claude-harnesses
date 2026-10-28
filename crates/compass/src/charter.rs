//! The `charter` artifact (DESIGN §4): the project's living "north star +
//! definition-of-done + gap" one-pager, stored at `.compass/charter.md`.
//!
//! # Serialization scheme
//!
//! A human-readable Markdown with deterministically parseable sections. Each
//! field is a level-2 heading whose text is the field name, followed by that
//! field's body until the next `## ` heading (or EOF):
//!
//! ```markdown
//! ## north_star
//! One or two lines of why this project ultimately exists.
//!
//! ## definition_of_done
//! - observable done condition A
//! - observable done condition B
//!
//! ## measuring_stick
//! Defensibility x closeness-to-goal / cost.
//!
//! ## current_gap
//! Goal minus current-state summary.
//!
//! ## next_action
//! The first physical step to take on resume.
//!
//! ## parked
//! - pointer to a parked item (written by the skill; route stores nothing)
//! ```
//!
//! Scalar fields (`north_star`, `measuring_stick`, `current_gap`,
//! `next_action`) keep their body verbatim (trimmed). List fields
//! (`definition_of_done`, `parked`) are `- ` bullets, one item per line.
//!
//! The scheme is chosen for lossless round-tripping of the *structured* fields:
//! `save` emits the headings in a fixed order and `load` keys off the heading
//! text, so unknown headings are ignored and field order is normalized on save.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The structured charter. See DESIGN §4 for field semantics.
///
/// Serde derives let the `/compass` skill compose a sharpened charter as JSON
/// and persist it via `compass charter --write <JSON>`; every field defaults so
/// a partial JSON object still deserializes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Charter {
    /// 1-2 lines: what this project is ultimately for. (re-carved when blurry)
    pub north_star: String,
    /// Observable done conditions (same vocabulary as condukt `done_criteria`).
    pub definition_of_done: Vec<String>,
    /// What we measure the next move by (DESIGN §7).
    pub measuring_stick: String,
    /// Goal minus current-state summary (regenerated each round).
    pub current_gap: String,
    /// First physical step on resume (written by the breadcrumb step).
    pub next_action: String,
    /// Pointers to parked items, written by the skill via `charter --write`
    /// (route itself stores nothing — user ruling 2026-10-27).
    pub parked: Vec<String>,
}

impl Charter {
    /// `.compass/charter.md` under the project root.
    pub fn project_path(root: &Path) -> PathBuf {
        root.join(".compass").join("charter.md")
    }

    /// Load a charter from an explicit `charter.md` path.
    ///
    /// A file that contains none of the charter's `## <field>` headings (e.g.
    /// garbage, an empty file, or YAML-style front matter this scheme does not
    /// use) is an error, not an empty charter: it cannot be determined what
    /// the charter says, and `Charter::default()` would read downstream as
    /// "no gap" (CLAUDE.md section 3: cannot-determine resolves to the
    /// restrictive side).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading charter {}", path.display()))?;
        if !has_known_heading(&text) {
            anyhow::bail!(
                "charter {} is unparseable: it has none of the `## <field>` headings ({})",
                path.display(),
                FIELDS.join(", ")
            );
        }
        Ok(Charter::parse(&text))
    }

    /// Save a charter to an explicit `charter.md` path, creating parent dirs.
    // Used by tests and the not-yet-wired breadcrumb/route commands (scaffold).
    #[allow(dead_code)]
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, self.to_markdown())
            .with_context(|| format!("writing charter {}", path.display()))?;
        Ok(())
    }

    /// Render the charter to its Markdown form (fixed heading order).
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        push_scalar(&mut out, "north_star", &self.north_star);
        push_list(&mut out, "definition_of_done", &self.definition_of_done);
        push_scalar(&mut out, "measuring_stick", &self.measuring_stick);
        push_scalar(&mut out, "current_gap", &self.current_gap);
        push_scalar(&mut out, "next_action", &self.next_action);
        push_list(&mut out, "parked", &self.parked);
        out
    }

    /// Parse the charter from its Markdown form. Unknown `## ` headings are
    /// ignored; missing headings leave the corresponding field at its default.
    pub fn parse(text: &str) -> Self {
        let mut charter = Charter::default();
        let mut current: Option<String> = None;
        let mut body: Vec<&str> = Vec::new();

        // Flush the accumulated body into the field named by `section`.
        let flush = |charter: &mut Charter, section: &Option<String>, body: &[&str]| {
            let Some(name) = section else { return };
            let joined = body.join("\n");
            match name.as_str() {
                "north_star" => charter.north_star = joined.trim().to_string(),
                "measuring_stick" => charter.measuring_stick = joined.trim().to_string(),
                "current_gap" => charter.current_gap = joined.trim().to_string(),
                "next_action" => charter.next_action = joined.trim().to_string(),
                "definition_of_done" => charter.definition_of_done = parse_list(body),
                "parked" => charter.parked = parse_list(body),
                _ => {} // unknown heading: ignore
            }
        };

        for line in text.lines() {
            if let Some(heading) = line.strip_prefix("## ") {
                flush(&mut charter, &current, &body);
                current = Some(heading.trim().to_string());
                body.clear();
            } else {
                body.push(line);
            }
        }
        flush(&mut charter, &current, &body);
        charter
    }
}

/// The field headings `to_markdown` emits and `parse` recognizes.
const FIELDS: [&str; 6] = [
    "north_star",
    "definition_of_done",
    "measuring_stick",
    "current_gap",
    "next_action",
    "parked",
];

/// Whether `text` contains at least one recognized `## <field>` heading.
fn has_known_heading(text: &str) -> bool {
    text.lines()
        .filter_map(|l| l.strip_prefix("## "))
        .any(|h| FIELDS.contains(&h.trim()))
}

fn push_scalar(out: &mut String, name: &str, value: &str) {
    out.push_str("## ");
    out.push_str(name);
    out.push('\n');
    if !value.is_empty() {
        out.push_str(value.trim());
        out.push('\n');
    }
    out.push('\n');
}

fn push_list(out: &mut String, name: &str, items: &[String]) {
    out.push_str("## ");
    out.push_str(name);
    out.push('\n');
    for item in items {
        out.push_str("- ");
        out.push_str(item.trim());
        out.push('\n');
    }
    out.push('\n');
}

/// Parse `- item` bullets, skipping blank lines. Non-bullet lines are kept
/// (with surrounding whitespace trimmed) so a hand-edited list stays lossy-free
/// for the common bullet case.
fn parse_list(body: &[&str]) -> Vec<String> {
    body.iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.strip_prefix("- ").unwrap_or(l).trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Charter {
        Charter {
            north_star: "Ship a re-grounding layer upstream of condukt.".to_string(),
            definition_of_done: vec![
                "compass crate compiles in the workspace".to_string(),
                "charter round-trips losslessly".to_string(),
            ],
            measuring_stick: "Defensibility x closeness-to-goal / cost.".to_string(),
            current_gap: "No crate scaffold yet; workspace build is broken.".to_string(),
            next_action: "Scaffold Cargo.toml + main.rs + config + charter.".to_string(),
            parked: vec![
                "cross-compile bins (#7)".to_string(),
                "hooks + skill (#6)".to_string(),
            ],
        }
    }

    #[test]
    fn save_load_round_trips_structured_fields() {
        // Auto-cleaned unique temp dir (atomic mkdtemp, no pid-collision TOCTOU).
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(".compass").join("charter.md");
        let original = sample();

        original.save(&path).expect("save");
        let loaded = Charter::load(&path).expect("load");

        assert_eq!(original, loaded);

        // Double round-trip is stable (normalized form is a fixed point).
        let reparsed = Charter::parse(&loaded.to_markdown());
        assert_eq!(loaded, reparsed);
    }

    #[test]
    fn empty_charter_round_trips() {
        let original = Charter::default();
        let loaded = Charter::parse(&original.to_markdown());
        assert_eq!(original, loaded);
    }

    #[test]
    fn load_rejects_a_charter_with_no_field_heading() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("charter.md");
        for text in ["---\nnorth_star: [unclosed\n---\n", "", "## bogus\nx\n"] {
            std::fs::write(&path, text).expect("write");
            let err = Charter::load(&path).expect_err(text);
            assert!(err.to_string().contains("unparseable"), "{err}");
        }
        // control: the serialized empty charter still loads.
        Charter::default().save(&path).expect("save");
        assert_eq!(Charter::load(&path).expect("load"), Charter::default());
    }

    #[test]
    fn unknown_headings_are_ignored() {
        let text = "## north_star\nGoal.\n\n## bogus_field\nignored\n";
        let c = Charter::parse(text);
        assert_eq!(c.north_star, "Goal.");
        assert!(c.definition_of_done.is_empty());
    }
}
