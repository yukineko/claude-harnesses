//! Read the project's progress file (`<cwd>/.claude/progress.md`) for the current project.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ProgressStatus {
    pub path: String,
    pub exists: bool,
    pub preview: Option<String>,
}

pub fn read(cwd: &Path) -> ProgressStatus {
    let path = cwd.join(".claude").join("progress.md");
    let exists = path.exists();
    let preview = if exists {
        std::fs::read_to_string(&path).ok().map(|s| {
            let lines: Vec<&str> = s.lines().take(10).collect();
            lines.join("\n")
        })
    } else {
        None
    };
    ProgressStatus {
        path: path.to_string_lossy().to_string(),
        exists,
        preview,
    }
}
