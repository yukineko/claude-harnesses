//! External linter / scanner orchestration (Check 3). Every tool is optional:
//! if its binary is missing the tool is skipped silently, exactly like the
//! original hook. Output is summarized into blocking issues.
//!
//! Cross-platform note: the PowerShell version resolved Windows `.cmd` shims
//! (eslint.cmd / tsc.cmd). Here we resolve the bare `node_modules/.bin/<tool>`
//! and add `.cmd` only on Windows, so the same code runs on Linux/macOS.

use super::Ctx;
use crate::classify::ext_of;
use crate::model::Issue;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use wait_timeout::ChildExt;

fn have(bin: &str) -> bool {
    which(bin).is_some()
}

/// Locate an executable on PATH (cross-platform `which`).
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{bin}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Run a command, capturing combined stdout+stderr, with a hard timeout.
/// Returns (success, combined_output). On timeout: (false, "<killed>").
fn run_bounded(mut cmd: Command, timeout: Duration) -> (bool, String) {
    use std::process::Stdio;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, format!("spawn failed: {e}")),
    };
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                use std::io::Read;
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                out.push_str(&String::from_utf8_lossy(&buf));
            }
            if let Some(mut s) = child.stderr.take() {
                use std::io::Read;
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                out.push_str(&String::from_utf8_lossy(&buf));
            }
            (status.success(), out)
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            (false, "<killed: timeout>".to_string())
        }
        Err(e) => (false, format!("wait failed: {e}")),
    }
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join("\n    ")
}

/// Bin path for a node tool inside a project root, honoring the OS shim suffix.
fn node_bin(project_abs: &Path, tool: &str) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{tool}.cmd")
    } else {
        tool.to_string()
    };
    project_abs.join("node_modules").join(".bin").join(name)
}

pub fn run(ctx: &Ctx, out: &mut Vec<Issue>) {
    let cfg = &ctx.cfg.linters;
    let timeout = Duration::from_secs(cfg.timeout_secs.max(1));
    let mut syntax_fails: Vec<String> = Vec::new();

    let has_python = have("python") || have("python3");
    let python_bin = if have("python") { "python" } else { "python3" };
    let has_ruff = have("ruff");
    let has_bash = have("bash");

    // 3a / per-file syntax & lint
    for file in ctx.files {
        if ctx.cls.is_excluded(file) || !ctx.root.join(file).exists() {
            continue;
        }
        let abs = ctx.root.join(file);
        match ext_of(file).as_deref() {
            Some(".py") => {
                if cfg.py_compile && has_python {
                    let mut c = Command::new(python_bin);
                    c.arg("-m")
                        .arg("py_compile")
                        .arg(&abs)
                        .current_dir(ctx.root);
                    let (ok, o) = run_bounded(c, timeout);
                    if !ok {
                        syntax_fails.push(format!(
                            "  {file} (py_compile):\n    {}",
                            first_lines(&o, 3)
                        ));
                    }
                }
                if cfg.ruff && has_ruff {
                    let mut c = Command::new("ruff");
                    c.arg("check")
                        .arg("--quiet")
                        .arg(&abs)
                        .current_dir(ctx.root);
                    let (ok, o) = run_bounded(c, timeout);
                    if !ok {
                        syntax_fails.push(format!("  {file} (ruff):\n    {}", first_lines(&o, 5)));
                    }
                }
            }
            Some(".sh") if cfg.bash_n && has_bash => {
                let mut c = Command::new("bash");
                c.arg("-n").arg(&abs).current_dir(ctx.root);
                let (ok, o) = run_bounded(c, timeout);
                if !ok {
                    syntax_fails.push(format!("  {file} (bash -n):\n    {}", first_lines(&o, 3)));
                }
            }
            Some(".ts") | Some(".tsx") | Some(".js") | Some(".jsx") if cfg.eslint => {
                if let Some(root) = eslint_root(ctx, file) {
                    let bin = node_bin(&ctx.root.join(&root), "eslint");
                    if bin.exists() {
                        let mut c = Command::new(&bin);
                        c.arg("--max-warnings=0")
                            .arg(&abs)
                            .current_dir(ctx.root.join(&root));
                        let (ok, o) = run_bounded(c, timeout);
                        if !ok {
                            syntax_fails
                                .push(format!("  {file} (eslint):\n    {}", first_lines(&o, 5)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !syntax_fails.is_empty() {
        out.push(Issue::block(
            "SYNTAX / LINT FAILURE",
            format!(
                "SYNTAX / LINT FAILURE in changed files:\n{}",
                syntax_fails.join("\n")
            ),
        ));
    }

    // 3c: tsc per node project (once)
    if cfg.tsc {
        let mut tsc_fails: Vec<String> = Vec::new();
        for proj in &cfg.node_projects {
            let proj_abs = ctx.root.join(proj);
            let bin = node_bin(&proj_abs, "tsc");
            if !bin.exists() {
                continue;
            }
            let touched = ctx.files.iter().any(|f| {
                let fn_ = crate::classify::norm(f);
                fn_.starts_with(&format!("{}/", crate::classify::norm(proj)))
                    && matches!(ext_of(f).as_deref(), Some(".ts") | Some(".tsx"))
            });
            if !touched {
                continue;
            }
            let mut c = Command::new(&bin);
            c.arg("--noEmit")
                .arg("--incremental")
                .current_dir(&proj_abs);
            let (ok, o) = run_bounded(c, timeout);
            let has_ts_err = o.contains("error TS");
            if !ok || has_ts_err {
                if o.contains("<killed") {
                    tsc_fails.push(format!(
                        "  {proj} (tsc): killed after {}s",
                        cfg.timeout_secs
                    ));
                } else {
                    let summary: Vec<&str> = o
                        .lines()
                        .filter(|l| l.contains("error TS"))
                        .take(5)
                        .collect();
                    let body = if summary.is_empty() {
                        first_lines(&o, 3)
                    } else {
                        summary.join("\n    ")
                    };
                    tsc_fails.push(format!("  {proj} (tsc):\n    {body}"));
                }
            }
        }
        if !tsc_fails.is_empty() {
            out.push(Issue::block(
                "TYPESCRIPT TYPE-CHECK FAILURE",
                format!("TYPESCRIPT TYPE-CHECK FAILURE:\n{}", tsc_fails.join("\n")),
            ));
        }
    }

    // 3e: radon cyclomatic complexity (python module)
    if cfg.radon && has_python {
        let mut probe = Command::new(python_bin);
        probe.arg("-c").arg("import radon").current_dir(ctx.root);
        let (radon_ok, _) = run_bounded(probe, timeout);
        if radon_ok {
            let mut fails: Vec<String> = Vec::new();
            for file in ctx.files {
                if ctx.cls.is_excluded(file)
                    || ctx.cls.is_test(file)
                    || ext_of(file).as_deref() != Some(".py")
                    || !ctx.root.join(file).exists()
                {
                    continue;
                }
                let head = ctx.read_head(file, 20);
                if regex_ignore_file(&head) {
                    continue;
                }
                let mut c = Command::new(python_bin);
                c.arg("-m")
                    .arg("radon")
                    .arg("cc")
                    .arg("-n")
                    .arg("D")
                    .arg("-s")
                    .arg(ctx.root.join(file))
                    .current_dir(ctx.root);
                let (_ok, o) = run_bounded(c, timeout);
                let hits: Vec<&str> = o.lines().filter(|l| radon_hit(l)).take(3).collect();
                if !hits.is_empty() {
                    fails.push(format!("  {file}: {}", hits.join("; ")));
                }
            }
            if !fails.is_empty() {
                out.push(Issue::block(
                    "COMPLEXITY TOO HIGH",
                    format!(
                        "COMPLEXITY TOO HIGH (radon cc rank D+, McCabe > 20):\n{}\nRefactor or add `# audit-ignore: <reason>`.",
                        fails.join("\n")
                    ),
                ));
            }
        }
    }

    // 3f: semgrep
    if cfg.semgrep && have("semgrep") {
        let scan: Vec<PathBuf> = ctx
            .files
            .iter()
            .filter(|f| {
                !ctx.cls.is_excluded(f)
                    && !ctx.cls.is_test(f)
                    && ctx.root.join(f).exists()
                    && matches!(
                        ext_of(f).as_deref(),
                        Some(".py") | Some(".ts") | Some(".tsx") | Some(".js") | Some(".jsx")
                    )
            })
            .map(|f| ctx.root.join(f))
            .collect();
        if !scan.is_empty() {
            let mut c = Command::new("semgrep");
            c.arg("--config")
                .arg("auto")
                .arg("--quiet")
                .arg("--error")
                .arg("--timeout")
                .arg("20");
            for f in &scan {
                c.arg(f);
            }
            c.current_dir(ctx.root);
            let (ok, o) = run_bounded(c, Duration::from_secs(cfg.timeout_secs.max(30)));
            if !ok {
                let summary: Vec<&str> = o
                    .lines()
                    .filter(|l| {
                        l.contains("rule:") || l.contains("severity:") || l.contains("message:")
                    })
                    .take(8)
                    .collect();
                let body = if summary.is_empty() {
                    first_lines(&o, 5)
                } else {
                    summary.join("\n    ")
                };
                out.push(Issue::block(
                    "SEMGREP",
                    format!("SEMGREP findings:\n    {body}"),
                ));
            }
        }
    }

    // 3b: gitleaks (scan a temp copy of changed files, no-git)
    if cfg.gitleaks && have("gitleaks") {
        let scan: Vec<&String> = ctx
            .files
            .iter()
            .filter(|f| !ctx.cls.is_excluded(f) && ctx.root.join(f).exists())
            .collect();
        if !scan.is_empty() {
            // `gitleaks detect` has no per-file argument list (unlike semgrep
            // above) — only `--source <dir>` or `--pipe`. Scanning `ctx.root`
            // directly with `--no-git` walks the WHOLE working tree, including
            // .gitignore'd build artifacts (e.g. Python __pycache__/*.pyc,
            // which embed source string literals as compiled constants and
            // can false-positive on secret-shaped test fixtures). So we copy
            // only the actually-changed files into a scratch dir, preserving
            // relative paths, and scan that instead — this is what the
            // comment above always claimed to do.
            let tmp = tempfile::tempdir();
            let Ok(tmp) = tmp else {
                out.push(Issue::block(
                    "GITLEAKS",
                    "gitleaks scan skipped: could not create scratch dir for scan".to_string(),
                ));
                return;
            };
            for f in &scan {
                let src = ctx.root.join(f);
                let dest = tmp.path().join(f);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&src, &dest);
            }
            let mut c = Command::new("gitleaks");
            c.arg("detect")
                .arg("--no-git")
                .arg("--source")
                .arg(tmp.path())
                .arg("--no-banner")
                .arg("--redact");
            c.current_dir(ctx.root);
            let (ok, o) = run_bounded(c, Duration::from_secs(cfg.timeout_secs.max(30)));
            if !ok {
                let summary: Vec<&str> = o
                    .lines()
                    .filter(|l| {
                        l.contains("Finding") || l.contains("Secret") || l.contains("RuleID")
                    })
                    .take(5)
                    .collect();
                let body = if summary.is_empty() {
                    first_lines(&o, 5)
                } else {
                    summary.join("\n    ")
                };
                out.push(Issue::block(
                    "GITLEAKS",
                    format!("GITLEAKS DETECTED SECRETS in changed files:\n    {body}"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod real_repo_repro {
    use super::*;
    use crate::classify::Classifier;
    use crate::config::Config;
    use std::cell::RefCell;

    /// Manual repro for backlog 848913b8 against the actual harness repo tree.
    /// Runs for real when `scripts/__pycache__/*.pyc` exists (e.g. after
    /// `pytest scripts/test_check_hardcoded_secret.py`), otherwise skips —
    /// same pattern as the gitleaks-not-on-PATH skip below.
    #[test]
    fn does_not_flag_pycache_outside_changed_set() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        if !root
            .join("scripts/__pycache__/check-hardcoded-secret.cpython-312.pyc")
            .exists()
        {
            eprintln!(
                "skipping: run `pytest scripts/test_check_hardcoded_secret.py` first to regenerate __pycache__"
            );
            return;
        }
        let cfg = Config::default();
        let cls = Classifier::new(&cfg.classify);
        let files = vec!["crates/precommit-audit/Cargo.toml".to_string()];
        let incomplete = RefCell::new(Vec::new());
        let ctx = Ctx {
            root: &root,
            cfg: &cfg,
            cls: &cls,
            files: &files,
            incomplete: &incomplete,
        };
        let mut out = Vec::new();
        run(&ctx, &mut out);
        let categories: Vec<&str> = out.iter().map(|i| i.category.as_str()).collect();
        assert!(
            out.iter().all(|i| i.category != "GITLEAKS"),
            "gitleaks flagged something even though the only changed file is clean; scan must have leaked into __pycache__: {categories:?}"
        );
    }
}

fn radon_hit(line: &str) -> bool {
    // radon lines look like "    F 12:0 name - D (23)"
    let t = line.trim_start();
    let mut chars = t.chars();
    match (chars.next(), chars.next()) {
        (Some(c), Some(' ')) if c.is_ascii_uppercase() => t.contains(':'),
        _ => false,
    }
}

fn regex_ignore_file(head: &str) -> bool {
    regex::Regex::new(r"audit-ignore-file:\s*\S")
        .map(|r| r.is_match(head))
        .unwrap_or(false)
}

/// Which configured node project (if any) owns this file, for eslint.
fn eslint_root(ctx: &Ctx, file: &str) -> Option<String> {
    let fnorm = crate::classify::norm(file);
    for proj in &ctx.cfg.linters.node_projects {
        let pnorm = crate::classify::norm(proj);
        if fnorm.starts_with(&format!("{pnorm}/"))
            && ctx.root.join(proj).join("package.json").exists()
        {
            return Some(proj.clone());
        }
    }
    None
}

#[cfg(test)]
mod gitleaks_scope_tests {
    use super::*;
    use crate::classify::Classifier;
    use crate::config::Config;
    use std::cell::RefCell;

    /// Regression test for backlog 848913b8: `run` must scan only the
    /// changed files (`ctx.files`), never the whole `ctx.root` tree. A
    /// gitignored/untracked file containing a secret-shaped string must NOT
    /// be flagged, even though it physically sits under `ctx.root`.
    #[test]
    fn gitleaks_ignores_untracked_files_outside_the_changed_set() {
        if which("gitleaks").is_none() {
            eprintln!("skipping: gitleaks not on PATH");
            return;
        }
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path();

        // A file that is NOT part of the changed set (simulates a gitignored
        // build artifact like scripts/__pycache__/*.pyc) containing a
        // secret-shaped literal that gitleaks' generic-api-key rule matches.
        std::fs::create_dir_all(root.join("untracked_dir")).unwrap();
        std::fs::write(
            root.join("untracked_dir/stale_cache.txt"),
            "api_key = \"sk-abcd1234efgh5678ignoreme\"\n",
        )
        .unwrap();

        // A file that IS part of the changed set and is clean.
        std::fs::write(root.join("clean.py"), "print('hello')\n").unwrap();

        let cfg = Config::default();
        let cls = Classifier::new(&cfg.classify);
        let files = vec!["clean.py".to_string()];
        let incomplete = RefCell::new(Vec::new());
        let ctx = Ctx {
            root,
            cfg: &cfg,
            cls: &cls,
            files: &files,
            incomplete: &incomplete,
        };
        let mut out = Vec::new();
        run(&ctx, &mut out);

        let categories: Vec<&str> = out.iter().map(|i| i.category.as_str()).collect();
        assert!(
            out.iter().all(|i| i.category != "GITLEAKS"),
            "gitleaks flagged an untracked file outside ctx.files: {categories:?}"
        );
    }

    /// A real secret in a file that IS in the changed set must still be
    /// caught (the fix must not blind the scan entirely).
    #[test]
    fn gitleaks_still_catches_a_secret_in_a_changed_file() {
        if which("gitleaks").is_none() {
            eprintln!("skipping: gitleaks not on PATH");
            return;
        }
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path();
        std::fs::write(
            root.join("leaky.py"),
            "api_key = \"sk-abcd1234efgh5678ignoreme\"\n",
        )
        .unwrap();

        let cfg = Config::default();
        let cls = Classifier::new(&cfg.classify);
        let files = vec!["leaky.py".to_string()];
        let incomplete = RefCell::new(Vec::new());
        let ctx = Ctx {
            root,
            cfg: &cfg,
            cls: &cls,
            files: &files,
            incomplete: &incomplete,
        };
        let mut out = Vec::new();
        run(&ctx, &mut out);

        let categories: Vec<&str> = out.iter().map(|i| i.category.as_str()).collect();
        assert!(
            out.iter().any(|i| i.category == "GITLEAKS"),
            "gitleaks failed to flag a real secret in a changed file: {categories:?}"
        );
    }
}
