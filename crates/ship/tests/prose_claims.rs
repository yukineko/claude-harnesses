//! Claim-reproduction tests for the prose in README.md / README.ja.md /
//! skills/ship/SKILL.md. One `claim_<id>` test per claim, driving the real
//! `ship` binary against throwaway git repos (HOME and git config isolated).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(dir, args).stdout).into_owned()
}

/// A temp git repo on `main` with one commit (including a crates/foo crate).
struct Fixture {
    _tmp: TempDir,
    home: TempDir,
    repo: PathBuf,
}

fn set_mtime(p: &Path, t: SystemTime) {
    // read-only open works for directories too; futimens needs ownership only
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let repo = tmp.path().canonicalize().unwrap().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::create_dir_all(repo.join("crates/foo/src")).unwrap();
    fs::create_dir_all(repo.join("crates/foo/bin")).unwrap();
    fs::write(repo.join("crates/foo/src/lib.rs"), "// code\n").unwrap();
    fs::write(repo.join("crates/foo/bin/foo-linux-x86_64"), "bin").unwrap();
    // bin is NEWER than src => not stale at baseline
    let now = SystemTime::now();
    set_mtime(
        &repo.join("crates/foo/src/lib.rs"),
        now - Duration::from_secs(100),
    );
    set_mtime(
        &repo.join("crates/foo/bin/foo-linux-x86_64"),
        now - Duration::from_secs(50),
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    Fixture {
        _tmp: tmp,
        home,
        repo,
    }
}

fn ship_cmd(f: &Fixture, cwd: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ship"));
    c.args(args)
        .current_dir(cwd)
        .env("HOME", f.home.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c
}

fn ship(f: &Fixture, cwd: &Path, args: &[&str]) -> String {
    let out = ship_cmd(f, cwd, args).output().expect("ship runs");
    assert!(out.status.success(), "ship {args:?} exit {:?}", out.status);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn line_with<'a>(out: &'a str, needle: &str) -> &'a str {
    out.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{out}"))
}

#[cfg(unix)]
fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Logical, non-comment lines of a shell/python source: backslash continuations
/// are joined, `#` comment lines are dropped. Returns (first line number, text).
fn logical_lines(src: &str) -> Vec<(usize, String)> {
    let mut out = vec![];
    let mut cur: Option<(usize, String)> = None;
    for (i, l) in src.lines().enumerate() {
        if cur.is_none() && l.trim_start().starts_with('#') {
            continue;
        }
        let (n, mut acc) = cur.take().unwrap_or((i + 1, String::new()));
        if let Some(stripped) = l.strip_suffix('\\') {
            acc.push_str(stripped);
            acc.push(' ');
            cur = Some((n, acc));
        } else {
            acc.push_str(l);
            out.push((n, acc));
        }
    }
    if let Some((n, acc)) = cur {
        out.push((n, acc));
    }
    out
}

fn tokens(line: &str) -> Vec<&str> {
    line.split(|c: char| c.is_whitespace() || ";|&()[],\"'`".contains(c))
        .filter(|t| !t.is_empty())
        .collect()
}

fn is_git_token(t: &str) -> bool {
    let base = t.rsplit('/').next().unwrap_or(t).to_ascii_lowercase();
    matches!(base.as_str(), "git" | "$git" | "${git}")
}

/// Every git subcommand invoked on this line. Handles `git`, `"git"`,
/// `/usr/bin/git`, `$GIT`, `"$GIT"`, python `["git", ...]`, and global options
/// (`-C d`, `-c k=v`, `--git-dir d`, `--work-tree d`, `--opt=v`) before the
/// subcommand. A subcommand that is itself a variable is reported as-is.
fn git_subcommands(line: &str) -> Vec<String> {
    let toks = tokens(line);
    let mut subs = vec![];
    for (i, t) in toks.iter().enumerate() {
        if !is_git_token(t) {
            continue;
        }
        let mut j = i + 1;
        while j < toks.len() {
            match toks[j] {
                "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
                | "--config-env" => j += 2,
                s if s.starts_with('-') => j += 1,
                s => {
                    subs.push(s.to_string());
                    break;
                }
            }
        }
    }
    subs
}

const MUTATING: &[&str] = &[
    "commit",
    "merge",
    "push",
    "add",
    "checkout",
    "reset",
    "rebase",
    "cherry-pick",
    "revert",
    "update-ref",
    "commit-tree",
    "notes",
    "stash",
    "tag",
    "rm",
    "mv",
    "switch",
    "restore",
    "apply",
    "am",
    "pull",
    "update-index",
    "clean",
    "fetch",
];

fn is_mutating_sub(s: &str) -> bool {
    // a subcommand we cannot resolve (variable) is cannot-determine => flagged
    s.starts_with('$') || MUTATING.contains(&s)
}

fn scan_source(src: &str) -> (Vec<String>, usize) {
    let mut bad = vec![];
    let mut seen = 0;
    for (n, l) in logical_lines(src) {
        for s in git_subcommands(&l) {
            seen += 1;
            if is_mutating_sub(&s) {
                bad.push(format!("{n}: {l}"));
            }
        }
    }
    (bad, seen)
}

#[test]
fn claim_scripts_no_mutating_git() {
    // scanner RED inputs: each form must be flagged
    let forms = [
        "\"$GIT\" commit -m x",
        "$GIT push origin main",
        "\"git\" commit -m x",
        "/usr/bin/git commit -m x",
        "git --git-dir \"$d\" commit -m x",
        "git --work-tree \"$d\" add .",
        "subprocess.run([\"git\", \"commit\"])",
        "git \\\n   commit -m x",
        "git -C \"$R\" -c user.name=x commit -m x",
        "git --git-dir=\"$d\" commit",
    ];
    for f in forms {
        assert_eq!(scan_source(f).0.len(), 1, "scanner missed form: {f:?}");
    }
    for sub in MUTATING {
        assert_eq!(
            scan_source(&format!("git {sub} x\n")).0.len(),
            1,
            "subcommand {sub} not flagged"
        );
    }
    // read-only and comment forms are not flagged
    let (bad, seen) = scan_source("git -C \"$R\" rev-parse HEAD\n# git commit\ngit status\n");
    assert!(bad.is_empty() && seen == 2, "{bad:?} {seen}");

    for name in ["rebuild-plugins.sh", "rollout-plugins.sh"] {
        let src = fs::read_to_string(repo_root().join("scripts").join(name)).unwrap();
        let (bad, seen) = scan_source(&src);
        assert!(seen > 0, "{name}: scanner saw no git invocations at all");
        assert!(
            bad.is_empty(),
            "{name} has mutating git:\n{}",
            bad.join("\n")
        );
    }
}

#[test]
fn claim_check_lists_categories() {
    let f = fixture();
    let r = f.repo.clone();
    // real bare remote + upstream, so "0 unpushed" is a successful measurement
    let remote = r.parent().unwrap().join("remote.git");
    git(
        r.parent().unwrap(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(&r, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&r, &["push", "-q", "-u", "origin", "main"]);
    assert_eq!(
        git_out(&r, &["rev-list", "--count", "@{upstream}..HEAD"]).trim(),
        "0"
    );

    // baseline: all five categories present, all [x]
    let out = ship(&f, &r, &["check"]);
    for needle in [
        "uncommitted changes",
        "unmerged condukt/* branches",
        "leftover worktrees",
        "unpushed commits",
        "stale plugin binaries",
    ] {
        assert!(
            line_with(&out, needle).contains("[x]"),
            "baseline {needle}: {out}"
        );
    }

    // dirty file
    fs::write(r.join("dirty.txt"), "x").unwrap();
    let out = ship(&f, &r, &["check"]);
    assert!(
        line_with(&out, "uncommitted changes").contains("[ ]"),
        "{out}"
    );
    fs::remove_file(r.join("dirty.txt")).unwrap();

    // condukt/x branch with a commit not on main
    git(&r, &["branch", "condukt/merged"]); // fully merged: must NOT be listed
    git(&r, &["checkout", "-q", "-b", "condukt/x"]);
    fs::write(r.join("c.txt"), "c").unwrap();
    git(&r, &["add", "c.txt"]);
    git(&r, &["commit", "-q", "-m", "c"]);
    git(&r, &["checkout", "-q", "main"]);
    let out = ship(&f, &r, &["check"]);
    let l = line_with(&out, "unmerged condukt/* branches");
    assert!(l.contains("[ ]") && l.contains("condukt/x"), "{out}");
    assert!(!l.contains("condukt/merged"), "{out}");

    // extra worktree
    let wt = r.parent().unwrap().join("wt");
    git(
        &r,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    let out = ship(&f, &r, &["check"]);
    let l = line_with(&out, "leftover worktrees");
    assert!(l.contains("[ ]") && l.contains("wt"), "{out}");

    // one commit ahead of the upstream
    fs::write(r.join("u.txt"), "u").unwrap();
    git(&r, &["add", "u.txt"]);
    git(&r, &["commit", "-q", "-m", "u"]);
    assert_eq!(
        git_out(&r, &["rev-list", "--count", "@{upstream}..HEAD"]).trim(),
        "1"
    );
    let out = ship(&f, &r, &["check"]);
    let l = line_with(&out, "unpushed commit(s)");
    assert!(l.contains("[ ]") && l.contains('1'), "{out}");

    // stale: src newer than bin
    set_mtime(
        &r.join("crates/foo/src/lib.rs"),
        SystemTime::now() + Duration::from_secs(60),
    );
    let out = ship(&f, &r, &["check"]);
    let l = line_with(&out, "stale plugin binaries");
    assert!(l.contains("[ ]") && l.contains("foo"), "{out}");
}

#[test]
fn claim_check_does_not_modify() {
    let f = fixture();
    let r = f.repo.clone();
    // touch a committed file (stat cache goes stale; content identical)
    set_mtime(
        &r.join("crates/foo/src/lib.rs"),
        SystemTime::now() + Duration::from_secs(5),
    );
    // dirty tracked file, staged new file, untracked file
    fs::write(r.join("crates/foo/bin/foo-linux-x86_64"), "dirty").unwrap();
    fs::write(r.join("staged.txt"), "s").unwrap();
    git(&r, &["add", "staged.txt"]);
    fs::write(r.join("untracked.txt"), "u").unwrap();

    let snap = |r: &Path| {
        (
            git_out(r, &["ls-files", "-s"]),
            git_out(r, &["diff", "--cached"]),
            git_out(r, &["rev-parse", "HEAD"]),
            git_out(r, &["status", "--porcelain"]),
            [
                "crates/foo/src/lib.rs",
                "crates/foo/bin/foo-linux-x86_64",
                "staged.txt",
                "untracked.txt",
            ]
            .map(|p| fs::read(r.join(p)).unwrap()),
        )
    };
    let before = snap(&r);
    assert!(
        before.1.contains("staged.txt"),
        "fixture has staged content"
    );
    ship(&f, &r, &["check"]);
    let after = snap(&r);
    assert_eq!(before.0, after.0, "ls-files -s changed");
    assert_eq!(before.1, after.1, "staged diff changed");
    assert_eq!(before.2, after.2, "HEAD changed");
    assert_eq!(before.3, after.3, "porcelain status changed");
    assert_eq!(before.4, after.4, "working tree contents changed");
}

#[cfg(unix)]
fn stub(f: &Fixture, script: &str, marker: &Path) {
    write_exec(
        &f.repo.join("scripts").join(script),
        &format!(
            "#!/bin/sh\nprintf 'argc=%s\\n' \"$#\" >> '{m}'\nfor a in \"$@\"; do printf 'arg=%s\\n' \"$a\" >> '{m}'; done\nprintf 'pwd=%s\\n' \"$(pwd -P)\" >> '{m}'\n",
            m = marker.display()
        ),
    );
}

#[cfg(unix)]
#[test]
fn claim_run_safe_runs_rebuild_stage_repo() {
    let f = fixture();
    let marker = f.home.path().join("rebuild.log");
    stub(&f, "rebuild-plugins.sh", &marker);
    // commit the stub so the only uncommitted thing is the file added below
    git(&f.repo, &["add", "scripts"]);
    git(&f.repo, &["commit", "-q", "-m", "stub"]);
    fs::write(f.repo.join("dirty.txt"), "d").unwrap();
    let sub = f.repo.join("crates/foo/src");
    let out = ship(&f, &sub, &["check", "--run-safe"]);
    let log = fs::read_to_string(&marker).expect("rebuild stub ran");
    assert_eq!(
        log,
        format!("argc=1\narg=--stage-repo\npwd={}\n", f.repo.display())
    );
    let (_, after) = out
        .split_once("remaining GATED items:")
        .unwrap_or_else(|| panic!("no 'remaining GATED items:' in:\n{out}"));
    assert!(
        after.contains("uncommitted changes present — commit (GATED"),
        "reprint lacks the gated item:\n{after}"
    );
}

#[cfg(unix)]
#[test]
fn claim_rollout_runs_script_no_args() {
    let f = fixture();
    let marker = f.home.path().join("rollout.log");
    stub(&f, "rollout-plugins.sh", &marker);
    let sub = f.repo.join("crates/foo/src");
    ship(&f, &sub, &["rollout"]);
    let log = fs::read_to_string(&marker).expect("rollout stub ran");
    assert_eq!(log, format!("argc=0\npwd={}\n", f.repo.display()));
}

#[cfg(unix)]
#[test]
fn claim_run_safe_and_rollout_separate() {
    let f = fixture();
    let rb = f.home.path().join("rebuild.log");
    let ro = f.home.path().join("rollout.log");
    stub(&f, "rebuild-plugins.sh", &rb);
    stub(&f, "rollout-plugins.sh", &ro);
    ship(&f, &f.repo, &["check", "--run-safe"]);
    assert!(rb.exists(), "run-safe must run rebuild");
    assert!(!ro.exists(), "run-safe must NOT run rollout");
    fs::remove_file(&rb).unwrap();
    ship(&f, &f.repo, &["rollout"]);
    assert!(ro.exists(), "rollout must run rollout script");
    assert!(!rb.exists(), "rollout must NOT itself run rebuild");
}

#[cfg(unix)]
#[test]
fn claim_binary_runs_only_readonly_git() {
    let f = fixture();
    stub(&f, "rebuild-plugins.sh", &f.home.path().join("rb.log"));
    stub(&f, "rollout-plugins.sh", &f.home.path().join("ro.log"));
    let which = Command::new("which").arg("git").output().unwrap();
    assert!(which.status.success());
    let real_git = String::from_utf8(which.stdout).unwrap().trim().to_string();
    let shim_dir = f.home.path().join("shim");
    let orig_path = std::env::var("PATH").unwrap();

    for (i, args) in [
        &["check"][..],
        &["check", "--run-safe"][..],
        &["rollout"][..],
    ]
    .iter()
    .enumerate()
    {
        let log = f.home.path().join(format!("gitlog{i}.txt"));
        write_exec(
            &shim_dir.join("git"),
            &format!(
                "#!/bin/sh\n{{ echo CALL; for a in \"$@\"; do echo \"$a\"; done; }} >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                real_git
            ),
        );
        let out = ship_cmd(&f, &f.repo, args)
            .env("PATH", format!("{}:{orig_path}", shim_dir.display()))
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        let text = fs::read_to_string(&log).unwrap_or_default();
        let calls: Vec<Vec<&str>> = text
            .split("CALL\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().collect())
            .collect();
        assert!(!calls.is_empty(), "{args:?}: no git calls logged");
        for call in &calls {
            let mut j = 0;
            while j < call.len() && call[j].starts_with('-') {
                j += if matches!(call[j], "-C" | "-c" | "--git-dir" | "--work-tree") {
                    2
                } else {
                    1
                };
            }
            let sub = call.get(j).copied().unwrap_or("");
            let ok = match sub {
                "status" | "rev-list" | "rev-parse" => true,
                "branch" => call.contains(&"--list"),
                "worktree" => call.get(j + 1) == Some(&"list"),
                _ => false,
            };
            assert!(ok, "{args:?}: non-read-only git call: {call:?}");
        }
        if args.len() == 1 && args[0] == "check" {
            assert!(
                calls.iter().any(|c| c.contains(&"status")),
                "check must have called git status: {calls:?}"
            );
        }
    }
}

/// Scan a rollout-style script for rebuild-plugins.sh invocations.
/// Returns (invocations, invocations that may pass --stage-repo). An argument
/// that is a bare variable expansion cannot be resolved here, so it counts as
/// "may pass" (cannot-determine resolves to the restrictive side).
fn rollout_rebuild_invocations(src: &str) -> (usize, usize) {
    let lines = logical_lines(src);
    let mut vars: Vec<String> = vec![];
    for (_, l) in &lines {
        let t = l.trim_start().trim_start_matches("export ").trim_start();
        let t = t.strip_prefix("local ").unwrap_or(t);
        if let Some((name, val)) = t.split_once('=') {
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && val.split_whitespace().count() == 1
                && val.contains("rebuild-plugins.sh")
            {
                vars.push(name.to_string());
            }
        }
    }
    let is_assign = |l: &str| {
        let t = l.trim_start();
        vars.iter().any(|v| t.starts_with(&format!("{v}=")))
    };
    let (mut inv, mut may_stage) = (0, 0);
    for (_, l) in &lines {
        let t = l.trim_start();
        if t.starts_with("echo") || is_assign(l) {
            continue;
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        let Some(pos) = toks.iter().position(|tok| {
            tok.contains("rebuild-plugins.sh")
                || vars
                    .iter()
                    .any(|v| tok.contains(&format!("${v}")) || tok.contains(&format!("${{{v}}}")))
        }) else {
            continue;
        };
        inv += 1;
        let rest = &toks[pos + 1..];
        let bare_var = |a: &&str| {
            let a = a.trim_matches('"');
            a.starts_with('$')
                && a[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_{}@*".contains(c))
        };
        if l.contains("--stage-repo") || rest.iter().any(bare_var) {
            may_stage += 1;
        }
    }
    (inv, may_stage)
}

/// True iff the (single) non-comment `cp -f "$src" "$repofile"` is enclosed,
/// on its `then` side, by exactly `if [ $stage_repo = 1 ]; then`.
fn repo_copy_only_under_stage_repo(src: &str) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    let cps: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            !l.trim_start().starts_with('#') && l.contains("cp -f \"$src\" \"$repofile\"")
        })
        .map(|(i, _)| i)
        .collect();
    if cps.len() != 1 {
        return false;
    }
    let indent = |l: &str| l.len() - l.trim_start().len();
    let opener = |t: &str| {
        ["if ", "for ", "while ", "case ", "until "]
            .iter()
            .any(|p| t.starts_with(p))
    };
    let mut cur = indent(lines[cps[0]]);
    let mut tainted: Option<usize> = None; // indent at which we crossed else/elif
    let mut skip: Option<usize> = None; // indent of a closed block being skipped
    for l in lines[..cps[0]].iter().rev() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let ind = indent(l);
        if let Some(s) = skip {
            if ind == s && opener(t) {
                skip = None;
            }
            continue;
        }
        if ind >= cur {
            continue;
        }
        if matches!(t, "fi" | "done" | "esac") {
            skip = Some(ind);
            continue;
        }
        if t == "else" || t.starts_with("elif ") {
            tainted = Some(ind);
            cur = ind;
            continue;
        }
        if opener(t) {
            if tainted == Some(ind) {
                tainted = None;
            } else if t == "if [ $stage_repo = 1 ]; then" {
                return true;
            }
            cur = ind;
        }
    }
    false
}

#[test]
fn claim_rollout_script_no_stage_repo() {
    // scanner RED inputs: each must be seen as an invocation that may pass --stage-repo
    let bad_inputs = [
        "RB=\"$REPO/scripts/rebuild-plugins.sh\"\n\"$RB\" --stage-repo\n",
        "bash \"$REPO/scripts/rebuild-plugins.sh\" --no-clean \\\n   --stage-repo\n",
        "STAGE=--stage-repo\nbash scripts/rebuild-plugins.sh $STAGE\n",
        "bash scripts/rebuild-plugins.sh --stage-repo\n",
    ];
    for s in bad_inputs {
        assert_eq!(rollout_rebuild_invocations(s), (1, 1), "missed: {s:?}");
    }
    assert_eq!(
        rollout_rebuild_invocations("RB=x/rebuild-plugins.sh\n\"$RB\" --no-clean\n"),
        (1, 0)
    );
    let (inv, may_stage) = rollout_rebuild_invocations(
        &fs::read_to_string(repo_root().join("scripts/rollout-plugins.sh")).unwrap(),
    );
    assert!(inv > 0, "no rebuild-plugins.sh invocation found");
    assert_eq!(may_stage, 0, "an invocation may pass --stage-repo");

    // guard analysis RED inputs
    let cp = "cp -f \"$src\" \"$repofile\"";
    let ok = format!("  if [ $stage_repo = 1 ]; then\n    {cp}\n  fi\n");
    assert!(repo_copy_only_under_stage_repo(&ok));
    let or_true = format!("  if [ $stage_repo = 1 ] || true; then\n    {cp}\n  fi\n");
    let in_else = format!("  if [ $stage_repo = 1 ]; then\n    :\n  else\n    {cp}\n  fi\n");
    let closed = format!("  if [ $stage_repo = 1 ]; then\n    :\n  fi\n  {cp}\n");
    let bare = format!("  {cp}\n");
    let twice = format!("{ok}{ok}");
    for (name, s) in [
        ("or true", &or_true),
        ("else branch", &in_else),
        ("closed guard", &closed),
        ("unguarded", &bare),
        ("two copy sites", &twice),
    ] {
        assert!(!repo_copy_only_under_stage_repo(s), "accepted: {name}");
    }
    let rb = fs::read_to_string(repo_root().join("scripts/rebuild-plugins.sh")).unwrap();
    assert!(
        repo_copy_only_under_stage_repo(&rb),
        "rebuild-plugins.sh repo copy is not solely under --stage-repo"
    );
}

#[test]
fn claim_hooks_json_sessionend() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
    let hooks = v["hooks"].as_object().expect("hooks object");
    assert_eq!(hooks.keys().collect::<Vec<_>>(), vec!["SessionEnd"]);
    let groups = hooks["SessionEnd"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    let cmds = groups[0]["hooks"].as_array().unwrap();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["type"], "command");
    assert_eq!(
        cmds[0]["command"],
        "${CLAUDE_PLUGIN_ROOT}/bin/ship session-end"
    );
}

#[test]
fn claim_skill_ship_exists() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/ship/SKILL.md");
    let s = fs::read_to_string(p).unwrap();
    let mut it = s.lines();
    assert_eq!(it.next(), Some("---"));
    let fm: Vec<&str> = it.take_while(|l| *l != "---").collect();
    assert!(fm.contains(&"name: ship"), "front matter: {fm:?}");
}

#[test]
fn claim_platform_bins_gitignored() {
    let ignored = |p: &str| {
        Command::new("git")
            .args(["check-ignore", "-q", p])
            .current_dir(repo_root())
            .output()
            .expect("git runs")
            .status
            .code()
    };
    for p in [
        "crates/ship/bin/ship-linux-x86_64",
        "crates/zzz/bin/zzz-linux-x86_64",
        "crates/a-b/bin/x-linux-x86_64",
    ] {
        assert_eq!(ignored(p), Some(0), "{p} not ignored");
    }
    // control: the tracked wrapper dispatcher is NOT ignored (exit 1)
    assert_eq!(ignored("crates/ship/bin/ship"), Some(1));
}

#[test]
fn claim_stale_newest_nested_src() {
    let f = fixture();
    let r = f.repo.clone();
    let bar = r.join("crates/bar");
    fs::create_dir_all(bar.join("src/sub")).unwrap();
    fs::create_dir_all(bar.join("bin")).unwrap();
    fs::write(bar.join("src/a.rs"), "a").unwrap();
    fs::write(bar.join("bin/bar-linux-x86_64"), "bin").unwrap();
    fs::write(bar.join("src/sub/b.rs"), "b").unwrap();
    let now = SystemTime::now();
    let t = |s: u64| now - Duration::from_secs(s);
    // directories (their own mtimes are scanned too) and a.rs oldest, then bin,
    // and the nested file newest
    for (p, secs) in [
        ("src/a.rs", 300),
        ("src/sub", 300),
        ("src", 300),
        ("bin/bar-linux-x86_64", 200),
        ("src/sub/b.rs", 100),
    ] {
        set_mtime(&bar.join(p), t(secs));
    }
    let out = ship(&f, &r, &["check"]);
    let l = line_with(&out, "stale plugin binaries");
    assert!(l.contains("[ ]") && l.contains("bar"), "{out}");
    assert!(!l.contains("foo"), "foo is not stale: {out}");
}

#[test]
fn claim_license_mit() {
    let s = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let pkg = s
        .split("[package]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    assert!(
        pkg.lines().any(|l| l.trim() == "license = \"MIT\""),
        "{pkg}"
    );
}
