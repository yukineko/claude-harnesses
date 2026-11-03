//! `sitecraft init <dir>` — create a new site repository (rulings D6).
//!
//! Contract (exit codes per the crate convention):
//!
//! * `<dir>` absent, or an empty directory → create it (with any missing
//!   parents), `git init` it, create `<dir>/sitecraft/`; exit 0.
//! * `<dir>` a non-empty directory, a non-directory (regular file, …), or a
//!   path through a non-directory → exit 1, nothing changed.
//! * `<dir>` (its nearest existing ancestor, for an absent `<dir>`) inside
//!   another git repository (`git rev-parse --is-inside-work-tree` succeeds)
//!   → exit 1, nothing created.
//! * `git` missing from `PATH`, `git` exiting non-zero for any other reason
//!   than "not a git repository", `git init` exiting non-zero, all `git`
//!   calls together exceeding [`GIT_TIME_LIMIT`] (the group is killed;
//!   reason `timeout`), `<dir>/.git` absent after `git init`, or any IO
//!   failure → exit 2. Whatever init itself created before such a failure is
//!   removed again (best effort; a removal failure is named in the reason).
//!
//! `git` runs with `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and
//! `GIT_CEILING_DIRECTORIES` removed, so neither the target nor the
//! outer-repository check can be redirected by the caller's environment.
//!
//! The scaffold itself (`<dir>/app/`, Phase 6 of the skill) is not done here.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::{Verdict, EXIT_CLEAN};

/// Time limit of all `git` calls of one `sitecraft init` together.
pub const GIT_TIME_LIMIT: Duration = Duration::from_secs(30);

/// The directory created inside the new repository.
pub const SITECRAFT_DIR: &str = "sitecraft";

/// Run `sitecraft init <dir>` and return its exit code (0 / 1 / 2).
#[must_use]
pub fn command(dir: &Path) -> i32 {
    let v = init(dir);
    if let Verdict::Clean(_) = v {
        println!(
            "sitecraft init: created a git repository with {}/ in {}",
            SITECRAFT_DIR,
            dir.display()
        );
        return EXIT_CLEAN;
    }
    crate::report("sitecraft init", &v)
}

/// What is at `<dir>` before anything is changed.
enum Target {
    /// Nothing; `ancestor` is its nearest existing ancestor directory and
    /// `first_missing` the outermost path component init will create.
    Absent {
        ancestor: PathBuf,
        first_missing: PathBuf,
    },
    /// An existing empty directory.
    Empty,
}

/// [`command`]'s verdict (see module docs). `Clean` means initialised.
pub fn init(dir: &Path) -> Verdict {
    let dir = match absolute(dir) {
        Ok(d) => d,
        Err(why) => return Verdict::undetermined(why),
    };
    let target = match inspect(&dir) {
        Ok(Ok(t)) => t,
        Ok(Err(refusal)) => return Verdict::violation(refusal),
        Err(why) => return Verdict::undetermined(why),
    };
    let deadline = Instant::now() + GIT_TIME_LIMIT;
    let probe_at = match &target {
        Target::Absent { ancestor, .. } => ancestor.clone(),
        Target::Empty => dir.clone(),
    };
    match inside_git_repo(&probe_at, deadline) {
        Ok(false) => {}
        Ok(true) => {
            return Verdict::violation(format!(
                "{} is inside another git repository; a nested repository is refused. Choose a \
                 directory outside it",
                probe_at.display()
            ))
        }
        Err(why) => return Verdict::undetermined(why),
    }
    let created = match &target {
        Target::Absent { first_missing, .. } => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                let why = format!("cannot create {}: {e}", dir.display());
                return Verdict::undetermined(undo(first_missing, why));
            }
            Some(first_missing.clone())
        }
        Target::Empty => None,
    };
    let fail = |why: String| -> Verdict {
        let why = match &created {
            Some(top) => undo(top, why),
            None => undo_inside(&dir, why),
        };
        Verdict::undetermined(why)
    };
    if let Err(why) = git_init(&dir, deadline) {
        return fail(why);
    }
    if !dir.join(".git").is_dir() {
        return fail(format!(
            "`git init` exited 0 but {} is not a directory",
            dir.join(".git").display()
        ));
    }
    if let Err(e) = std::fs::create_dir(dir.join(SITECRAFT_DIR)) {
        return fail(format!(
            "cannot create {}: {e}",
            dir.join(SITECRAFT_DIR).display()
        ));
    }
    Verdict::from_findings(Vec::new())
}

fn absolute(dir: &Path) -> Result<PathBuf, String> {
    if dir.as_os_str().is_empty() {
        return Err("the target directory is empty; pass `sitecraft init <dir>`".to_string());
    }
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(dir))
        .map_err(|e| format!("cannot read the cwd to resolve {}: {e}", dir.display()))
}

/// `Ok(Ok(target))` usable, `Ok(Err(why))` refused (exit 1), `Err(why)`
/// undetermined (exit 2).
fn inspect(dir: &Path) -> Result<Result<Target, String>, String> {
    match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => {
            let mut entries = std::fs::read_dir(dir)
                .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
            match entries.next() {
                None => Ok(Ok(Target::Empty)),
                Some(Ok(_)) => Ok(Err(format!(
                    "{} is not empty; sitecraft init only uses a missing or empty directory \
                     (nothing was changed)",
                    dir.display()
                ))),
                Some(Err(e)) => Err(format!("cannot list {}: {e}", dir.display())),
            }
        }
        Ok(_) => Ok(Err(format!(
            "{} exists and is not a directory (nothing was changed)",
            dir.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => absent(dir),
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => Ok(Err(format!(
            "{}: a component of the path is not a directory (nothing was changed)",
            dir.display()
        ))),
        Err(e) => Err(format!("cannot inspect {}: {e}", dir.display())),
    }
}

/// `dir` is absent: find its nearest existing ancestor (must be a directory).
fn absent(dir: &Path) -> Result<Result<Target, String>, String> {
    let mut first_missing = dir.to_path_buf();
    for a in dir.ancestors().skip(1) {
        match std::fs::metadata(a) {
            Ok(m) if m.is_dir() => {
                return Ok(Ok(Target::Absent {
                    ancestor: a.to_path_buf(),
                    first_missing,
                }))
            }
            Ok(_) => {
                return Ok(Err(format!(
                    "cannot create {}: {} is not a directory (nothing was changed)",
                    dir.display(),
                    a.display()
                )))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                first_missing = a.to_path_buf();
            }
            Err(e) => return Err(format!("cannot inspect {}: {e}", a.display())),
        }
    }
    Err(format!(
        "no existing ancestor directory of {} was found",
        dir.display()
    ))
}

/// Remove `top` (a directory init created) and append the outcome to `why`.
fn undo(top: &Path, why: String) -> String {
    if !top.exists() {
        return format!("{why} (nothing was created)");
    }
    match std::fs::remove_dir_all(top) {
        Ok(()) => format!("{why} (removed {} again)", top.display()),
        Err(e) => format!(
            "{why}; additionally, {} could not be removed ({e}): remove it by hand",
            top.display()
        ),
    }
}

/// `dir` existed empty: remove what init put inside it.
fn undo_inside(dir: &Path, why: String) -> String {
    let mut failed = Vec::new();
    for name in [".git", SITECRAFT_DIR] {
        let p = dir.join(name);
        if p.exists() {
            if let Err(e) = std::fs::remove_dir_all(&p) {
                failed.push(format!("{} ({e})", p.display()));
            }
        }
    }
    if failed.is_empty() {
        why
    } else {
        format!(
            "{why}; additionally, could not remove {}: remove by hand",
            failed.join(", ")
        )
    }
}

fn git_cmd() -> Command {
    let mut c = Command::new("git");
    c.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    c
}

/// Is `dir` inside a git repository? A non-zero exit is "no" only when git
/// says "not a git repository"; anything else is `Err`.
fn inside_git_repo(dir: &Path, deadline: Instant) -> Result<bool, String> {
    let mut c = git_cmd();
    c.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"]);
    let out = run_git(c, "git rev-parse", deadline)?;
    if out.status.success() {
        return Ok(true);
    }
    if out.stderr.contains("not a git repository") {
        return Ok(false);
    }
    Err(format!(
        "`git rev-parse --is-inside-work-tree` in {} failed ({}); cannot tell whether it is \
         inside another repository: {}",
        dir.display(),
        out.status,
        out.stderr.trim()
    ))
}

fn git_init(dir: &Path, deadline: Instant) -> Result<(), String> {
    let mut c = git_cmd();
    c.arg("init").arg("-q").arg(dir);
    let out = run_git(c, "git init", deadline)?;
    if !out.status.success() {
        return Err(format!(
            "`git init {}` failed ({}): {}",
            dir.display(),
            out.status,
            out.stderr.trim()
        ));
    }
    Ok(())
}

struct GitOut {
    status: ExitStatus,
    stderr: String,
}

/// Spawn `cmd` (its own process group) and wait until `deadline`; past it
/// the group is killed and the answer is `Err` (reason `timeout`).
fn run_git(mut cmd: Command, what: &str, deadline: Instant) -> Result<GitOut, String> {
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run `{what}` ({e}); install git or put it on PATH"))?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        so.as_mut()
            .map(|s| s.read_to_end(&mut b))
            .transpose()
            .map(|_| b)
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        se.as_mut()
            .map(|s| s.read_to_end(&mut b))
            .transpose()
            .map(|_| b)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                kill_group(&mut child);
                let _ = child.wait();
                let _ = t_out.join();
                let _ = t_err.join();
                return Err(format!(
                    "timeout: `{what}` did not finish within the {}s git time limit of \
                     sitecraft init (killed)",
                    GIT_TIME_LIMIT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                kill_group(&mut child);
                let _ = child.wait();
                return Err(format!("cannot observe `{what}`: {e}"));
            }
        }
    };
    let join = |t: std::thread::JoinHandle<std::io::Result<Vec<u8>>>, name: &str| match t.join() {
        Ok(Ok(b)) => Ok(b),
        Ok(Err(e)) => Err(format!("cannot read the {name} of `{what}`: {e}")),
        Err(_) => Err(format!("the {name} reader of `{what}` panicked")),
    };
    join(t_out, "stdout")?;
    let stderr = join(t_err, "stderr")?;
    Ok(GitOut {
        status,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn kill_group(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: plain signal delivery to the child's own process group (it
        // was spawned with process_group(0), so pgid == pid).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}
