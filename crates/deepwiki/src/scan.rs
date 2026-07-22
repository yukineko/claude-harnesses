//! Deterministic repository mapping. Walks the tree (skipping build/vendor dirs
//! and binary blobs), then summarizes languages, layout, entry points, and the
//! key project files — the raw material a subagent turns into wiki pages. No
//! LLM, no network: just the filesystem.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

/// Directories never worth descending into.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    "vendor",
    ".idea",
    ".vscode",
    "coverage",
    ".gradle",
    ".cargo",
    ".deepwiki",
    ".turbo",
    "Pods",
    "DerivedData",
    ".terraform",
];

/// Extensions we count as source lines, mapped to a language label.
fn lang_for(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" => "C++",
        "cs" => "C#",
        "php" => "PHP",
        "scala" => "Scala",
        "sh" | "bash" | "zsh" => "Shell",
        "sql" => "SQL",
        "md" | "mdx" => "Markdown",
        "toml" | "yaml" | "yml" | "json" => "Config",
        "html" | "css" | "scss" => "Web",
        _ => return None,
    })
}

const KEY_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "Makefile",
    "Dockerfile",
    "docker-compose.yml",
    "tsconfig.json",
    "CMakeLists.txt",
    "composer.json",
    ".tool-versions",
];

#[derive(Debug, Serialize)]
pub struct RepoMap {
    pub root: String,
    pub total_files: usize,
    pub total_source_lines: usize,
    /// Language → (files, lines), sorted by lines desc when rendered.
    pub languages: BTreeMap<String, LangStat>,
    /// Top-level entries with a recursive text-file count.
    pub top_level: Vec<DirEntry>,
    pub key_files: Vec<String>,
    pub entry_points: Vec<String>,
    pub readmes: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct LangStat {
    pub files: usize,
    pub lines: usize,
}

#[derive(Debug, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub files: usize,
}

fn is_entry_point(rel: &str, name: &str) -> bool {
    matches!(
        name,
        "main.rs"
            | "lib.rs"
            | "main.go"
            | "main.py"
            | "__main__.py"
            | "app.py"
            | "index.ts"
            | "index.js"
            | "main.ts"
            | "main.js"
            | "App.tsx"
            | "Main.java"
    ) || rel.starts_with("cmd/")
        || rel == "src/main.rs"
}

pub fn scan(root: &Path) -> RepoMap {
    let mut map = RepoMap {
        root: root.display().to_string(),
        total_files: 0,
        total_source_lines: 0,
        languages: BTreeMap::new(),
        top_level: Vec::new(),
        key_files: Vec::new(),
        entry_points: Vec::new(),
        readmes: Vec::new(),
    };
    walk(root, root, &mut map);

    // Top-level summary.
    match std::fs::read_dir(root) {
        Ok(entries) => {
            let mut tops: Vec<DirEntry> = Vec::new();
            for e in entries {
                let e = match e {
                    Ok(e) => e,
                    Err(err) => {
                        eprintln!(
                            "warning: scan: unreadable entry in {}: {err}",
                            root.display()
                        );
                        continue;
                    }
                };
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && name != ".github" {
                    continue;
                }
                let is_dir = e.path().is_dir();
                if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                let files = if is_dir { count_files(&e.path()) } else { 1 };
                tops.push(DirEntry {
                    name,
                    is_dir,
                    files,
                });
            }
            tops.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(b.files.cmp(&a.files)));
            map.top_level = tops;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("warning: scan: cannot read {}: {e}", root.display()),
    }

    map.key_files.sort();
    map.key_files.dedup();
    map.entry_points.sort();
    map.readmes.sort();
    map
}

fn walk(root: &Path, dir: &Path, map: &mut RepoMap) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!("warning: walk: cannot read {}: {e}", dir.display());
            return;
        }
    };
    for e in entries {
        let e = match e {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warning: walk: unreadable entry in {}: {e}", dir.display());
                continue;
            }
        };
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name == ".git" {
                continue;
            }
            walk(root, &path, map);
            continue;
        }
        map.total_files += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if KEY_FILES.contains(&name.as_str()) {
            map.key_files.push(rel.clone());
        }
        if name.to_lowercase().starts_with("readme") {
            map.readmes.push(rel.clone());
        }
        if is_entry_point(&rel, &name) {
            map.entry_points.push(rel.clone());
        }

        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            if let Some(lang) = lang_for(ext) {
                let lines = count_lines(&path);
                map.total_source_lines += lines;
                let stat = map.languages.entry(lang.to_string()).or_default();
                stat.files += 1;
                stat.lines += lines;
            }
        }
    }
}

fn count_lines(path: &Path) -> usize {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.len() > 2_000_000 {
        return 0; // skip huge/generated files
    }
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!("warning: count_files: cannot read {}: {e}", d.display());
                continue;
            }
        };
        for e in entries {
            let e = match e {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "warning: count_files: unreadable entry in {}: {e}",
                        d.display()
                    );
                    continue;
                }
            };
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(p);
                }
            } else {
                n += 1;
            }
        }
    }
    n
}

/// Render the map as markdown for a subagent (or a human) to read.
pub fn render_markdown(map: &RepoMap) -> String {
    let mut s = String::new();
    s.push_str(&format!("# repo map: {}\n\n", map.root));
    s.push_str(&format!(
        "- total files: {}\n- source lines: {}\n\n",
        map.total_files, map.total_source_lines
    ));

    s.push_str("## languages\n");
    let mut langs: Vec<(&String, &LangStat)> = map.languages.iter().collect();
    langs.sort_by_key(|b| std::cmp::Reverse(b.1.lines));
    for (lang, st) in langs {
        s.push_str(&format!(
            "- {lang}: {} files, {} lines\n",
            st.files, st.lines
        ));
    }

    s.push_str("\n## top-level layout\n");
    for d in &map.top_level {
        let kind = if d.is_dir { "dir " } else { "file" };
        s.push_str(&format!("- [{kind}] {} ({} files)\n", d.name, d.files));
    }

    if !map.entry_points.is_empty() {
        s.push_str("\n## entry points\n");
        for e in &map.entry_points {
            s.push_str(&format!("- {e}\n"));
        }
    }
    if !map.key_files.is_empty() {
        s.push_str("\n## key files\n");
        for k in &map.key_files {
            s.push_str(&format!("- {k}\n"));
        }
    }
    if !map.readmes.is_empty() {
        s.push_str("\n## readmes\n");
        for r in &map.readmes {
            s.push_str(&format!("- {r}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_mapping() {
        assert_eq!(lang_for("rs"), Some("Rust"));
        assert_eq!(lang_for("xyz"), None);
    }

    #[test]
    fn entry_point_detection() {
        assert!(is_entry_point("src/main.rs", "main.rs"));
        assert!(is_entry_point("cmd/app/run.go", "run.go"));
        assert!(!is_entry_point("src/util.rs", "util.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_panic_on_unreadable_subdir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("secret.rs"), "// hi\n").unwrap();
        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&locked, perms.clone()).unwrap();
        let result = std::panic::catch_unwind(|| scan(dir.path()));
        perms.set_mode(0o755);
        std::fs::set_permissions(&locked, perms).unwrap();
        assert!(result.is_ok(), "scan must not panic on an unreadable subdir");
        let map = result.unwrap();
        assert!(
            map.total_files >= 1,
            "scan must still return the readable files it found, not an empty map"
        );
    }
}
