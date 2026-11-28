//! User ruling 2026-11-28 (「driverで repo スクリプトを許可」, backlog b6726716):
//! in a live `/flow` driver session, the repo's own committed, clean
//! `scripts/` files get no Table B.5 objection, like the plugin cache `bin/`
//! (backlog a50833d7). Every probe here is git, bounded, and an undecidable
//! answer is `false` (the Ask / driver Deny stands).

use harness_core::verdict::Determination;

use super::{
    basename, git_tracked_clean, injects_beyond_system_path, literal, sets_var_where, FdTarget,
    RedirOp, RedirTarget, SimpleCommand, Value, Word, READ_ONLY,
};

/// One bounded (3 s) read-only git probe run in `dir`; must exit 0.
pub(super) fn git_probe(dir: &str, args: &[&str]) -> Determination<String> {
    let mut cmd = std::process::Command::new("git");
    harness_core::git_env::scrub(&mut cmd).arg("-C").arg(dir);
    cmd.args([
        "--no-optional-locks",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
    ]);
    cmd.args(args);
    match harness_core::boundary::run_with_timeout(&mut cmd, std::time::Duration::from_millis(3000))
    {
        Determination::Known(out) => out.stdout_allowing(&[0]),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// A canonical git path printed by `args` run in `dir`, or the undetermined
/// reason.
fn git_path(scope: &crate::scope::SafeRoots, dir: &str, args: &[&str]) -> Determination<String> {
    match git_probe(dir, args) {
        Determination::Known(t) => match scope.resolve(t.trim_end_matches('\n')) {
            Some(p) => Determination::Known(p),
            None => Determination::undetermined("git path does not resolve"),
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// `real` (canonical) is a file under `scripts/` of a work tree (the
/// session's own or the main one) of the SAME repository as the hook's cwd
/// (equal git common dir), tracked and clean ([`git_tracked_clean`], asked
/// from the file's own work tree), in a driver session. Anything undecidable
/// is `false`.
pub(super) fn driver_repo_script(
    scope: &crate::scope::SafeRoots,
    hook_cwd: &str,
    real: &str,
) -> bool {
    if !scope.is_driver_session() {
        return false;
    }
    let Some((dir, _)) = real.rsplit_once('/') else {
        return false;
    };
    let Determination::Known(top) = git_path(scope, dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let Some(rel) = real.strip_prefix(&format!("{}/", top.trim_end_matches('/'))) else {
        return false;
    };
    if !rel.starts_with("scripts/") {
        return false;
    }
    let common_args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    let Determination::Known(mine) = git_path(scope, hook_cwd, &common_args) else {
        return false;
    };
    let Determination::Known(theirs) = git_path(scope, dir, &common_args) else {
        return false;
    };
    mine == theirs && git_tracked_clean(scope, dir, real)
}

/// Is every file under `<top>/scripts` clean: no staged / unstaged change, no
/// untracked file, and no git-ignored `conftest.py` / pytest config that
/// pytest would still load? `false` on anything undecidable.
fn scripts_tree_clean(top: &str) -> bool {
    let Determination::Known(status) = git_probe(
        top,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--",
            "scripts",
        ],
    ) else {
        return false;
    };
    if !status.is_empty() {
        return false;
    }
    let Determination::Known(ignored) = git_probe(
        top,
        &[
            "ls-files",
            "-o",
            "-i",
            "--exclude-standard",
            "-z",
            "--",
            "scripts",
        ],
    ) else {
        return false;
    };
    !ignored.split('\0').any(|f| {
        matches!(
            basename(f),
            "conftest.py" | "pytest.ini" | "tox.ini" | "setup.cfg" | "pyproject.toml"
        ) || f.ends_with(".pth")
    })
}

/// Pytest options that take no operand and load no code. Anything else
/// (`-p`, `-c`, `--rootdir`, `-o`, `--confcutdir`, ...) is not trusted.
fn safe_pytest_flag(v: &str) -> bool {
    matches!(
        v,
        "-q" | "-qq" | "-v" | "-vv" | "-x" | "-s" | "-l" | "-ra" | "-rA" | "-rf" | "-rs"
    ) || v.starts_with("--tb=")
        || matches!(v, "--no-header" | "--maxfail=1" | "--color=no")
}

/// `python3 -m pytest [safe flags] FILE...` in a live `/flow` driver session,
/// where every FILE (a `::node` suffix allowed) is a [`driver_repo_script`]
/// and the whole `scripts/` tree of its repository is clean
/// ([`scripts_tree_clean`]: pytest also runs `conftest.py`). The rest of the
/// line may not write anything (every other command is a [`READ_ONLY`] name
/// with no path redirect) and no injecting / `PYTEST_*` variable may be set.
/// Anything undecidable is `false`.
pub(super) fn driver_pytest_trusted(
    commands: &[SimpleCommand],
    run: usize,
    args: &[Word],
    scope: &crate::scope::SafeRoots,
) -> bool {
    if !scope.is_driver_session() {
        return false;
    }
    let Some(run_cmd) = commands.get(run) else {
        return false;
    };
    let Some(hook_cwd) = scope.session_cwd() else {
        return false;
    };
    if sets_var_where(commands, |n, v| {
        injects_beyond_system_path(n, v) || n.starts_with("PYTEST_")
    }) {
        return false;
    }
    for (j, c) in commands.iter().enumerate() {
        if c.redirects.iter().any(|r| {
            matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
                    | RedirOp::DupOut
            ) && !matches!(r.target, RedirTarget::Fd(FdTarget::Number(_)))
        }) {
            return false;
        }
        if j == run {
            continue;
        }
        let Some(name) = c.argv.first().and_then(|w| literal(w).ok()) else {
            return false;
        };
        if name.contains('/') || !READ_ONLY.contains(&name) {
            return false;
        }
    }
    let [m, pytest, rest @ ..] = args else {
        return false;
    };
    if literal(m).ok() != Some("-m") || literal(pytest).ok() != Some("pytest") {
        return false;
    }
    let mut files = 0;
    for w in rest {
        let Ok(v) = literal(w) else { return false };
        if v.starts_with('-') {
            if !safe_pytest_flag(v) {
                return false;
            }
            continue;
        }
        let file = v.split("::").next().unwrap_or(v);
        let resolved =
            super::super::eval::resolve_at(&run_cmd.cwd, Value::Literal(file.to_string()));
        let Value::Literal(p) = resolved else {
            return false;
        };
        let Determination::Known(lex) = scope.lexical_absolute(&p, Some(hook_cwd)) else {
            return false;
        };
        let Some(real) = scope.resolve(&lex) else {
            return false;
        };
        if !real.ends_with(".py") || !driver_repo_script(scope, hook_cwd, &real) {
            return false;
        }
        let Some((dir, _)) = real.rsplit_once('/') else {
            return false;
        };
        let Determination::Known(top) = git_path(scope, dir, &["rev-parse", "--show-toplevel"])
        else {
            return false;
        };
        if !scripts_tree_clean(&top) {
            return false;
        }
        files += 1;
    }
    files > 0
}
