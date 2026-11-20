//! [`Effect`]: one thing a program does to the outside world, with literal
//! operands, and its shell spelling.

/// One effect of a program. `call` is the call as the program spells it
/// after import resolution (`os.system`, `subprocess.run`, `open`,
/// `pathlib.Path.unlink`), for a host's reason text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A command line handed to `/bin/sh -c` (`os.system(CMD)`,
    /// `subprocess.run(CMD, shell=True)`).
    Shell { call: String, command: String },
    /// A program run without a shell, `argv[0]` looked up on `PATH`
    /// (`subprocess.run([...])`).
    Exec { call: String, argv: Vec<String> },
    /// A file-system operation on literal paths. A relative path is relative
    /// to the program's working directory, which the host knows.
    Fs { call: String, op: FsOp },
}

/// A file-system operation (the effect vocabulary of `SUBSET.md` Table B,
/// spelled as the shell command that has it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    /// The entry is removed; `recursive` = a whole tree (`shutil.rmtree`).
    /// Spelled `rm -- P` / `rm -rf -- P`.
    Delete { path: String, recursive: bool },
    /// An empty directory is removed. Spelled `rmdir -- P`.
    RemoveDir { path: String },
    /// The file's content is replaced (also: created, or modified in place —
    /// an `r+` open is read as this, the more restrictive reading). Spelled
    /// `: > P`. `content`: see [`FsOp::Append`].
    Truncate {
        path: String,
        content: Option<String>,
    },
    /// Bytes are added at the end. Spelled `: >> P`.
    ///
    /// `content` (here and in [`FsOp::Truncate`]) is the whole text this one
    /// call writes, when the program text determines it. Only two shapes
    /// carry it: `open(P, M).write(C)` (an `open`-like call the reading
    /// models, `M` a literal mode holding `w`, `x` or `a`, `.write` called
    /// directly on that call with one positional `C` and no keyword) and
    /// `Path(P).write_text(C)` / `.write_bytes(C)` (one positional `C`, no
    /// keyword), where `C` is a string, or UTF-8 bytes, value the reading
    /// resolves. Every other write (a handle written later, an `r+` open,
    /// `os.open`, a `C` it does not resolve) has `None`: the text is NOT
    /// read, which a host must treat as unknown text, never as empty.
    Append {
        path: String,
        content: Option<String>,
    },
    /// The mode changes. Spelled `chmod MODE -- P` (octal).
    Chmod { path: String, mode: u32 },
    /// The owner / group changes. Spelled `chown OWNER -- P`.
    Chown { path: String, owner: String },
    /// `from` is renamed to `to`. Spelled `mv -- FROM TO`.
    Move { from: String, to: String },
    /// `from` is copied onto `to`. Spelled `cp [-R] -- FROM TO`.
    Copy {
        from: String,
        to: String,
        recursive: bool,
    },
    /// The link `link` is created, pointing at `target`. Spelled
    /// `ln [-s] -- TARGET LINK`.
    Link {
        target: String,
        link: String,
        symbolic: bool,
    },
    /// A directory (and its parents) is created. Spelled `mkdir -p -- P`.
    MakeDir { path: String },
    /// The file is created if missing and its times updated. Spelled
    /// `touch -- P`.
    Touch { path: String },
    /// The file is cut / extended to `size` bytes. Spelled
    /// `truncate -s SIZE -- P`; size 0 is spelled `: > P` (it is a
    /// truncation).
    Resize { path: String, size: u64 },
}

impl Effect {
    /// The call that has the effect, as the program spells it.
    pub fn call(&self) -> &str {
        match self {
            Effect::Shell { call, .. } | Effect::Exec { call, .. } | Effect::Fs { call, .. } => {
                call
            }
        }
    }

    /// True when the effect runs another program (a shell or a process).
    pub fn runs_program(&self) -> bool {
        matches!(self, Effect::Shell { .. } | Effect::Exec { .. })
    }

    /// True when the effect creates or changes the content of a file (a file
    /// a later command could run).
    pub fn writes_file(&self) -> bool {
        match self {
            Effect::Fs { op, .. } => matches!(
                op,
                FsOp::Truncate { .. }
                    | FsOp::Append { .. }
                    | FsOp::Move { .. }
                    | FsOp::Copy { .. }
                    | FsOp::Link { .. }
                    | FsOp::Touch { .. }
                    | FsOp::Resize { .. }
            ),
            Effect::Shell { .. } | Effect::Exec { .. } => false,
        }
    }

    /// The paths a file-system effect acts on (empty for a shell / process
    /// run, whose operands are the host's to read in its spelling).
    pub fn paths(&self) -> Vec<&str> {
        let Effect::Fs { op, .. } = self else {
            return Vec::new();
        };
        match op {
            FsOp::Delete { path, .. }
            | FsOp::RemoveDir { path }
            | FsOp::Truncate { path, .. }
            | FsOp::Append { path, .. }
            | FsOp::Chmod { path, .. }
            | FsOp::Chown { path, .. }
            | FsOp::MakeDir { path }
            | FsOp::Touch { path }
            | FsOp::Resize { path, .. } => vec![path],
            FsOp::Move { from, to } | FsOp::Copy { from, to, .. } => vec![from, to],
            FsOp::Link { target, link, .. } => vec![target, link],
        }
    }

    /// The shell command that has exactly this effect on exactly these
    /// operands, every operand single-quoted. `None` when an operand cannot
    /// be spelled as a shell word (it holds a NUL byte) — the caller must
    /// treat that as undetermined.
    ///
    /// The spelling is the contract between this crate and a host: a host
    /// judges it with its own shell rules, so an effect of an embedded
    /// program gets the verdict its shell spelling gets.
    pub fn shell_spelling(&self) -> Option<String> {
        match self {
            Effect::Shell { command, .. } => Some(format!("sh -c {}", sq(command)?)),
            Effect::Exec { argv, .. } => {
                if argv.is_empty() {
                    return None;
                }
                words(argv.iter().map(String::as_str))
            }
            Effect::Fs { op, .. } => op.shell_spelling(),
        }
    }
}

impl FsOp {
    fn shell_spelling(&self) -> Option<String> {
        Some(match self {
            FsOp::Delete { path, recursive } => {
                let flags = if *recursive { "-rf " } else { "" };
                format!("rm {flags}-- {}", sq(path)?)
            }
            FsOp::RemoveDir { path } => format!("rmdir -- {}", sq(path)?),
            FsOp::Truncate { path, .. } => format!(": > {}", sq(path)?),
            FsOp::Append { path, .. } => format!(": >> {}", sq(path)?),
            FsOp::Chmod { path, mode } => format!("chmod {mode:o} -- {}", sq(path)?),
            FsOp::Chown { path, owner } => format!("chown {} -- {}", sq(owner)?, sq(path)?),
            FsOp::Move { from, to } => format!("mv -- {} {}", sq(from)?, sq(to)?),
            FsOp::Copy {
                from,
                to,
                recursive,
            } => {
                let flags = if *recursive { "-R " } else { "" };
                format!("cp {flags}-- {} {}", sq(from)?, sq(to)?)
            }
            FsOp::Link {
                target,
                link,
                symbolic,
            } => {
                let flags = if *symbolic { "-s " } else { "" };
                format!("ln {flags}-- {} {}", sq(target)?, sq(link)?)
            }
            FsOp::MakeDir { path } => format!("mkdir -p -- {}", sq(path)?),
            FsOp::Touch { path } => format!("touch -- {}", sq(path)?),
            // Resizing to zero bytes IS truncation: spelt the way the shell
            // truncates (`: > P`, which also creates a missing file — a
            // superset of the effect), so it gets the truncation rules.
            FsOp::Resize { path, size: 0 } => format!(": > {}", sq(path)?),
            FsOp::Resize { path, size } => format!("truncate -s {size} -- {}", sq(path)?),
        })
    }
}

/// `s` as one single-quoted shell word (`'` spelled `'\''`); `None` with a
/// NUL byte, which no shell word can hold. Public so a host spells its own
/// words (a `cd DIR && ` prefix) the same way.
pub fn sq(s: &str) -> Option<String> {
    if s.contains('\0') {
        return None;
    }
    Some(format!("'{}'", s.replace('\'', "'\\''")))
}

fn words<'a>(it: impl Iterator<Item = &'a str>) -> Option<String> {
    let quoted: Option<Vec<String>> = it.map(sq).collect();
    Some(quoted?.join(" "))
}
