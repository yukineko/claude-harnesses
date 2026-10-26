use std::path::Path;

/// Read the whole progress file. Returns None if it is missing, unreadable, or
/// empty/whitespace-only.
pub fn read_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(raw)
}
