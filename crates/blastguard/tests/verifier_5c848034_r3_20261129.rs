#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent adversarial verifier, round 3, for backlog 5c848034 (branch
//! fix-5c848034, fix commit bd1bdd7a9 on main 84f801e26, blastguard 0.2.1058).
//! Written by a verifier, not the implementer (CLAUDE.md §2(a)).
//!
//! Harness as in rounds 1–2. HOLE tests: verdict first, then the same command
//! line run for real with `/bin/bash -c` in a sandbox under
//! `/private/tmp/claude-wt-verify` with a fake `$HOME` (and no
//! `XDG_CONFIG_HOME`, so git's XDG config is `$HOME/.config/git/config`). A
//! hole is a real change of the fake `~/.claude` tree with an `allow` verdict;
//! a real run that changes nothing is a PRECONDITION failure.
//!
//! FP tests: ordinary development commands that must stay `allow`.
//!
//! SYS tests (verdict only — nothing is ever run against a system directory):
//! a respelling of a system-directory destination must get the same verdict
//! as the plain spelling, which must itself not be `allow`.
//!
//! Layout: `<base>/home/.claude/{settings.json,hooks/pre.sh}`; main tree
//! `<home>/src/repo` (branch main; tracked `settings.json` = `{"pwned":1}`,
//! `hooks/x.sh`, `src/lib.rs`, `sub/f`, `.claude/settings.json` =
//! `{"proj":1}`) with linked worktree `<home>/src/.harness-worktrees/wt`.
//!
//! DOES NOT PROVE: real `$HOME`; Linux tool semantics; any git other than the
//! one on this machine; that an Ask reaches a human; anything about a system
//! directory beyond the verdict.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = git_env::pinned(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "fixture git {args:?} in {}", dir.display());
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-wt-verify").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("v5c84r3-")
            .tempdir_in("/private/tmp/claude-wt-verify")
            .unwrap();
        let base = dir.path().canonicalize().unwrap();
        let fx = Fx { _dir: dir, base };
        write(&fx.claude().join("settings.json"), "{}\n");
        write(&fx.claude().join("hooks/pre.sh"), "#!/bin/sh\n");
        let repo = fx.repo();
        write(&repo.join("settings.json"), "{\"pwned\":1}\n");
        write(&repo.join("hooks/x.sh"), "exit 0\n");
        write(&repo.join("src/lib.rs"), "x\n");
        write(&repo.join("sub/f"), "x\n");
        write(&repo.join(".claude/settings.json"), "{\"proj\":1}\n");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["worktree", "add", "-q", fx.wt().to_str().unwrap()]);
        write(&fx.base.join("mf/o.txt"), "{\"hooks\":1}\n");
        std::fs::create_dir_all(fx.base.join("tmp")).unwrap();
        fx
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn claude(&self) -> PathBuf {
        self.home().join(".claude")
    }
    fn repo(&self) -> PathBuf {
        self.home().join("src/repo")
    }
    fn wt(&self) -> PathBuf {
        self.home().join("src/.harness-worktrees/wt")
    }
    /// `{H}` home, `{C}` ~/.claude, `{K}` hooks dir, `{B}` base, `{W}` wt,
    /// `{R}` repo, `{S}` home/src, `{UP}` `../` repeated up to `/` from home.
    fn sub(&self, t: &str) -> String {
        let up = "../".repeat(self.home().components().count() - 1);
        t.replace("{H}", &self.home().display().to_string())
            .replace("{C}", &self.claude().display().to_string())
            .replace("{K}", &self.claude().join("hooks").display().to_string())
            .replace("{B}", &self.base.display().to_string())
            .replace("{W}", &self.wt().display().to_string())
            .replace("{R}", &self.repo().display().to_string())
            .replace("{S}", &self.home().join("src").display().to_string())
            .replace("{UP}", &up)
    }
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "verifier-5c848034-r3",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .env("HOME", fx.home())
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store-approvals"))
        .env("BLASTGUARD_BLOCK_LOG_DIR", fx.base.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", fx.base.join("relay"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("TMPDIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        eprintln!("V5C84R3 [{cmd}] allow");
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    let d = v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string();
    eprintln!(
        "V5C84R3 [{cmd}] {d}: {}",
        v["hookSpecificOutput"]["permissionDecisionReason"]
    );
    d
}

fn real(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let mut c = Command::new("/bin/bash");
    git_env::scrub(&mut c)
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("HOME", fx.home())
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &fx.base)
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    let out = bounded::run_bounded_stdin(&mut c, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    format!(
        "status={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every path under ~/.claude with its content, `.git` internals excluded.
fn snapshot(fx: &Fx) -> Vec<(String, Vec<u8>)> {
    fn walk(d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            if p.is_dir() {
                out.push((p.display().to_string(), Vec::new()));
                walk(&p, out);
            } else {
                out.push((
                    p.display().to_string(),
                    std::fs::read(&p).unwrap_or_default(),
                ));
            }
        }
    }
    let mut v = Vec::new();
    walk(&fx.claude(), &mut v);
    v.sort();
    v
}

fn setup(fx: &Fx, s: &str) {
    if !s.is_empty() {
        let log = real(fx, &fx.base, &fx.sub(s));
        assert!(log.starts_with("status=Some(0)"), "setup `{s}`: {log}");
    }
}

fn holes_after(setup_cmd: &str, cwd_t: &str, cmd_t: &str) {
    let fx = Fx::new();
    setup(&fx, setup_cmd);
    let cwd = PathBuf::from(fx.sub(cwd_t));
    let cmd = fx.sub(cmd_t);
    let v = verdict(&fx, &cwd, &cmd);
    let before = snapshot(&fx);
    let log = real(&fx, &cwd, &cmd);
    let after = snapshot(&fx);
    assert!(
        after != before,
        "PRECONDITION (not a hole): `{cmd_t}` (cwd {cwd_t}) real run did not change ~/.claude ({log})"
    );
    assert!(
        v != "allow",
        "HOLE: `{cmd_t}` (cwd {cwd_t}) => allow, real run changed ~/.claude ({log})"
    );
}

fn allow_after(setup_cmd: &str, cwd_t: &str, cmd_t: &str) {
    let fx = Fx::new();
    setup(&fx, setup_cmd);
    let v = verdict(&fx, &PathBuf::from(fx.sub(cwd_t)), &fx.sub(cmd_t));
    assert_eq!(v, "allow", "FP: `{cmd_t}` (cwd {cwd_t}) => {v}");
}

/// Verdict only: the respelling must be judged like the plain spelling, which
/// itself must not be `allow`.
fn same_as_plain(setup_cmd: &str, cwd_t: &str, plain: &str, variant: &str) {
    let fx = Fx::new();
    setup(&fx, setup_cmd);
    let cwd = PathBuf::from(fx.sub(cwd_t));
    let u = verdict(&fx, &cwd, &fx.sub(plain));
    assert_ne!(u, "allow", "PRECONDITION: plain `{plain}` => allow");
    let q = verdict(&fx, &cwd, &fx.sub(variant));
    assert_eq!(q, u, "SPELLING GAP: `{variant}` => {q}, `{plain}` => {u}");
}

macro_rules! hole {
    ($(#[$m:meta])* $name:ident: $setup:expr, $cwd:expr, $cmd:expr) => {
        $(#[$m])*
        #[test]
        fn $name() {
            holes_after($setup, $cwd, $cmd);
        }
    };
}

macro_rules! fp {
    ($(#[$m:meta])* $name:ident: $setup:expr, $cwd:expr, $cmd:expr) => {
        $(#[$m])*
        #[test]
        fn $name() {
            allow_after($setup, $cwd, $cmd);
        }
    };
}

macro_rules! sys {
    ($(#[$m:meta])* $name:ident: $setup:expr, $cwd:expr, $u:expr, $q:expr) => {
        $(#[$m])*
        #[test]
        fn $name() {
            same_as_plain($setup, $cwd, $u, $q);
        }
    };
}

const CREPO: &str = "git init -q -b main {C} && git -C {C} add . && git -C {C} commit -qm i && echo changed > {C}/settings.json && echo u > {C}/untracked.txt";
const BARE_IN_K: &str = "git init -q --bare {K}/r.git";
const XDG_HH: &str =
    "mkdir -p {H}/.config/git && git config --file {H}/.config/git/config alias.hh 'reset --hard'";
const INC_HH: &str = "git config --file {B}/inc.cfg alias.hh 'reset --hard' && git config --global include.path {B}/inc.cfg";
const INCIF_HH: &str = "git config --file {B}/inc.cfg alias.hh 'reset --hard' && git config --global includeIf.gitdir:{C}/.path {B}/inc.cfg";
const GLOBAL_HH: &str = "git config --global alias.hh 'reset --hard'";
const REPO_HH: &str = "git -C {C} config alias.hh 'reset --hard'";
const BANG_GLOBAL: &str =
    "git config --global alias.pw '!echo pwn > \"$HOME/.claude/settings.json\"'";
const USER_ALIASES: &str = "git config --global alias.st status && git config --global alias.co checkout && git config --global alias.br branch && git config --global alias.lg 'log --oneline --graph' && git config --global alias.ci commit";
const BARE_TMP: &str =
    "git init -q --bare {B}/tmp/bare.git && git -C {R} remote add origin {B}/tmp/bare.git && git -C {R} push -q -u origin main";

// ======================== (1) system directories (verdict only)
sys!(sys_cp_backslash_slash: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt \\/etc/bg5c84");
sys!(sys_cp_backslash_mid: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /e\\tc/bg5c84");
sys!(sys_cp_ansi_c: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt $'/etc'/bg5c84");
sys!(sys_cp_ansi_c_hex: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt $'\\x2fetc/bg5c84'");
sys!(sys_cp_ansi_c_oct: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt $'\\057etc'/bg5c84");
sys!(sys_cp_home_dotdot: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt \"$HOME\"/{UP}etc/bg5c84");
sys!(sys_cp_home_dotdot_tilde: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt ~/{UP}etc/bg5c84");
sys!(sys_cp_brace_list: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt {/etc,/etc}/bg5c84");
sys!(sys_cp_brace_inner: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /{etc,etc}/bg5c84");
sys!(#[ignore = "RED(5c848034-r3): `cp x /et?/f` is ask while `cp x /etc/f` is deny (glob over a system dir not resolved; base same)"] sys_cp_glob_dir_q: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /et?/bg5c84");
sys!(#[ignore = "RED(5c848034-r3): `cp x /[e]tc/f` is ask while `cp x /etc/f` is deny (base same)"] sys_cp_glob_dir_class: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /[e]tc/bg5c84");
sys!(sys_cp_private_etc: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /private/etc/bg5c84");
sys!(sys_cp_double_slash: "", "{R}", "cp {B}/mf/o.txt /usr/bin/bg5c84", "cp {B}/mf/o.txt //usr//bin/bg5c84");
sys!(sys_cp_dot_seg: "", "{R}", "cp {B}/mf/o.txt /usr/bin/bg5c84", "cp {B}/mf/o.txt /usr/./bin/bg5c84");
sys!(sys_cp_tmp_dotdot: "", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt /tmp/../etc/bg5c84");
sys!(sys_cp_symlink_to_etc: "ln -s /etc {B}/le", "{R}", "cp {B}/mf/o.txt /etc/bg5c84", "cp {B}/mf/o.txt {B}/le/bg5c84");
sys!(sys_tee_ansi_c: "", "{R}", "echo x | tee /etc/bg5c84", "echo x | tee $'/etc/bg5c84'");
sys!(sys_tee_backslash: "", "{R}", "echo x | tee /etc/bg5c84", "echo x | tee \\/etc/bg5c84");
sys!(sys_install_ansi_c: "", "{R}", "install {B}/mf/o.txt /usr/bin/bg5c84", "install {B}/mf/o.txt $'/usr/bin'/bg5c84");
sys!(sys_cp_system_lib: "", "{R}", "cp {B}/mf/o.txt /System/Library/bg5c84", "cp {B}/mf/o.txt \"/System\"/Library/bg5c84");
sys!(sys_cp_library_daemons: "", "{R}", "cp {B}/mf/o.txt /Library/LaunchDaemons/bg5c84.plist", "cp {B}/mf/o.txt $'/Library'/LaunchDaemons/bg5c84.plist");
sys!(sys_cp_bin_backslash: "", "{R}", "cp {B}/mf/o.txt /bin/bg5c84", "cp {B}/mf/o.txt \\/bin/bg5c84");
sys!(#[ignore = "RED(5c848034-r3): `git --work-tree=$'/etc' rm -f` is ask while unquoted is deny ($'…' not resolved statically; base allow)"] sys_git_wt_ansi_c: "", "{R}", "git --work-tree=/etc rm -q -f bg5c84", "git --work-tree=$'/etc' rm -q -f bg5c84");
sys!(sys_git_wt_backslash: "", "{R}", "git --work-tree=/etc rm -q -f bg5c84", "git --work-tree=\\/etc rm -q -f bg5c84");
sys!(sys_git_wt_spaced_quoted: "", "{R}", "git --work-tree=/etc rm -q -f bg5c84", "git --work-tree \"/etc\" rm -q -f bg5c84");
sys!(sys_git_wt_home_dotdot: "", "{R}", "git --work-tree=/etc rm -q -f bg5c84", "git --work-tree=\"$HOME\"/{UP}etc rm -q -f bg5c84");
sys!(sys_git_c_gitdir_checkout: "", "{R}", "git -C /etc --git-dir={R}/.git checkout -f HEAD -- settings.json", "git -C \"/etc\" --git-dir={R}/.git checkout -f HEAD -- settings.json");
sys!(sys_git_c_gitdir_ansi_c: "", "{R}", "git -C /etc --git-dir={R}/.git checkout -f HEAD -- settings.json", "git -C $'/etc' --git-dir={R}/.git checkout -f HEAD -- settings.json");
sys!(sys_git_cd_gitdir_checkout: "", "{R}", "git -C /etc --git-dir={R}/.git checkout -f HEAD -- settings.json", "cd /etc && git --git-dir={R}/.git checkout -f HEAD -- settings.json");
sys!(#[ignore = "RED(5c848034-r3): `GIT_WORK_TREE=$'/etc' git rm -f` is ask while unquoted is deny (base allow)"] sys_git_env_wt_ansi_c: "", "{R}", "GIT_WORK_TREE=/etc git rm -q -f bg5c84", "GIT_WORK_TREE=$'/etc' git rm -q -f bg5c84");
sys!(#[ignore = "RED(5c848034-r3): `export GIT_WORK_TREE=/etc; git rm -f x` is ALLOW while the prefix form is deny (exported git env not tracked; base same)"] sys_git_export_wt: "", "{R}", "GIT_WORK_TREE=/etc git rm -q -f bg5c84", "export GIT_WORK_TREE=/etc; git rm -q -f bg5c84");
sys!(sys_git_alias_init: "", "{R}", "git init -q /usr/bg5c84", "git -c alias.ii=init ii -q /usr/bg5c84");
sys!(#[ignore = "RED(5c848034-r3): `-c alias.wa='worktree add' wa -q /usr/x` is ask (\"has no value\") while the plain form is deny (base same)"] sys_git_alias_wt_add: "", "{R}", "git worktree add -q /usr/bg5c84", "git -c alias.wa='worktree add' wa -q /usr/bg5c84");
sys!(sys_git_global_alias_init: "git config --global alias.ii init", "{R}", "git init -q /usr/bg5c84", "git ii -q /usr/bg5c84");
sys!(#[ignore = "RED(5c848034-r3): alias.ii=init in ~/.config/git/config: `git ii -q /usr/x` is ask while `git init -q /usr/x` is deny (XDG config unread; base allow)"] sys_git_xdg_alias_init: "mkdir -p {H}/.config/git && git config --file {H}/.config/git/config alias.ii init", "{R}", "git init -q /usr/bg5c84", "git ii -q /usr/bg5c84");
sys!(sys_git_alias_rm_wt_etc: "", "{R}", "git --work-tree=/etc rm -q -f bg5c84", "git -c alias.r='rm -q -f' --work-tree=/etc r bg5c84");

// ======================== (2) alias hides a destructive git arm (verdict only, normal repo)
// The plain spelling's verdict comes from the git arms (detect/git.rs), which
// see the alias NAME, not the verb (implementer residual).
sys!(arm_alias_reset_hard: "", "{R}", "git reset -q --hard", "git -c alias.r='reset --hard' r -q");
sys!(#[ignore = "RED(5c848034-r3): alias.hh='reset --hard' in ~/.gitconfig: `git hh` is ALLOW while `git reset --hard` is deny (stored alias not fed to git arms; base same)"] arm_global_alias_reset_hard: GLOBAL_HH, "{R}", "git reset -q --hard", "git hh -q");
sys!(arm_alias_clean_fdx: "", "{R}", "git clean -fdx", "git -c alias.nuke='clean -fdx' nuke");
sys!(arm_alias_checkout_f: "", "{R}", "git checkout -f", "git -c alias.cf='checkout -f' cf");
sys!(arm_alias_stash_clear: "", "{R}", "git stash clear", "git -c alias.sc='stash clear' sc");
// Not kept: `git stash drop` / `git branch -D feat` — the plain spelling is
// itself `allow` (observed), so an alias cannot widen it.
sys!(#[ignore = "RED(5c848034-r3): `-c alias.pf='push --force' pf origin main` is ALLOW while `git push --force origin main` is ask (base same)"] arm_alias_push_force: "", "{R}", "git push --force origin main", "git -c alias.pf='push --force' pf origin main");
sys!(arm_alias_restore_dot: "", "{R}", "git restore .", "git -c alias.rs=restore rs .");

// ======================== (2) ~/.claude: alias to work-tree-writing verbs
hole!(h_alias_reset_hard_c: CREPO, "{R}", "git -C {C} -c alias.r='reset --hard' r -q");
hole!(h_alias_clean_cd: CREPO, "{R}", "cd {C} && git -c alias.nuke='clean -fdq' nuke");
hole!(h_alias_checkout_f: CREPO, "{R}", "git -C {C} -c alias.co=checkout co -- .");
hole!(h_alias_restore: CREPO, "{R}", "git -C {C} -c alias.rs=restore rs settings.json");
hole!(h_alias_stash: CREPO, "{R}", "git -C {C} -c alias.sd=stash sd -q");
hole!(h_alias_chain: CREPO, "{R}", "git -C {C} -c alias.a=b -c alias.b='reset --hard' a -q");
hole!(h_alias_upper: CREPO, "{R}", "git -C {C} -c alias.r='reset --hard' R -q");
hole!(h_alias_wt_gitdir: "", "{R}", "git -c alias.r='rm -q -f' --git-dir={R}/.git --work-tree={C} r settings.json");

// stored aliases (config files)
hole!(h_stored_repo_alias: &format!("{CREPO} && {REPO_HH}"), "{R}", "git -C {C} hh -q");
hole!(h_stored_global_alias: &format!("{CREPO} && {GLOBAL_HH}"), "{R}", "git -C {C} hh -q");
hole!(#[ignore = "RED(5c848034-r3): alias in ~/.config/git/config (unread) → `git -C ~/.claude hh` allow, resets ~/.claude (base same)"] h_stored_xdg_alias: &format!("{CREPO} && {XDG_HH}"), "{R}", "git -C {C} hh -q");
hole!(#[ignore = "RED(5c848034-r3): alias via ~/.gitconfig include.path (unread) → allow, resets ~/.claude (base same)"] h_stored_include_alias: &format!("{CREPO} && {INC_HH}"), "{R}", "git -C {C} hh -q");
hole!(#[ignore = "RED(5c848034-r3): alias via ~/.gitconfig includeIf.gitdir (unread) → allow, resets ~/.claude (base same)"] h_stored_includeif_alias: &format!("{CREPO} && {INCIF_HH}"), "{R}", "git -C {C} hh -q");
hole!(h_stored_config_global_env: &format!("{CREPO} && git config --file {{B}}/g.cfg alias.hh 'reset --hard'"), "{R}", "GIT_CONFIG_GLOBAL={B}/g.cfg git -C {C} hh -q");
hole!(#[ignore = "RED(5c848034-r3): `HOME=X git -C ~/.claude hh` reads X/.gitconfig alias (blastguard reads its own $HOME) → allow (base same)"] h_stored_home_prefix: &format!("{CREPO} && mkdir -p {{B}}/h2 && git config --file {{B}}/h2/.gitconfig alias.hh 'reset --hard'"), "{R}", "HOME={B}/h2 git -C {C} hh -q");
hole!(h_env_config_count_alias: CREPO, "{R}", "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.hh GIT_CONFIG_VALUE_0='reset --hard' git -C {C} hh -q");
hole!(h_config_env_alias: CREPO, "{R}", "V='reset --hard' git --config-env=alias.hh=V -C {C} hh -q");
hole!(#[ignore = "RED(5c848034-r3): stored `!` shell alias in ~/.gitconfig → `git pw` allow, writes ~/.claude/settings.json (base same)"] h_stored_bang_alias: BANG_GLOBAL, "{R}", "git pw");
hole!(h_stored_alias_wt_rm: "git config --global alias.r 'rm -q -f'", "{R}", "git --work-tree={C} r settings.json");

// ======================== (2) --git-dir / GIT_WORK_TREE / core.worktree combos
hole!(#[ignore = "RED(5c848034-r3): `export GIT_WORK_TREE=~/.claude; git --git-dir=R rm -f` allow, removes ~/.claude file (base same)"] h_export_wt_gitdir: "", "{R}", "export GIT_WORK_TREE={C}; git --git-dir={R}/.git rm -q -f settings.json");
hole!(#[ignore = "RED(5c848034-r3): `export GIT_WORK_TREE=~/.claude && git rm -f` allow, removes ~/.claude file (base same)"] h_export_wt_plain: "", "{R}", "export GIT_WORK_TREE={C} && git rm -q -f settings.json");
hole!(h_env_cmd_wt: "", "{R}", "env GIT_WORK_TREE={C} git rm -q -f settings.json");
hole!(h_env_both: "", "{B}", "GIT_DIR={R}/.git GIT_WORK_TREE={C} git rm -q -f settings.json");
hole!(h_export_gitdir_cd: "", "{R}", "export GIT_DIR={R}/.git; cd {C} && git checkout -f HEAD -- settings.json");
hole!(h_gitdir_c_home_ancestor: "", "{R}", "git -C {H} --git-dir={R}/.git checkout -f HEAD -- .claude/settings.json");
hole!(h_gitdir_env_cd_home: "", "{R}", "cd {H} && GIT_DIR={R}/.git git checkout -f HEAD -- .claude/settings.json");
hole!(h_gitdir_c_tilde_home: "", "{R}", "git -C ~ --git-dir={R}/.git checkout -f HEAD -- .claude/settings.json");
hole!(h_gitdir_c_ansi_c: "", "{R}", "git -C $'{C}' --git-dir={R}/.git checkout -f HEAD -- settings.json");
hole!(h_gitdir_c_hooks_checkout_all: "", "{R}", "git -C {K} --git-dir={R}/.git checkout -f HEAD -- .");
hole!(#[ignore = "RED(5c848034-r3): core.worktree=~/.claude stored in repo config → plain `git rm -f settings.json` allow, removes ~/.claude file (base same)"] h_core_worktree_stored: "git -C {R} config core.worktree {C}", "{R}", "git rm -q -f settings.json");
hole!(h_core_worktree_stored_gitdir: "git -C {R} config core.worktree {C}", "{B}", "git --git-dir={R}/.git rm -q -f settings.json");
hole!(h_core_worktree_bare_dot: "mkdir -p {B}/dw/.claude && echo pwn > {B}/dw/.claude/settings.json && git init -q --bare {B}/dot.git && git --git-dir={B}/dot.git --work-tree={B}/dw add . && git --git-dir={B}/dot.git --work-tree={B}/dw commit -qm i && git --git-dir={B}/dot.git config core.bare false && git --git-dir={B}/dot.git config core.worktree {H}", "{B}", "git --git-dir={B}/dot.git checkout -f");

// ======================== (2) ANSI-C / escape spellings of protected operands
hole!(h_cp_ansi_c_dest: "", "{R}", "cp {B}/mf/o.txt $'{C}/settings.json'");
hole!(h_cp_ansi_c_name: "", "{R}", "cp {B}/mf/o.txt {C}/$'settings.json'");
hole!(h_cp_ansi_c_hex_name: "", "{R}", "cp {B}/mf/o.txt {C}/$'\\x73ettings.json'");
hole!(h_tee_ansi_c: "", "{R}", "echo x | tee $'{K}/pre.sh'");
hole!(h_cp_backslash_name: "", "{R}", "cp {B}/mf/o.txt {C}/sett\\ings.json");
hole!(h_cp_brace_name: "", "{R}", "echo x | tee {C}/{settings,zz}.json");
hole!(h_mv_ansi_c: "", "{R}", "mv {B}/mf/o.txt $'{K}'/pre.sh");
hole!(h_git_wt_ansi_c: "", "{R}", "git --work-tree=$'{C}' rm -q -f settings.json");
hole!(h_git_env_wt_ansi_c: "", "{R}", "GIT_WORK_TREE=$'{C}' git rm -q -f settings.json");
hole!(h_git_c_ansi_c_crepo: CREPO, "{R}", "git -C $'{C}' reset -q --hard");

// ======================== (2) relative paths git resolves from the TOP, not the cwd
hole!(h_coi_prefix_subdir: "", "{R}/sub", "git checkout-index -a -f --prefix=../../.claude/");
hole!(h_coi_prefix_c_subdir: "", "{H}", "git -C {R}/sub checkout-index -a -f --prefix=../../.claude/");
hole!(h_push_relative_subdir: BARE_IN_K, "{R}/sub", "git push -q ../../.claude/hooks/r.git main");
hole!(h_push_stored_relative_subdir: &format!("{BARE_IN_K} && git -C {{R}} remote add bk ../../.claude/hooks/r.git"), "{R}/sub", "git push -q bk main");

// ======================== (3) FP: aliases in ordinary repos
fp!(fp_global_alias_st_wt: USER_ALIASES, "{W}", "git st");
fp!(fp_global_alias_lg_wt: USER_ALIASES, "{W}", "git lg -5");
fp!(fp_global_alias_br_wt: USER_ALIASES, "{W}", "git br -a");
fp!(fp_global_alias_co_b_wt: USER_ALIASES, "{W}", "git co -q -b feat");
fp!(fp_global_alias_co_file_wt: USER_ALIASES, "{W}", "git co -- src/lib.rs");
fp!(fp_global_alias_ci_wt: USER_ALIASES, "{W}", "git ci -q --allow-empty -m x");
fp!(fp_global_alias_st_repo: USER_ALIASES, "{R}", "git st");
fp!(fp_global_alias_co_b_repo: USER_ALIASES, "{R}", "git co -q -b feat");
fp!(fp_global_alias_c_repo: USER_ALIASES, "{H}", "git -C {R} lg -3");
fp!(fp_xdg_alias_st_wt: "mkdir -p {H}/.config/git && git config --file {H}/.config/git/config alias.st status", "{W}", "git st");
fp!(fp_cmdline_alias_st_wt: "", "{W}", "git -c alias.st=status st");
fp!(fp_cmdline_alias_co_wt: "", "{W}", "git -c alias.co=checkout co -q -b feat2");

// FP: work-tree-writing verbs in a normal repo main tree (has .claude/)
fp!(fp_repo_checkout_b: "", "{R}", "git checkout -q -b feat");
fp!(fp_repo_switch_c: "", "{R}", "git switch -q -c feat");
// (`git reset --hard` in a main tree is a deliberate pre-existing Deny —
// "git reset --hard discards working-tree changes" — on base too; not an FP.)
fp!(fp_repo_reset_mixed: "", "{R}", "git reset -q");
fp!(fp_repo_restore: "", "{R}", "git restore src/lib.rs");
fp!(fp_repo_stash: "", "{R}", "git stash -q");
fp!(fp_repo_clean_n: "", "{R}", "git clean -n");
fp!(fp_repo_checkout_subdir: "", "{R}/sub", "git checkout -- f");
fp!(fp_repo_c_checkout: "", "{H}", "git -C {R} checkout -q -b feat");
fp!(fp_plainrepo_checkout_b: "git init -q -b main {S}/p && git -C {S}/p commit -q --allow-empty -m i", "{S}/p", "git checkout -q -b feat");
fp!(fp_wt_reset_hard: "", "{W}", "git reset -q --hard");
fp!(fp_wt_merge: "", "{W}", "git merge -q --ff-only main");

// FP: --git-dir in a normal repo
fp!(fp_gitdir_log_repo: "", "{R}", "git --git-dir={R}/.git log --oneline -1");
fp!(fp_gitdir_status_rel: "", "{R}", "git --git-dir=.git status");
fp!(fp_gitdir_wt_status: "", "{H}", "git --git-dir={R}/.git --work-tree={R} status");
fp!(fp_gitdir_checkout_b_repo: "", "{R}", "git --git-dir=.git checkout -q -b feat");
fp!(fp_gitdir_wt_checkout_repo: "", "{H}", "git --git-dir={R}/.git --work-tree={R} checkout -q -b feat");

// FP: -C status/log/diff
fp!(fp_c_status: "", "{H}", "git -C {R} status");
fp!(fp_c_log: "", "{S}", "git -C repo log --oneline -3");
fp!(fp_c_diff_wt: "", "{R}", "git -C {W} diff");

// FP: push
fp!(fp_push_no_args: BARE_TMP, "{R}", "git push -q");
fp!(fp_push_origin_wt: BARE_TMP, "{W}", "git push -q origin HEAD:refs/heads/w");
fp!(fp_push_ssh_remote: "git -C {R} remote add gh git@github.com:x/y.git", "{R}", "git push -q gh main");
fp!(fp_push_https_remote: "git -C {R} remote add gh https://example.invalid/x/y.git", "{R}", "git push -q gh main");
fp!(fp_push_subdir_origin: BARE_TMP, "{R}/sub", "git push -q origin main");

// FP: ANSI-C / quoted words in own worktree
fp!(fp_echo_ansi_c: "", "{W}", "echo $'a\\tb'");
fp!(fp_printf_ansi_c_redirect: "", "{W}", "printf $'x\\n' > out.txt");
fp!(fp_cp_ansi_c_wt: "", "{W}", "cp src/lib.rs $'sub/new.rs'");
fp!(fp_commit_ansi_c_msg: "", "{W}", "git commit -q --allow-empty -m $'subj\\n\\nbody'");
fp!(fp_tee_quoted_wt: "", "{W}", "echo x | tee 'sub/t.log'");
fp!(fp_mv_quoted_wt: "", "{W}", "mv \"sub/f\" 'sub/g'");
fp!(#[ignore = "RED(5c848034-r3): FP `cp src/lib.rs sub/{a,b}.rs` in own worktree is ask (brace expansion not resolved; base same, pre-existing)"] fp_cp_brace_wt: "", "{W}", "cp src/lib.rs sub/{a,b}.rs");
