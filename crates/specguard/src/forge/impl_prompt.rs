//! ④ prompt-build: render per-requirement impl prompts from a ratified spec.
//!
//! The impl agent needs write permission (it will create/edit files in a
//! worktree), so a different prompt and different agent flags apply vs ②normalize
//! (DESIGN.md §1, §6).

use crate::ir::{Requirement, Spec};

pub const IMPL_MARKER: &str = "<<<SPEC_IMPL>>>";
pub const DEFAULT_TEMPLATE: &str = include_str!("../../templates/impl-prompt.md");

pub const IMPL_PLACEHOLDERS: &[&str] = &[
    "{{PROJECT_NAME}}",
    "{{DATE}}",
    "{{SPEC_ID}}",
    "{{REQ_ID}}",
    "{{STATEMENT}}",
    "{{ACCEPTANCE}}",
    "{{CANON}}",
    "{{IMPL_MARKER}}",
];

pub fn missing_placeholders(template: &str) -> Vec<&'static str> {
    IMPL_PLACEHOLDERS
        .iter()
        .filter(|p| !template.contains(**p))
        .copied()
        .collect()
}

fn acceptance_block(acceptance: &[String]) -> String {
    if acceptance.is_empty() {
        return "- (なし)\n".to_string();
    }
    acceptance.iter().map(|a| format!("- {a}\n")).collect()
}

fn canon_block(canon: &[String]) -> String {
    if canon.is_empty() {
        return "- (なし)\n".to_string();
    }
    canon.iter().map(|c| format!("- `{c}`\n")).collect()
}

/// Render one impl prompt for a single requirement.
pub fn render(
    template: &str,
    project_name: &str,
    spec_id: &str,
    req: &Requirement,
    date: &str,
) -> String {
    template
        .replace("{{PROJECT_NAME}}", project_name)
        .replace("{{DATE}}", date)
        .replace("{{SPEC_ID}}", spec_id)
        .replace("{{REQ_ID}}", &req.id)
        .replace("{{STATEMENT}}", &req.statement)
        .replace("{{ACCEPTANCE}}", &acceptance_block(&req.acceptance))
        .replace("{{CANON}}", &canon_block(&req.canon))
        .replace("{{IMPL_MARKER}}", IMPL_MARKER)
}

/// Render all prompts for a ratified spec, returning `(req_id, prompt)` pairs.
pub fn render_all(
    template: &str,
    project_name: &str,
    spec: &Spec,
    date: &str,
) -> Vec<(String, String)> {
    spec.requirements
        .iter()
        .map(|r| {
            (
                r.id.clone(),
                render(template, project_name, spec.spec.id.as_str(), r, date),
            )
        })
        .collect()
}

/// Write rendered prompts to `<dir>/<spec_id>-<req_id>.prompt.md`.
pub fn write_prompts(
    dir: &std::path::Path,
    spec_id: &str,
    prompts: &[(String, String)],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut paths = Vec::new();
    for (req_id, prompt) in prompts {
        let path = dir.join(format!("{spec_id}-{req_id}.prompt.md"));
        std::fs::write(&path, prompt)?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Requirement;

    fn req() -> Requirement {
        Requirement {
            id: "R1".into(),
            statement: "rate-limit 429".into(),
            acceptance: vec!["5回失敗→429".into()],
            canon: vec!["docs/auth.md#rate-limit".into()],
            falsifiable: true,
            repro_test: None,
        }
    }

    #[test]
    fn default_template_satisfies_contract() {
        assert!(missing_placeholders(DEFAULT_TEMPLATE).is_empty());
    }

    #[test]
    fn render_fills_all_placeholders() {
        let out = render(DEFAULT_TEMPLATE, "Demo", "2026-test", &req(), "2026-06-23");
        assert!(out.contains("R1"));
        assert!(out.contains("rate-limit 429"));
        assert!(out.contains("5回失敗→429"));
        assert!(out.contains(IMPL_MARKER));
        assert!(!out.contains("{{"));
    }
}
