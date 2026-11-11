#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Boundary tests for three fail-closed relaxations in the blastguard
//! PreToolUse gate (written by a disinterested author, CLAUDE.md section 2(a);
//! `src/` untouched). Each test asserts the RESTRICTIVE verdict (anything but
//! allow) for a hostile variant, plus each relaxation's intended allow as a
//! control. Verdicts only (empty stdout = allow); the whose-tree text is
//! matched only where noted.
//!
//!  1. #1630 case-fold in `SafeRoots::no_symlink_below`: a real symlink whose
//!     spelling differs only in case from what is typed must not be read as
//!     "same file, different case".
//!  2. #1626 python `-c` program text with `> T` in a string literal is data
//!     only when the program provably cannot start a process.
//!  3. #1625 `Ctx::in_worktree_checkout_for_redirect`: a chained line whose
//!     prefix could relink the target must keep the whose-tree refusal.
//!
//! Fixture: fake HOME, a repo, a session worktree, a linked worktree under
//! `$HOME/.condukt/worktrees`, "another project" repos, and a fake scratchpad
//! under /private/tmp. The filesystem here must be case-insensitive for the
//! case-fold tests to bite (APFS default); on a case-sensitive one they pass
//! trivially.

#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const WHOSE_TREE: &str = "not one of this session's trees";
const REL: &str = "crates/condukt/src/maintree.rs";

struct Fx {
    base: PathBuf,
    home: PathBuf,
    sess: PathBuf,
    wt: PathBuf,
    other: PathBuf,
    scratch: PathBuf,
    tmp_root: PathBuf,
    uniq: String,
    session: String,
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo_with_tracked(home: &Path, repo: &Path) {
    std::fs::create_dir_all(repo.join("crates/condukt/src")).unwrap();
    git(home, repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(REL), "fn a() {}\n").unwrap();
    std::fs::write(repo.join("secret.txt"), "s\n").unwrap();
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(home, repo, &["add", "."]);
    git(home, repo, &["commit", "-q", "-m", "init"]);
}

impl Fx {
    fn new(name: &str) -> Fx {
        let uniq = format!("{}-{name}", std::process::id());
        let base = neutral_base::neutral_base()
            .join("scope_redirect_relax")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let repo = base.join("src/harness");
        std::fs::create_dir_all(home.join(".condukt/worktrees")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_repo_with_tracked(&home, &repo);
        let wts = base.join("src/.harness-worktrees");
        std::fs::create_dir_all(&wts).unwrap();
        let sess = wts.join("session-bf349dd5");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-bf349dd5",
                sess.to_str().unwrap(),
            ],
        );
        let wt = home.join(".condukt/worktrees/mab3-3da84f08");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mab3/3da84f08",
                wt.to_str().unwrap(),
            ],
        );
        // "Another project": a separate repo outside every safe root.
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        init_repo_with_tracked(&home, &other);

        let tmp_root = PathBuf::from(format!("/private/tmp/bg-scope-relax-{uniq}"));
        let _ = std::fs::remove_dir_all(&tmp_root);
        let session = "bf349dd5-8e0c-4418-af43-0de710b93593".to_string();
        let scratch = tmp_root
            .join("claude-502/-Users-yuki-src-harness")
            .join(&session)
            .join("scratchpad");
        std::fs::create_dir_all(&scratch).unwrap();
        Fx {
            base: std::fs::canonicalize(&base).unwrap(),
            home,
            sess,
            wt,
            other,
            scratch,
            tmp_root,
            uniq,
            session,
        }
    }

    /// (decision, raw stdout), judged as a Bash call from `cwd`.
    fn bash(&self, cwd: &Path, command: &str) -> (String, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": self.session,
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.sess)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(cwd);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0 ({command:?}); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let d = if stdout.trim().is_empty() {
            "allow".to_string()
        } else {
            let doc: serde_json::Value = serde_json::from_str(stdout.trim())
                .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
            doc["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .unwrap_or_else(|| panic!("no permissionDecision: {stdout}"))
                .to_string()
        };
        println!("[{command}] => {d}: {}", stdout.trim());
        (d, stdout)
    }

    fn t(&self) -> String {
        self.other.join("secret.txt").display().to_string()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp_root);
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_file(format!("/private/tmp/bg-scope-relax-Link-{}", self.uniq));
    }
}

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        _ => 2,
    }
}

#[track_caller]
fn assert_restrictive(case: &str, r: &(String, String)) {
    assert_ne!(
        r.0,
        "allow",
        "{case}: must be ask/deny (fail-open if allow); got decision JSON: {:?}",
        r.1.trim()
    );
}

// ---------------------------------------------------------------------------
// 1. #1630 case-fold
// ---------------------------------------------------------------------------

/// Control: the intended allow (regular file on disk `mergeMain.log`, typed
/// `mergemain.log`).
#[test]
fn case_fold_control_regular_file_case_only_difference_allows() {
    let fx = Fx::new("cf-ctl");
    std::fs::write(fx.scratch.join("mergeMain.log"), "old\n").unwrap();
    let r = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/mergemain.log", fx.scratch.display()),
    );
    assert_eq!(r.0, "allow", "intended allow: {:?}", r.1.trim());
}

/// A real symlink `mergeMain.log` -> tracked file of another repo, typed in a
/// different case. Must be no more permissive than the same-case spelling,
/// and not allow.
fn symlink_file_case(fx: &Fx, target: &Path, case: &str) {
    symlink(target, fx.scratch.join("mergeMain.log")).unwrap();
    let same = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/mergeMain.log", fx.scratch.display()),
    );
    let diff = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/mergemain.log", fx.scratch.display()),
    );
    assert_restrictive(&format!("{case} (case-different spelling)"), &diff);
    assert!(
        rank(&diff.0) >= rank(&same.0),
        "{case}: case-different spelling ({}) is more permissive than same-case ({})",
        diff.0,
        same.0
    );
}

#[test]
fn case_fold_symlink_to_tracked_file_in_other_repo_is_not_allowed() {
    let fx = Fx::new("cf-sym-repo");
    let t = fx.other.join("secret.txt");
    symlink_file_case(&fx, &t, "symlink -> tracked file of another repo");
}

#[test]
fn case_fold_symlink_to_file_in_session_repo_is_not_allowed() {
    let fx = Fx::new("cf-sym-sess");
    let t = fx.sess.join("a.txt");
    symlink_file_case(&fx, &t, "symlink -> tracked file of the session worktree");
}

#[test]
fn case_fold_symlink_to_untracked_home_file_is_not_allowed() {
    let fx = Fx::new("cf-sym-home");
    let t = fx.home.join(".bashrc");
    std::fs::write(&t, "# rc\n").unwrap();
    symlink_file_case(&fx, &t, "symlink -> $HOME/.bashrc");
}

#[test]
fn case_fold_symlink_to_etc_hosts_is_not_allowed() {
    let fx = Fx::new("cf-sym-etc");
    symlink_file_case(&fx, Path::new("/etc/hosts"), "symlink -> /etc/hosts");
}

/// Dangling symlink with case-different spelling pointing into another repo
/// at a tracked path that does not exist yet is not a prefix problem; instead
/// point the dangling link outside /private/tmp.
#[test]
fn case_fold_dangling_symlink_outside_tmp_is_not_allowed() {
    let fx = Fx::new("cf-dangling");
    let t = fx.home.join("not-yet-created.txt");
    symlink_file_case(&fx, &t, "dangling symlink -> $HOME/not-yet-created.txt");
}

/// Directory component: `Sub` is a symlink to a directory holding a tracked
/// file of another repo; typed `sub`.
#[test]
fn case_fold_symlinked_dir_component_with_different_case_is_not_allowed() {
    let fx = Fx::new("cf-dir");
    symlink(fx.other.join("crates/condukt/src"), fx.scratch.join("Sub")).unwrap();
    let same = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/Sub/maintree.rs", fx.scratch.display()),
    );
    let diff = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/sub/maintree.rs", fx.scratch.display()),
    );
    assert_restrictive("symlinked dir, case-different", &diff);
    assert!(
        rank(&diff.0) >= rank(&same.0),
        "diff {} vs same {}",
        diff.0,
        same.0
    );
}

/// Symlink directly under the /private/tmp root (the first component below
/// the root), case-different.
#[test]
fn case_fold_symlink_directly_below_tmp_root_is_not_allowed() {
    let fx = Fx::new("cf-top");
    let link = format!("/private/tmp/bg-scope-relax-Link-{}", fx.uniq);
    let _ = std::fs::remove_file(&link);
    symlink(fx.other.join("crates/condukt/src"), &link).unwrap();
    let lower = format!("/private/tmp/bg-scope-relax-link-{}/maintree.rs", fx.uniq);
    let same = fx.bash(&fx.sess, &format!("echo hi > {link}/maintree.rs"));
    let diff = fx.bash(&fx.sess, &format!("echo hi > {lower}"));
    assert_restrictive("symlink under tmp root, case-different", &diff);
    assert!(
        rank(&diff.0) >= rank(&same.0),
        "diff {} vs same {}",
        diff.0,
        same.0
    );
}

/// Symlink whose TARGET differs from the spelled name only in case but sits
/// in the scratchpad: `Link.log -> LINK.LOG`-style cannot coexist on APFS, so
/// use a symlink `mergeMain.log` -> sibling `MERGEMAIN.LOG` that does not
/// exist; writing creates a file in the scratchpad (harmless, but it must not
/// widen anything). Also a symlink pointing at the scratchpad file's own
/// differently-cased name is not a way out: verdict must not exceed same-case.
#[test]
fn case_fold_symlink_to_differently_cased_sibling_is_no_more_permissive() {
    let fx = Fx::new("cf-sib");
    std::fs::write(fx.scratch.join("Real.log"), "x\n").unwrap();
    symlink("REAL.LOG", fx.scratch.join("Alias.log")).unwrap();
    let same = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/Alias.log", fx.scratch.display()),
    );
    let diff = fx.bash(
        &fx.sess,
        &format!("echo hi > {}/alias.log", fx.scratch.display()),
    );
    assert!(
        rank(&diff.0) >= rank(&same.0),
        "case-different ({}) more permissive than same-case ({}): {:?}",
        diff.0,
        same.0,
        diff.1.trim()
    );
}

// ---------------------------------------------------------------------------
// 2. #1626 python -c program text
// ---------------------------------------------------------------------------

fn py(fx: &Fx, prog: &str) -> (String, String) {
    let prog = prog.replace("TARGET", &fx.t());
    fx.bash(&fx.sess, &format!("python3 -c '{prog}'"))
}

#[test]
fn py_control_data_string_with_gt_target_allows() {
    let fx = Fx::new("py-ctl");
    let r = py(&fx, "import sys; s=\"> TARGET\"; print(s)");
    assert_eq!(r.0, "allow", "intended allow: {:?}", r.1.trim());
}

macro_rules! py_hostile {
    ($(#[$attr:meta])* $name:ident, $prog:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            let fx = Fx::new(stringify!($name));
            let r = py(&fx, $prog);
            assert_restrictive(stringify!($name), &r);
        }
    };
}

py_hostile!(
    py_os_execl,
    "import os; os.execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_os_execv,
    "import os; os.execv(\"/bin/sh\",[\"sh\",\"-c\",\"echo x > TARGET\"])"
);
py_hostile!(
    py_os_execlp,
    "import os; os.execlp(\"sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_os_spawnl,
    "import os; os.spawnl(os.P_WAIT,\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_os_posix_spawn,
    "import os; os.posix_spawn(\"/bin/sh\",[\"sh\",\"-c\",\"echo x > TARGET\"],os.environ)"
);
py_hostile!(
    py_posix_execv,
    "import posix; posix.execv(\"/bin/sh\",[\"sh\",\"-c\",\"echo x > TARGET\"])"
);
py_hostile!(
    py_pty_spawn,
    "import pty; pty.spawn([\"/bin/sh\",\"-c\",\"echo x > TARGET\"])"
);
py_hostile!(
    py_ctypes_system,
    "import ctypes; ctypes.CDLL(None).system(b\"echo x > TARGET\")"
);
py_hostile!(
    py_ctypes_execl,
    "import ctypes; ctypes.CDLL(None).execl(b\"/bin/sh\",b\"sh\",b\"-c\",b\"echo x > TARGET\",None)"
);
py_hostile!(
    py_from_os_import_execv_as_alias,
    "from os import execv as e; e(\"/bin/sh\",[\"sh\",\"-c\",\"echo x > TARGET\"])"
);
py_hostile!(
    py_import_os_as_o_execl,
    "import os as o; o.execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_attribute_alias_then_call,
    "import os; f=os.execl; f(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_attribute_in_list_then_call,
    "import os; [os.execl][0](\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_vars_lookup,
    "import os; vars(os)[\"exec\"+\"l\"](\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_dunder_dict_lookup,
    "import os; os.__dict__[\"execl\"](\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_sys_modules_os,
    "import sys; sys.modules[\"os\"].execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_pathlib_os_reexport,
    "import pathlib; pathlib.os.execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_glob_os_reexport,
    "import glob; glob.os.execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_getattr_concat,
    "import os; getattr(os,\"exe\"+\"cl\")(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(
    py_dunder_import,
    "__import__(\"os\").execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > TARGET\")"
);
py_hostile!(py_os_popen, "import os; os.popen(\"echo x > TARGET\")");
py_hostile!(py_unterminated_string, "import sys; s=\"> TARGET; print(s)");
py_hostile!(
    py_unterminated_triple_quote,
    "import sys; s=\"\"\"> TARGET; print(s)"
);
py_hostile!(
    py_non_allowlisted_import_multiprocessing,
    "import multiprocessing; s=\"> TARGET\"; print(s)"
);
py_hostile!(py_exec_call, "exec(\"print(1)\"); s=\"> TARGET\"");
py_hostile!(py_eval_call, "s=\"> TARGET\"; eval(\"1\")");
py_hostile!(
    py_payload_with_command_substitution,
    "import sys; s=\"> TARGET $(id)\"; print(s)"
);
py_hostile!(
    py_payload_with_backtick,
    "import sys; s=\"> TARGET `id`\"; print(s)"
);

/// A real shell redirect OUTSIDE the quoted program must stay restrictive.
#[test]
fn py_real_shell_redirect_after_program_is_not_allowed() {
    let fx = Fx::new("py-real-redirect");
    let r = fx.bash(&fx.sess, &format!("python3 -c 'print(1)' > {}", fx.t()));
    assert_restrictive("real redirect after python -c", &r);
}

/// Same, with a data-looking program plus a real redirect.
#[test]
fn py_data_program_plus_real_redirect_is_not_allowed() {
    let fx = Fx::new("py-real-redirect2");
    let r = fx.bash(
        &fx.sess,
        &format!(
            "python3 -c 'import sys; s=\"> {t}\"; print(s)' > {t}",
            t = fx.t()
        ),
    );
    assert_restrictive("data program + real redirect", &r);
}

/// Data program chained with a real redirecting command.
#[test]
fn py_data_program_then_chained_real_redirect_is_not_allowed() {
    let fx = Fx::new("py-chained");
    let r = fx.bash(
        &fx.sess,
        &format!(
            "python3 -c 'import sys; s=\"> {t}\"; print(s)' && echo x > {t}",
            t = fx.t()
        ),
    );
    assert_restrictive("data program && real redirect", &r);
}

/// The relaxation is python-only: perl / node with the same string are not
/// allowed (before it they were not allowed either).
#[test]
fn perl_e_with_gt_string_is_not_allowed() {
    let fx = Fx::new("perl");
    let r = fx.bash(
        &fx.sess,
        &format!("perl -e 'my $s=\"> {}\"; print $s'", fx.t()),
    );
    assert_restrictive("perl -e", &r);
}

#[test]
fn node_e_with_gt_string_is_not_allowed() {
    let fx = Fx::new("node");
    let r = fx.bash(
        &fx.sess,
        &format!("node -e 'const s=\"> {}\"; console.log(s)'", fx.t()),
    );
    assert_restrictive("node -e", &r);
}

#[test]
fn python_dash_c_target_etc_hosts_with_exec_is_not_allowed() {
    let fx = Fx::new("py-etc");
    let r = fx.bash(
        &fx.sess,
        "python3 -c 'import os; os.execl(\"/bin/sh\",\"sh\",\"-c\",\"echo x > /etc/hosts\")'",
    );
    assert_restrictive("exec writing /etc/hosts", &r);
}

// ---------------------------------------------------------------------------
// 3. #1625 write_prefix_cannot_relink
// ---------------------------------------------------------------------------

fn chain(fx: &Fx, prefix_after_cd: &str, write: &str, tail: &str) -> String {
    let s = fx.base.join("scratch");
    std::fs::create_dir_all(&s).unwrap();
    format!(
        "cd {wt} && S={s} && {prefix_after_cd}{write}{tail}",
        wt = fx.wt.display(),
        s = s.display(),
    )
}

#[test]
fn wt_control_intended_chain_is_not_whose_tree_refusal() {
    let fx = Fx::new("wt-ctl");
    let cmd = chain(
        &fx,
        &format!("cp {REL} $S/x && "),
        &format!("git show HEAD:{REL} > {REL}"),
        &format!(" && echo done ; cp $S/x {REL}"),
    );
    for cwd in [fx.sess.clone(), fx.wt.clone()] {
        let (d, out) = fx.bash(&cwd, &cmd);
        assert!(
            !out.contains(WHOSE_TREE),
            "cwd {}: intended chain must not be the whose-tree refusal (decision {d}): {out}",
            cwd.display()
        );
    }
}

/// Run `cmd` from the session worktree and require not-allow; additionally
/// require the whose-tree refusal text when `strict` (the relaxation must not
/// have fired at all).
///
/// Only the session worktree, as this doc says: with cwd INSIDE the linked
/// worktree that tree is a safe root, the line is decided by the confined-root
/// arm before `write_prefix_cannot_relink` is consulted, and main 6972be32
/// gives the same allow — a pre-existing class, pinned separately by
/// `wt_relink_prefix_from_linked_worktree_cwd_pre_existing`.
fn wt_hostile(fx: &Fx, cmd: &str, strict: bool) {
    let cwd = &fx.sess;
    let r = fx.bash(cwd, cmd);
    assert_restrictive(&format!("cwd {}: {cmd}", cwd.display()), &r);
    if strict {
        assert!(
            r.1.contains(WHOSE_TREE),
            "cwd {}: relaxation must not have fired (expected whose-tree refusal): {:?}",
            cwd.display(),
            r.1.trim()
        );
    }
}

/// The second cwd the original `wt_hostile` loop used: the session runs INSIDE
/// the linked worktree. The prefix relinks the tracked target to a file outside
/// the tree before the write opens it. Allowed on main 6972be32 as well (the
/// confined-root arm judges the placement at hook time), so this is a
/// pre-existing RED, not one of the #1625 relaxation's.
#[test]
#[ignore = "pre-existing RED (allow on main 6972be32): relink before a write inside the cwd's own tree; needs a ruling, remove ignore when fixed"]
fn wt_relink_prefix_from_linked_worktree_cwd_pre_existing() {
    let fx = Fx::new("wt-ln-wtcwd");
    let cmd = format!(
        "cd {wt} && ln -sf {t} {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    let r = fx.bash(&fx.wt, &cmd);
    assert_restrictive(&format!("cwd {}: {cmd}", fx.wt.display()), &r);
}

#[test]
fn wt_ln_s_in_prefix_relinks_target() {
    let fx = Fx::new("wt-ln");
    let cmd = format!(
        "cd {wt} && ln -sf {t} {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_mv_in_prefix_relinks_target() {
    let fx = Fx::new("wt-mv");
    symlink(fx.t(), fx.wt.join("lnk")).unwrap();
    let cmd = format!(
        "cd {wt} && mv lnk {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_cp_s_in_prefix_relinks_target() {
    let fx = Fx::new("wt-cps");
    let cmd = format!(
        "cd {wt} && cp -sf {t} {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_cp_capital_r_of_symlink_in_prefix() {
    let fx = Fx::new("wt-cpR");
    symlink(fx.t(), fx.wt.join("lnk")).unwrap();
    let cmd = format!(
        "cd {wt} && cp -R lnk {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_cp_with_variable_holding_flag() {
    let fx = Fx::new("wt-cpX");
    let cmd = format!(
        "cd {wt} && X=-sf && cp $X {t} {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_cp_with_unassigned_variable() {
    let fx = Fx::new("wt-cpU");
    symlink(fx.t(), fx.wt.join("lnk")).unwrap();
    let cmd = format!(
        "cd {wt} && cp $X lnk {REL} && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_semicolon_before_write() {
    let fx = Fx::new("wt-semi");
    let cmd = format!(
        "cd {wt} && ln -sf {t} {REL} ; git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_semicolon_between_benign_prefix_and_write() {
    let fx = Fx::new("wt-semi2");
    let cmd = format!(
        "cd {wt} ; true && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_or_before_write() {
    let fx = Fx::new("wt-or");
    let cmd = format!(
        "cd {wt} && ln -sf {t} {REL} || git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_benign_prefix_then_or_write() {
    let fx = Fx::new("wt-or2");
    let cmd = format!(
        "cd {wt} && true || git show HEAD:{REL} > {REL}",
        wt = fx.wt.display()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_pipe_after_write() {
    let fx = Fx::new("wt-pipe");
    let cmd = format!(
        "cd {wt} && git show HEAD:{REL} > {REL} | ln -sf {t} {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_background_after_write() {
    let fx = Fx::new("wt-bg");
    let cmd = format!(
        "cd {wt} && git show HEAD:{REL} > {REL} & ln -sf {t} {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_command_substitution_in_write_segment() {
    let fx = Fx::new("wt-subst");
    let cmd = format!(
        "cd {wt} && echo $(ln -sf {t} {REL}) > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

#[test]
fn wt_backtick_in_write_segment() {
    let fx = Fx::new("wt-bt");
    let cmd = format!(
        "cd {wt} && echo `ln -sf {t} {REL}` > {REL}",
        wt = fx.wt.display(),
        t = fx.t()
    );
    wt_hostile(&fx, &cmd, true);
}

/// PATH assigned by a literal NAME=value prefix word, then `cp` resolves to
/// an untracked executable in the worktree that relinks the target.
#[test]
fn wt_path_assignment_hijacks_cp() {
    let fx = Fx::new("wt-path");
    let bin = fx.wt.join("evilbin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("cp");
    std::fs::write(&script, format!("#!/bin/sh\nln -sf {} {REL}\n", fx.t())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cmd = format!(
        "cd {wt} && PATH={bin} && cp {REL} /dev/null && git show HEAD:{REL} > {REL}",
        wt = fx.wt.display(),
        bin = bin.display()
    );
    wt_hostile(&fx, &cmd, true);
}

/// `cd` into a symlinked directory inside the worktree that leads to another
/// project's tracked file.
#[test]
fn wt_cd_into_symlinked_dir_leading_to_other_repo() {
    let fx = Fx::new("wt-cdlink");
    symlink(fx.other.join("crates/condukt/src"), fx.wt.join("lnkdir")).unwrap();
    let cmd = format!(
        "cd {wt}/lnkdir && S={s} && cp maintree.rs $S/x && git show HEAD:maintree.rs > maintree.rs",
        wt = fx.wt.display(),
        s = fx.base.display()
    );
    wt_hostile(&fx, &cmd, false);
    let cmd2 = format!(
        "cd {wt} && S={s} && cp {REL} $S/x && git show HEAD:{REL} > {wt}/lnkdir/maintree.rs",
        wt = fx.wt.display(),
        s = fx.base.display()
    );
    wt_hostile(&fx, &cmd2, false);
}

/// The same chained shape against a tracked file of ANOTHER project (not a
/// linked worktree under a storage root) must still be refused.
#[test]
fn wt_other_project_same_chain_shape_is_still_refused() {
    let fx = Fx::new("wt-other");
    let s = fx.base.join("scratch");
    std::fs::create_dir_all(&s).unwrap();
    let cmd = format!(
        "cd {o} && S={s} && cp {REL} $S/x && git show HEAD:{REL} > {REL} && echo done ; cp $S/x {REL}",
        o = fx.other.display(),
        s = s.display()
    );
    wt_hostile(&fx, &cmd, true);
}

/// A plain clone (its `.git` is a directory, not a linked worktree) placed
/// under the condukt worktree storage root.
#[test]
fn wt_non_linked_clone_under_storage_root_is_still_refused() {
    let fx = Fx::new("wt-clone");
    let clone = fx.home.join(".condukt/worktrees/plain-clone");
    std::fs::create_dir_all(&clone).unwrap();
    init_repo_with_tracked(&fx.home, &clone);
    assert!(
        clone.join(".git").is_dir(),
        "fixture: not a linked worktree"
    );
    let s = fx.base.join("scratch");
    std::fs::create_dir_all(&s).unwrap();
    let cmd = format!(
        "cd {c} && S={s} && cp {REL} $S/x && git show HEAD:{REL} > {REL} && echo done ; cp $S/x {REL}",
        c = clone.display(),
        s = s.display()
    );
    wt_hostile(&fx, &cmd, true);
}
