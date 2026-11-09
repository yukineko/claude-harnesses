//! Merge-time `cargo fmt --check` + `cargo clippy -D warnings` gate of a pinned
//! task-branch tip (backlog 0a68823d).
//!
//! `condukt worktree merge` is the one deterministic choke point every task
//! branch passes before it reaches the default branch. Nothing else on the
//! worker -> commit -> merge path runs fmt or clippy (`.githooks/pre-commit`
//! does not), so a worker could land a red tip. [`ensure_tip_lint_clean`] runs
//! both lints against EXACTLY the pinned `tip` commit, scoped — the way
//! `scripts/lint-changed-crates.sh` scopes them — to the Cargo packages the
//! branch changed relative to its recorded creation base.
//!
//! Every non-determination refuses (CLAUDE.md §3): a changed set git cannot
//! produce, a manifest or lockfile that cannot be read/parsed, `cargo metadata`
//! that fails or does not list a changed package, a cargo that cannot be found
//! / spawned / times out / dies on a signal, a tip that cannot be checked out.
//! Only a changed set that is DETERMINED to touch no Cargo package skips the
//! lint (no cargo is spawned at all — docs- or script-only branches pay
//! nothing, and non-Cargo repositories are unaffected).
//!
//! Scope, stated as a limitation and not hidden: like lint-changed-crates.sh,
//! the reverse dependents of a changed package are NOT linted, and the lint
//! judges the tip as committed, not the merge result with the default branch.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use harness_core::boundary::{run_with_timeout, CommandOutput};
use harness_core::verdict::Determination;

use super::{git, git_exit_code};

/// Bound on each cargo invocation. A cold clippy of a large crate takes
/// minutes; a hung cargo must still not hold the repo-primary lock forever.
const LINT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Files whose change can alter how every package of their workspace builds or
/// lints (mirrors the workspace-level set of lint-changed-crates.sh).
const WORKSPACE_LEVEL_FILES: &[&str] = &[
    "Cargo.lock",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// What a changed set requires of the workspace a manifest belongs to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Want {
    /// Lint the package this manifest itself declares.
    this_package: bool,
    /// Lint every member of the manifest's workspace.
    all_members: bool,
    /// Lint these members (by package name) of the manifest's workspace.
    named: BTreeSet<String>,
}

/// Lint plan: repo-relative `Cargo.toml` path -> what to lint around it.
/// Empty = the changed set was determined to touch no Cargo package.
pub(crate) type Plan = BTreeMap<String, Want>;

/// Refuse (`Err`) unless the Cargo packages `tip` changed vs `base` are
/// `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean
/// AT `tip`. The branch's own worktree is used as the lint checkout only when
/// it is provably at `tip` with no uncommitted change outside `.backlog/` (its
/// target dir is warm); otherwise a temporary detached worktree of `tip` is
/// used. Either way exactly `tip` is linted. A failed worktree listing only
/// means the branch's worktree is not offered, so it never relaxes WHAT is
/// linted.
pub fn ensure_tip_lint_clean(
    repo: &Path,
    branch: &str,
    default_branch: &str,
    base: &str,
    tip: &str,
) -> Result<()> {
    let primary = repo.canonicalize().ok();
    let task_worktree = match super::worktree_for_branch(repo, branch) {
        Ok(Some(wt)) if wt.canonicalize().ok() != primary => Some(wt),
        _ => None,
    };
    ensure_tip_lint_clean_with(
        repo,
        branch,
        default_branch,
        base,
        tip,
        task_worktree.as_deref(),
        find_cargo(),
    )
}

pub(crate) fn ensure_tip_lint_clean_with(
    repo: &Path,
    branch: &str,
    default_branch: &str,
    base: &str,
    tip: &str,
    task_worktree: Option<&Path>,
    cargo: Option<PathBuf>,
) -> Result<()> {
    let untouched = format!("refusing to merge ('{default_branch}' untouched)");
    let plan = plan(repo, base, tip).with_context(|| {
        format!(
            "merge-lint-undetermined: could not determine which Cargo packages '{branch}' \
             changed ({base}..{tip}); {untouched}"
        )
    })?;
    if plan.is_empty() {
        return Ok(());
    }
    let Some(cargo) = cargo else {
        bail!(
            "merge-lint-undetermined: '{branch}' changes Cargo package(s) ({}) but no `cargo` \
             was found (PATH, $CARGO_HOME/bin, $HOME/.cargo/bin); fmt/clippy of the tip \
             cannot run, {untouched}",
            plan.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    };

    let temp;
    let dir: PathBuf = match task_worktree.filter(|wt| worktree_is_exactly_at(wt, tip)) {
        Some(wt) => wt.to_path_buf(),
        None => {
            temp = TempTipWorktree::check_out(repo, tip).with_context(|| {
                format!(
                    "merge-lint-undetermined: could not check out the tip {tip} of '{branch}' \
                     to lint it; {untouched}"
                )
            })?;
            temp.path.clone()
        }
    };

    let groups = resolve_plan(&cargo, &dir, &plan).with_context(|| {
        format!(
            "merge-lint-undetermined: could not resolve the changed Cargo packages of \
             '{branch}' at {tip}; {untouched}"
        )
    })?;

    let mut failures = Vec::new();
    for (ws_root, pkgs) in &groups {
        let manifest = ws_root.join("Cargo.toml");
        let manifest = manifest.to_string_lossy().to_string();
        let mut pkg_args: Vec<&str> = Vec::new();
        for p in pkgs {
            pkg_args.push("-p");
            pkg_args.push(p);
        }
        let mut fmt_args = vec!["fmt", "--manifest-path", &manifest];
        fmt_args.extend(&pkg_args);
        fmt_args.extend(["--", "--check", "--color", "never"]);
        let mut clippy_args = vec!["clippy", "--color", "never", "--manifest-path", &manifest];
        clippy_args.extend(&pkg_args);
        clippy_args.extend(["--all-targets", "--", "-D", "warnings"]);
        for args in [fmt_args, clippy_args] {
            let shown = format!("cargo {}", args.join(" "));
            match run_cargo(&cargo, ws_root, &args) {
                Determination::Known(out) if out.code() == 0 => {}
                Determination::Known(out) => failures.push(format!(
                    "`{shown}` FAILED (exit {}):\n{}",
                    out.code(),
                    output_tail(out)
                )),
                Determination::Undetermined(why) => bail!(
                    "merge-lint-undetermined: `{shown}` did not run to a conclusion for the tip \
                     {tip} of '{branch}': {why}; {untouched}"
                ),
            }
        }
    }
    if !failures.is_empty() {
        bail!(
            "merge-lint-failed: the tip {tip} of '{branch}' is not fmt/clippy clean for the \
             package(s) it changed; {untouched}. Fix it on the branch (`cargo fmt`, then \
             `cargo clippy --all-targets -- -D warnings`), commit with the task trailer, and \
             merge again.\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

/// Determine the lint plan from `git diff base..tip`. `Err` = undetermined.
pub(crate) fn plan(repo: &Path, base: &str, tip: &str) -> Result<Plan> {
    let changed = git(
        repo,
        &["diff", "--name-only", "-z", "--no-renames", base, tip, "--"],
    )
    .context("`git diff --name-only` of the branch failed")?;
    let changed: Vec<&str> = changed
        .split('\0')
        .map(|s| s.trim_matches('\n'))
        .filter(|s| !s.is_empty())
        .collect();
    let mut plan = Plan::new();
    if changed.is_empty() {
        return Ok(plan);
    }
    let tip_files: HashSet<String> = git(repo, &["ls-tree", "-r", "--name-only", "-z", tip])
        .context("`git ls-tree` of the tip failed")?
        .split('\0')
        .map(|s| s.trim_matches('\n').to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for path in changed {
        let (dir, name) = split_path(path);
        if name == "Cargo.toml" {
            if tip_files.contains(path) {
                let kind = manifest_kind(repo, tip, path)?;
                let w = plan.entry(path.to_string()).or_default();
                if kind.workspace {
                    w.all_members = true;
                } else {
                    w.this_package = true;
                }
            } else if let Some(m) = nearest_manifest(&tip_files, parent_dir(dir)) {
                // A deleted manifest (a removed crate): conservatively lint the
                // whole workspace that remains around it.
                plan.entry(m).or_default().all_members = true;
            }
            continue;
        }
        let ws_dir = if path.ends_with(".cargo/config.toml") || path.ends_with(".cargo/config") {
            Some(parent_dir(dir))
        } else if WORKSPACE_LEVEL_FILES.contains(&name) {
            Some(dir)
        } else {
            None
        };
        if let Some(ws_dir) = ws_dir {
            let Some(m) = nearest_manifest(&tip_files, ws_dir) else {
                continue; // no Cargo manifest governs it at the tip
            };
            if name == "Cargo.lock" {
                match lock_path_package_change(repo, base, tip, path)? {
                    Some(names) => plan.entry(m).or_default().named.extend(names),
                    None => plan.entry(m).or_default().all_members = true,
                }
            } else {
                plan.entry(m).or_default().all_members = true;
            }
            continue;
        }
        if let Some(m) = nearest_manifest(&tip_files, dir) {
            if manifest_kind(repo, tip, &m)?.package {
                plan.entry(m).or_default().this_package = true;
            }
            // Under a virtual workspace manifest only (e.g. docs/): no package.
        }
    }
    plan.retain(|_, w| w.this_package || w.all_members || !w.named.is_empty());
    Ok(plan)
}

/// `("a/b", "c.rs")` for `"a/b/c.rs"`, `("", "c.rs")` for `"c.rs"`.
fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", path),
    }
}

fn parent_dir(dir: &str) -> &str {
    split_path(dir).0
}

/// The `Cargo.toml` in `dir` or its nearest ancestor present at the tip.
fn nearest_manifest(tip_files: &HashSet<String>, dir: &str) -> Option<String> {
    let mut d = dir;
    loop {
        let candidate = if d.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{d}/Cargo.toml")
        };
        if tip_files.contains(&candidate) {
            return Some(candidate);
        }
        if d.is_empty() {
            return None;
        }
        d = parent_dir(d);
    }
}

struct ManifestKind {
    package: bool,
    workspace: bool,
}

fn manifest_kind(repo: &Path, tip: &str, manifest: &str) -> Result<ManifestKind> {
    let text = git(repo, &["show", &format!("{tip}:{manifest}")])
        .with_context(|| format!("could not read {manifest} at the tip"))?;
    let value: toml::Table = text
        .parse()
        .with_context(|| format!("could not parse {manifest} at the tip"))?;
    let kind = ManifestKind {
        package: value.contains_key("package"),
        workspace: value.contains_key("workspace"),
    };
    if !kind.package && !kind.workspace {
        bail!("{manifest} at the tip declares neither [package] nor [workspace]");
    }
    Ok(kind)
}

/// `Some(names)` when the lockfile change is provably confined to path
/// packages (entries without `source`, i.e. workspace members — what a version
/// bump rewrites); `None` when anything else differs (lint every member).
/// `Err` when either side cannot be read or parsed.
fn lock_path_package_change(
    repo: &Path,
    base: &str,
    tip: &str,
    lock: &str,
) -> Result<Option<BTreeSet<String>>> {
    let read = |rev: &str| -> Result<Option<toml::Table>> {
        // `cat-file -e` exits 0 when present, 128 (fatal) when absent.
        let spec = format!("{rev}:{lock}");
        match git_exit_code(repo, &["cat-file", "-e", &spec])? {
            0 => {}
            128 => return Ok(None),
            c => bail!("`git cat-file -e {spec}` exited {c}"),
        }
        let text = git(repo, &["show", &spec]).with_context(|| format!("could not read {spec}"))?;
        Ok(Some(
            text.parse()
                .with_context(|| format!("could not parse {spec}"))?,
        ))
    };
    let (Some(mut old), Some(mut new)) = (read(base)?, read(tip)?) else {
        return Ok(None); // lockfile added or removed: everything may change
    };
    let take = |t: &mut toml::Table| -> Result<Vec<toml::Value>> {
        match t.remove("package") {
            None => Ok(Vec::new()),
            Some(toml::Value::Array(a)) => Ok(a),
            Some(_) => Err(anyhow!("{lock}: `package` is not an array")),
        }
    };
    let (old_pkgs, new_pkgs) = (take(&mut old)?, take(&mut new)?);
    if old != new {
        return Ok(None); // header (version / metadata / patch) changed
    }
    let mut names = BTreeSet::new();
    let differing = old_pkgs
        .iter()
        .filter(|p| !new_pkgs.contains(p))
        .chain(new_pkgs.iter().filter(|p| !old_pkgs.contains(p)));
    for p in differing {
        let t = p
            .as_table()
            .ok_or_else(|| anyhow!("{lock}: a [[package]] entry is not a table"))?;
        if t.contains_key("source") {
            return Ok(None);
        }
        let name = t
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| anyhow!("{lock}: a [[package]] entry has no name"))?;
        names.insert(name.to_string());
    }
    Ok(Some(names))
}

/// Resolve the plan into `workspace root -> package names` with
/// `cargo metadata --no-deps` run at the lint checkout `dir`.
fn resolve_plan(
    cargo: &Path,
    dir: &Path,
    plan: &Plan,
) -> Result<BTreeMap<PathBuf, BTreeSet<String>>> {
    let mut groups: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for (manifest, want) in plan {
        let manifest_abs = dir.join(manifest);
        let shown = manifest_abs.to_string_lossy().to_string();
        let args = [
            "metadata",
            "--no-deps",
            "--format-version=1",
            "--manifest-path",
            &shown,
        ];
        let json = match run_cargo(cargo, dir, &args) {
            Determination::Known(out) => match out.stdout_on_success() {
                Determination::Known(s) => s,
                Determination::Undetermined(why) => bail!("{why}"),
            },
            Determination::Undetermined(why) => bail!("{why}"),
        };
        let meta: serde_json::Value = serde_json::from_str(&json)
            .with_context(|| format!("could not parse `cargo metadata` for {manifest}"))?;
        let root = meta["workspace_root"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("`cargo metadata` for {manifest} has no workspace_root"))?;
        let packages = meta["packages"]
            .as_array()
            .ok_or_else(|| anyhow!("`cargo metadata` for {manifest} has no packages"))?;
        let members: Vec<(String, PathBuf)> = packages
            .iter()
            .map(|p| {
                let name = p["name"].as_str().map(str::to_string);
                let mp = p["manifest_path"].as_str().map(PathBuf::from);
                name.zip(mp)
                    .ok_or_else(|| anyhow!("`cargo metadata` package without name/manifest_path"))
            })
            .collect::<Result<_>>()?;
        let set = groups.entry(root.clone()).or_default();
        if want.all_members {
            if members.is_empty() {
                bail!("`cargo metadata` for {manifest} lists no workspace members");
            }
            set.extend(members.iter().map(|(n, _)| n.clone()));
        }
        for n in &want.named {
            if !members.iter().any(|(m, _)| m == n) {
                bail!(
                    "changed lockfile entry '{n}' is not a member of the workspace at {manifest}"
                );
            }
            set.insert(n.clone());
        }
        if want.this_package {
            let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            let target = canon(&manifest_abs);
            let (name, _) = members
                .iter()
                .find(|(_, mp)| canon(mp) == target)
                .ok_or_else(|| {
                    anyhow!("`cargo metadata` does not list the package of {manifest}")
                })?;
            set.insert(name.clone());
        }
    }
    groups.retain(|_, s| !s.is_empty());
    if groups.is_empty() {
        bail!("the changed Cargo packages resolved to no package to lint");
    }
    Ok(groups)
}

fn run_cargo(cargo: &Path, dir: &Path, args: &[&str]) -> Determination<CommandOutput> {
    let mut cmd = Command::new(cargo);
    harness_core::git_env::scrub(&mut cmd);
    cmd.current_dir(dir).args(args).stdin(Stdio::null());
    run_with_timeout(&mut cmd, LINT_TIMEOUT)
}

/// Last lines of stdout+stderr for the refusal diagnostic.
fn output_tail(out: CommandOutput) -> String {
    const MAX_LINES: usize = 60;
    let code = out.code();
    let stderr = out.stderr().to_string();
    // The observed code is passed as "ok", so this is always `Known`; it is
    // the only accessor for stdout (where `cargo fmt --check` prints its diff).
    let stdout = match out.stdout_allowing(&[code]) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => format!("(stdout could not be read: {why})"),
    };
    let all = format!("{stdout}\n{stderr}");
    let lines: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..]
        .iter()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `cargo` on PATH, else `$CARGO_HOME/bin/cargo`, else `$HOME/.cargo/bin/cargo`
/// (a hook-launched condukt's PATH need not include rustup's shim dir).
fn find_cargo() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("cargo")).collect())
        .unwrap_or_default();
    if let Some(ch) = std::env::var_os("CARGO_HOME") {
        candidates.push(PathBuf::from(ch).join("bin/cargo"));
    }
    if let Some(h) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(h).join(".cargo/bin/cargo"));
    }
    candidates.into_iter().find(|c| c.is_file())
}

/// `wt` has `tip` checked out and no uncommitted change outside `.backlog/`
/// (the uncommitted store diff that rides along with the merge). Any probe
/// failure is `false`: the caller then lints a fresh checkout of `tip`
/// instead, so "cannot tell" never relaxes WHAT is linted.
fn worktree_is_exactly_at(wt: &Path, tip: &str) -> bool {
    let at_tip = matches!(git(wt, &["rev-parse", "HEAD"]), Ok(h) if h.trim() == tip);
    if !at_tip {
        return false;
    }
    match git(
        wt,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Ok(s) => s
            .split('\0')
            .filter(|e| !e.trim().is_empty())
            // `git` trims its output, so the first entry may have lost the
            // leading space of its two-char code: split on the first space
            // instead of slicing. A rename's code-less second entry has no
            // space and so is (conservatively) not a `.backlog/` entry.
            .all(|e| {
                e.trim_start()
                    .split_once(' ')
                    .is_some_and(|(_, p)| p.trim_start().starts_with(".backlog/"))
            }),
        Err(_) => false,
    }
}

/// A detached worktree of `tip`, removed on drop.
struct TempTipWorktree {
    repo: PathBuf,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl TempTipWorktree {
    fn check_out(repo: &Path, tip: &str) -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("condukt-merge-lint-")
            .tempdir()
            .context("could not create a temporary directory")?;
        let base = dir
            .path()
            .canonicalize()
            .unwrap_or(dir.path().to_path_buf());
        let path = base.join("tip");
        let shown = path.to_string_lossy().to_string();
        // Hooks disabled: a post-checkout hook must not run for a lint checkout.
        git(
            repo,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "--detach",
                "--quiet",
                &shown,
                tip,
            ],
        )?;
        Ok(TempTipWorktree {
            repo: repo.to_path_buf(),
            path,
            _dir: dir,
        })
    }
}

impl Drop for TempTipWorktree {
    fn drop(&mut self) {
        let shown = self.path.to_string_lossy().to_string();
        if git(&self.repo, &["worktree", "remove", "--force", &shown]).is_err() {
            // The directory itself is deleted by the TempDir; drop the stale
            // registration so it does not linger in `git worktree list`.
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = git(&self.repo, &["worktree", "prune"]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    fn repo_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "t@t.t"]);
        sh(&repo, &["config", "user.name", "t"]);
        sh(&repo, &["config", "commit.gpgsign", "false"]);
        for (p, c) in files {
            let f = repo.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        }
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "seed"]);
        let base = sh(&repo, &["rev-parse", "HEAD"]);
        (tmp, repo, base)
    }

    fn commit(repo: &Path, files: &[(&str, &str)]) -> String {
        for (p, c) in files {
            let f = repo.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        }
        sh(repo, &["add", "-A"]);
        sh(repo, &["commit", "-q", "-m", "change"]);
        sh(repo, &["rev-parse", "HEAD"])
    }

    const VIRTUAL: &str = "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n";
    const PKG_A: &str = "[package]\nname = \"a\"\nversion = \"0.1.0\"\n";
    const PKG_B: &str = "[package]\nname = \"b\"\nversion = \"0.1.0\"\n";

    fn ws_repo() -> (tempfile::TempDir, PathBuf, String) {
        repo_with(&[
            ("Cargo.toml", VIRTUAL),
            ("crates/a/Cargo.toml", PKG_A),
            ("crates/a/src/lib.rs", ""),
            ("crates/b/Cargo.toml", PKG_B),
            ("crates/b/src/lib.rs", ""),
            ("docs/x.md", "x"),
        ])
    }

    #[test]
    fn non_cargo_change_needs_no_lint_and_spawns_no_cargo() {
        let (_t, repo, base) = repo_with(&[("README", "a")]);
        let tip = commit(&repo, &[("README", "b")]);
        // A cargo path that cannot exist: had it been spawned, this would refuse.
        let r = ensure_tip_lint_clean_with(
            &repo,
            "b",
            "main",
            &base,
            &tip,
            None,
            Some(PathBuf::from("/nonexistent/cargo")),
        );
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn docs_change_under_virtual_workspace_needs_no_lint() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("docs/x.md", "y")]);
        assert!(plan(&repo, &base, &tip).unwrap().is_empty());
    }

    #[test]
    fn crate_file_change_plans_only_that_package() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("crates/a/src/lib.rs", "pub fn f() {}\n")]);
        let p = plan(&repo, &base, &tip).unwrap();
        assert_eq!(p.keys().collect::<Vec<_>>(), vec!["crates/a/Cargo.toml"]);
        assert!(p["crates/a/Cargo.toml"].this_package);
    }

    #[test]
    fn root_manifest_change_plans_every_member() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("Cargo.toml", &format!("{VIRTUAL}\n# c\n"))]);
        let p = plan(&repo, &base, &tip).unwrap();
        assert!(p["Cargo.toml"].all_members, "{p:?}");
    }

    #[test]
    fn lockfile_path_package_bump_plans_named_members_only() {
        let lock = |v: &str| {
            format!(
                "version = 3\n\n[[package]]\nname = \"a\"\nversion = \"{v}\"\n\n[[package]]\n\
                 name = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+x\"\n"
            )
        };
        let (_t, repo, base) = repo_with(&[
            ("Cargo.toml", VIRTUAL),
            ("Cargo.lock", &lock("0.1.0")),
            ("crates/a/Cargo.toml", PKG_A),
            ("crates/a/src/lib.rs", ""),
        ]);
        let tip = commit(&repo, &[("Cargo.lock", &lock("0.1.1"))]);
        let p = plan(&repo, &base, &tip).unwrap();
        let w = &p["Cargo.toml"];
        assert!(!w.all_members, "{p:?}");
        assert_eq!(w.named, BTreeSet::from(["a".to_string()]));
    }

    #[test]
    fn unparseable_manifest_is_undetermined_and_refuses() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("crates/a/Cargo.toml", "[package\nname=")]);
        let r = ensure_tip_lint_clean_with(
            &repo,
            "b",
            "main",
            &base,
            &tip,
            None,
            Some(PathBuf::from("/nonexistent/cargo")),
        );
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("merge-lint-undetermined"), "{e}");
    }

    #[test]
    fn undeterminable_changed_set_refuses() {
        let (_t, repo, _base) = ws_repo();
        let tip = sh(&repo, &["rev-parse", "HEAD"]);
        let bogus = "0123456789abcdef0123456789abcdef01234567";
        let r = ensure_tip_lint_clean_with(&repo, "b", "main", bogus, &tip, None, None);
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("merge-lint-undetermined"), "{e}");
    }

    #[test]
    fn cargo_not_found_refuses_when_a_package_changed() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("crates/a/src/lib.rs", "pub fn f() {}\n")]);
        let r = ensure_tip_lint_clean_with(&repo, "b", "main", &base, &tip, None, None);
        let e = format!("{:#}", r.unwrap_err());
        assert!(
            e.contains("merge-lint-undetermined") && e.contains("no `cargo`"),
            "{e}"
        );
    }

    #[test]
    fn cargo_that_cannot_spawn_refuses() {
        let (_t, repo, base) = ws_repo();
        let tip = commit(&repo, &[("crates/a/src/lib.rs", "pub fn f() {}\n")]);
        let r = ensure_tip_lint_clean_with(
            &repo,
            "b",
            "main",
            &base,
            &tip,
            None,
            Some(PathBuf::from("/nonexistent/cargo")),
        );
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("merge-lint-undetermined"), "{e}");
        // The temporary lint checkout is gone again.
        let listing = sh(&repo, &["worktree", "list", "--porcelain"]);
        assert_eq!(listing.matches("worktree ").count(), 1, "{listing}");
    }
}
