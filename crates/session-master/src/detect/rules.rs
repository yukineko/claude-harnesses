//! Rules split out of `detect.rs` (file-growth cap): written-then-run
//! tracking, the non-literal command word, `docker`, `man`, and env vars that
//! hold a program. Same contract: master-only effect = `found`, cannot tell =
//! `undet`, otherwise silent.

use super::{
    basename, joined, literal, mentions_sensitive, Arg, Stdin, Walker, CONTROL_SUBCOMMANDS,
    GIT_MOVES_HEAD, MAX_DEPTH, SESSION_ENV,
};
use std::path::Path;

/// Words that are master-only effects when they follow a command word that
/// cannot be read (`$G push`). Git subcommands that move a branch or ref are
/// all here; an unknown alias-like word is handled separately.
const MASTER_WORDS: &[&str] = &[
    "push",
    "send-pack",
    "http-push",
    "merge",
    "pull",
    "rebase",
    "reset",
    "cherry-pick",
    "revert",
    "am",
    "update-ref",
    "branch",
    "fetch",
    "symbolic-ref",
    "checkout",
    "switch",
    "rollout",
];

/// Env vars whose value is run as a program by the tool that reads them.
const EXEC_ENV: &[&str] = &[
    "MANPAGER",
    "PAGER",
    "GIT_PAGER",
    "BROWSER",
    "MANOPT",
    "EDITOR",
    "VISUAL",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_EXTERNAL_DIFF",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "LESSOPEN",
    "LESSCLOSE",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "GIT_PROXY_COMMAND",
    "GIT_EXEC_PATH",
];

const DOCKER_GLOBAL_VALUE: &[&str] = &[
    "--config",
    "-H",
    "--host",
    "-c",
    "--context",
    "-l",
    "--log-level",
    "--tlscacert",
    "--tlscert",
    "--tlskey",
    // podman
    "--root",
    "--runroot",
    "--connection",
    "--url",
    "--identity",
    "--storage-driver",
    "--storage-opt",
    "--cgroup-manager",
    "--conmon",
    "--events-backend",
    "--hooks-dir",
    "--namespace",
    "--network-cmd-path",
    "--imagestore",
    "--tmpdir",
    "--runtime",
    "--runtime-flag",
    "--module",
    "--ssh",
    "--cni-config-dir",
    "--db-backend",
    "--registries-conf",
    "--signature-policy",
];

const DOCKER_EXEC_VALUE: &[&str] = &[
    "-e",
    "--env",
    "-u",
    "--user",
    "-w",
    "--workdir",
    "--env-file",
    "--detach-keys",
];

/// Subcommands that run nothing on this host (`exec` is judged separately).
const DOCKER_INERT: &[&str] = &[
    "logs", "ps", "images", "inspect", "pull", "stop", "start", "restart", "rm", "top", "stats",
    "version", "info", "cp",
];

/// Absolute, lexically normalised (`//`, `/./`, `/x/../` collapsed) path;
/// quotes are dropped and a relative path is taken from `cwd`.
pub(super) fn norm_path(s: &str, cwd: &Path) -> String {
    let s: String = s.chars().filter(|c| !matches!(c, '"' | '\'')).collect();
    let abs = if s.starts_with('/') {
        s
    } else {
        format!("{}/{}", cwd.display(), s)
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in abs.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(seg),
        }
    }
    format!("/{}", out.join("/"))
}

/// Is `n` a variable whose value is run as a program (or swaps git's config)?
fn is_exec_env(n: &str) -> bool {
    EXEC_ENV.contains(&n) || (n.starts_with("GIT_CONFIG") && n != "GIT_CONFIG_NOSYSTEM")
}

/// A bare word like `sy` / `status` (no dot, slash or extension): at a
/// subcommand position it may be a git alias.
fn alias_like(s: &str) -> bool {
    // git lower-cases config variable names, so an alias (`alias.<name>`) can
    // only ever be spelled in lower case: `README` / `Makefile` are file names.
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Walker {
    pub(super) fn np(&self, s: &str) -> String {
        norm_path(s, &self.cwd)
    }

    /// Was `path` (already normalised) written by this program, directly or
    /// inside a directory it recursively filled?
    pub(super) fn was_written(&self, path: &str) -> bool {
        self.written.iter().any(|w| w == path)
            || self
                .written_dirs
                .iter()
                .any(|d| path.starts_with(&format!("{d}/")))
    }

    /// Did the program write any file (other than a `/dev/` sink)?
    pub(super) fn wrote_files(&self) -> bool {
        !self.written_dirs.is_empty() || self.written.iter().any(|p| !p.starts_with("/dev/"))
    }

    fn note_written(&mut self, a: &Arg) {
        let p = self.np(a.lit.as_deref().unwrap_or(&a.raw));
        self.written.push(p);
    }

    fn note_written_dir(&mut self, a: &Arg) {
        let p = self.np(a.lit.as_deref().unwrap_or(&a.raw));
        self.written.push(p.clone());
        self.written_dirs.push(p);
    }

    /// Record the files a writer program (`cp`, `tee`, `dd of=`, `curl -o`
    /// ...) creates, so running one of them later in the same program can be
    /// recognised. Copying into a directory lands only at `dir/basename(src)`
    /// unless the copy is recursive / a non-literal source (then anywhere in
    /// the directory).
    pub(super) fn note_writer(&mut self, base: &str, args: &[Arg]) {
        match base {
            "cp" | "mv" | "ln" | "install" | "rsync" | "ditto" | "cpio" => {
                let mut target_dir: Option<&Arg> = None;
                let mut recursive = base == "rsync";
                let mut pos: Vec<&Arg> = Vec::new();
                let mut k = 0usize;
                while let Some(a) = args.get(k) {
                    match a.lit.as_deref() {
                        Some("-t" | "--target-directory") => {
                            target_dir = args.get(k + 1);
                            k += 2;
                        }
                        Some(s) if s.starts_with("--target-directory=") => {
                            self.written_dirs
                                .push(self.np(&s["--target-directory=".len()..]));
                            k += 1;
                        }
                        Some(s) if s.starts_with('-') && s.len() > 1 => {
                            recursive |= s == "--recursive"
                                || s == "--archive"
                                || (!s.starts_with("--") && s[1..].contains(['r', 'R', 'a']));
                            k += 1;
                        }
                        _ => {
                            pos.push(a);
                            k += 1;
                        }
                    }
                }
                if let Some(d) = target_dir {
                    self.note_written_dir(d);
                    return;
                }
                let Some((dest, srcs)) = pos.split_last() else {
                    return;
                };
                if recursive {
                    self.note_written_dir(dest);
                    return;
                }
                let dest_is_dir =
                    dest.raw.trim_matches(['"', '\'']).ends_with('/') || srcs.len() > 1;
                if !dest_is_dir {
                    self.note_written(dest);
                }
                let dp = self.np(dest.lit.as_deref().unwrap_or(&dest.raw));
                for s in srcs {
                    match s.lit.as_deref() {
                        Some(l) => {
                            let b = basename(l.trim_end_matches('/'));
                            self.written
                                .push(format!("{}/{}", dp.trim_end_matches('/'), b));
                        }
                        None => self.note_written_dir(dest),
                    }
                }
            }
            "tee" => {
                for a in args
                    .iter()
                    .filter(|a| !a.lit.as_deref().is_some_and(|s| s.starts_with('-')))
                {
                    self.note_written(a);
                }
            }
            "dd" => {
                for a in args {
                    if let Some(v) = a.lit.as_deref().and_then(|s| s.strip_prefix("of=")) {
                        let p = self.np(v);
                        self.written.push(p);
                    }
                }
            }
            "curl" | "wget" => {
                // Option letter that names the output file, per tool.
                let out_letter = if base == "curl" { 'o' } else { 'O' };
                for (k, a) in args.iter().enumerate() {
                    let Some(s) = a.lit.as_deref() else {
                        continue;
                    };
                    if let Some(v) = s
                        .strip_prefix("--output=")
                        .or_else(|| s.strip_prefix("--output-document="))
                    {
                        let p = self.np(v);
                        self.written.push(p);
                    } else if matches!(s, "--output" | "--output-document") {
                        if let Some(v) = args.get(k + 1) {
                            self.note_written(v);
                        }
                    } else if matches!(s, "-O" | "--remote-name" | "--remote-name-all" | "-J")
                        && base == "curl"
                    {
                        // Saved under a name the server picks, in the cwd.
                        let cwd = self.cwd.display().to_string();
                        self.written_dirs.push(cwd);
                    } else if s.starts_with('-') && !s.starts_with("--") {
                        // A short cluster (`-fsSLo/tmp/g`, `-fo /tmp/g`).
                        if let Some(at) = s[1..].find(out_letter) {
                            let inline = &s[1 + at + 1..];
                            if inline.is_empty() {
                                if let Some(v) = args.get(k + 1) {
                                    self.note_written(v);
                                }
                            } else {
                                let p = self.np(inline);
                                self.written.push(p);
                            }
                        }
                    }
                }
            }
            "tar" | "unzip" | "bsdtar" | "7z" | "7za"
                if base == "unzip"
                    || args.iter().any(|a| {
                        a.lit.as_deref().is_some_and(|s| {
                            s == "--extract"
                                || s == "x"
                                || (s.starts_with('-') && !s.starts_with("--") && s.contains('x'))
                                || (!s.starts_with('-') && s.len() < 8 && s.contains('x'))
                        })
                    }) =>
            {
                // Extraction fills a directory with files this program
                // never names: anything under the target (or the cwd).
                let mut dir: Option<String> = None;
                for (k, a) in args.iter().enumerate() {
                    if matches!(a.lit.as_deref(), Some("-C" | "-d" | "--directory")) {
                        dir = args
                            .get(k + 1)
                            .map(|v| self.np(v.lit.as_deref().unwrap_or(&v.raw)));
                    }
                }
                let d = dir.unwrap_or_else(|| self.cwd.display().to_string());
                self.written_dirs.push(d);
            }
            _ => {}
        }
    }

    /// The command word is not a literal (`$EDITOR file.txt`). It may expand
    /// to anything, but the deny-by-default intent (SPEC §5.1) is to stop
    /// master-only *effects*, not every unknown word. Fail-closed rule, with
    /// no dependence on one word list: Allow only when there is at least one
    /// argument, every argument is literal, and none of them could form a
    /// master-only git operation: no git-subcommand word that moves a branch
    /// or ref, no sensitive program, no git `-c key=value` (aliases and
    /// executor settings), and no bare alias-like word at the subcommand
    /// position (`$G sy origin main`). `-c CODE` without a dotted key is shell
    /// code and is parsed as a program, like a literal `bash -c`.
    pub(super) fn non_literal_command(&mut self, rest: &[Arg], stdin: &Stdin, depth: usize) {
        let args = rest.get(1..).unwrap_or(&[]);
        if args.is_empty() || args.iter().any(|a| a.lit.is_none()) {
            self.undet(&joined(rest), "command 名が literal でない");
            return;
        }
        // A program read from stdin (`-s`, `-`, any short cluster with `s`):
        // judge the text we can see, refuse what we cannot.
        let reads_stdin = args.iter().any(|a| {
            a.lit.as_deref().is_some_and(|s| {
                s == "-" || (s.starts_with('-') && !s.starts_with("--") && s[1..].contains('s'))
            })
        });
        if reads_stdin {
            for b in &stdin.bodies {
                self.judge_code(b, depth);
            }
            if !stdin.files.is_empty() || (stdin.bodies.is_empty() && stdin.piped) {
                self.undet(
                    &joined(rest),
                    "非 literal の command が stdin からプログラムを読む（中身は読めない）",
                );
            }
        }
        let mut first_pos: Option<&str> = None;
        let mut j = 0usize;
        while let Some(s) = args.get(j).and_then(|a| a.lit.as_deref()) {
            if s.starts_with("--config-env") {
                self.undet(
                    &joined(rest),
                    "非 literal の command 名に git --config-env（設定値が env 経由で読めない）",
                );
                return;
            }
            // `-ec CODE`, `-c -- CODE`: any short cluster with `c` takes code.
            if s != "-c"
                && s.starts_with('-')
                && !s.starts_with("--")
                && s.len() > 2
                && s[1..].contains('c')
            {
                let mut k = j + 1;
                while args
                    .get(k)
                    .and_then(|a| a.lit.as_deref())
                    .is_some_and(|w| w == "--" || (w.starts_with('-') && w.len() > 1))
                {
                    k += 1;
                }
                if let Some(body) = args.get(k).and_then(|a| a.lit.as_deref()) {
                    self.judge_code(body, depth);
                }
            }
            let named = s
                .split(|c: char| !(c.is_ascii_alphanumeric() || "_.-/+".contains(c)))
                .find(|t| {
                    MASTER_WORDS.contains(t)
                        || GIT_MOVES_HEAD.contains(t)
                        || CONTROL_SUBCOMMANDS.contains(t)
                        || *t == "--session"
                        || t.contains(SESSION_ENV)
                        || mentions_sensitive(t).is_some()
                });
            if let Some(t) = named {
                self.undet(
                    &joined(rest),
                    &format!("command 名が literal でなく、引数に master 専用の語 {t} がある"),
                );
                return;
            }
            if s == "-c" {
                let mut k = j + 1;
                while args
                    .get(k)
                    .and_then(|a| a.lit.as_deref())
                    .is_some_and(|w| w == "--" || (w.starts_with('-') && w.len() > 1))
                {
                    k += 1;
                }
                let body = args.get(j + 1).and_then(|a| a.lit.as_deref()).unwrap_or("");
                let key = body.split('=').next().unwrap_or("");
                if body.contains('=') && key.contains('.') {
                    self.undet(
                        &joined(rest),
                        "非 literal の command 名に git -c <key>=<value>",
                    );
                    return;
                }
                if depth + 1 > MAX_DEPTH {
                    self.undet(&joined(rest), "再帰深さ上限を超えた");
                    return;
                }
                if let Some(code) = args.get(k).and_then(|a| a.lit.as_deref()) {
                    self.judge_code(code, depth);
                }
                j = k + 1;
                continue;
            }
            if matches!(s, "-C" | "--git-dir" | "--work-tree" | "--namespace") {
                j += 2;
                continue;
            }
            if !s.starts_with('-') && first_pos.is_none() {
                first_pos = Some(s);
            }
            j += 1;
        }
        if let Some(p) = first_pos {
            if alias_like(p) {
                self.undet(
                    &joined(rest),
                    &format!("非 literal の command 名の後の {p} は git alias の可能性"),
                );
            }
        }
    }

    /// Code handed to a program we cannot identify (`$X -c CODE`, stdin):
    /// judged as shell when it parses as shell. When it does not parse it is
    /// not shell (`python -c 'import sys; print(1)'`), so only text that could
    /// still hide a sensitive program (a name, an expansion, an obfuscating
    /// quote pair) is refused.
    fn judge_code(&mut self, body: &str, depth: usize) {
        let parses = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            brush_parser::Parser::new(body.as_bytes(), &brush_parser::ParserOptions::default())
                .parse_program()
                .is_ok()
        }))
        .unwrap_or(false);
        if parses {
            self.program(body, depth + 1);
        } else if mentions_sensitive(body).is_some()
            || body.contains(['$', '`', '\\'])
            || body.contains("''")
            || body.contains("\"\"")
        {
            self.undet(
                body,
                "非 literal の command に渡されたコードが shell として読めない",
            );
        }
    }

    /// `git config KEY VALUE` / `git -c KEY=VALUE` / `--config-env`: a key
    /// that names a program (pager, fsmonitor, sshCommand, a helper, an
    /// alias, an include...) makes its value code, judged as a program.
    pub(super) fn config_entry(&mut self, key: &str, value: Option<&str>, depth: usize) {
        let k = key.to_ascii_lowercase();
        let first = k.split('.').next().unwrap_or("");
        let last = k.rsplit('.').next().unwrap_or("");
        if first == "alias" || first == "include" || first == "includeif" || last == "hookspath" {
            self.undet(
                key,
                "git config の alias / include / hooksPath は実行物を指す",
            );
            return;
        }
        let execish = first == "pager"
            || matches!(
                last,
                "pager"
                    | "fsmonitor"
                    | "sshcommand"
                    | "editor"
                    | "askpass"
                    | "program"
                    | "helper"
                    | "cmd"
                    | "command"
                    | "textconv"
                    | "external"
                    | "driver"
                    | "gitproxy"
                    | "packobjectshook"
                    | "clean"
                    | "smudge"
                    | "process"
                    | "alternaterefscommand"
            );
        if !execish {
            return;
        }
        match value {
            Some(v) if depth < MAX_DEPTH => self.program(v.trim_start_matches('!'), depth + 1),
            _ => self.undet(key, "プログラムを指す git config の値が読めない"),
        }
    }

    /// `printf -v NAME` / `read NAME` / `mapfile NAME`: sets a variable by
    /// name, so an executor variable can be filled without an `=` word.
    pub(super) fn var_setter(&mut self, base: &str, args: &[Arg]) {
        let mut names: Vec<&str> = Vec::new();
        match base {
            "printf" => {
                for (k, a) in args.iter().enumerate() {
                    match a.lit.as_deref() {
                        Some("-v") => names.extend(args.get(k + 1).and_then(|v| v.lit.as_deref())),
                        Some(s) if s.starts_with("-v") => names.push(&s[2..]),
                        _ => {}
                    }
                }
            }
            "read" | "mapfile" | "readarray" => {
                names.extend(
                    args.iter()
                        .filter_map(|a| a.lit.as_deref())
                        .filter(|s| !s.starts_with('-')),
                );
            }
            _ => return,
        }
        if names.iter().any(|n| is_exec_env(n)) {
            self.undet(base, "プログラムを保持する環境変数を builtin が設定する");
        }
    }

    /// A word that sets an env var whose value is run as a program
    /// (`MANPAGER='git push; cat' man ls`): judge the value as a program.
    pub(super) fn exec_env(&mut self, name: &str, value: Option<&str>, depth: usize) {
        if name.starts_with("GIT_CONFIG") && name != "GIT_CONFIG_NOSYSTEM" {
            self.undet(name, "git の設定を env で差し替える（プログラムを指せる）");
            return;
        }
        if name == "PATH" {
            self.sets_path = true;
        }
        if !EXEC_ENV.contains(&name) {
            return;
        }
        match value {
            Some(v) if depth < MAX_DEPTH => self.program(v, depth + 1),
            _ => self.undet(name, "プログラムを保持する環境変数の値が読めない"),
        }
    }

    /// `export|declare|env NAME=value ...` words that set an executor env.
    pub(super) fn exec_env_word(&mut self, raw: &str, depth: usize) {
        if let Some((n, _)) = raw.split_once('=') {
            if is_exec_env(n) || n == "PATH" {
                let v = literal(raw).and_then(|s| s.split_once('=').map(|(_, v)| v.to_string()));
                self.exec_env(n, v.as_deref(), depth);
            }
        }
    }

    /// `man`: only its pager / browser options run a program; judge it.
    pub(super) fn man(&mut self, args: &[Arg], depth: usize) {
        let mut k = 0usize;
        while let Some(a) = args.get(k) {
            let Some(s) = a.lit.as_deref() else {
                self.undet(&a.raw, "man の引数が literal でない（pager の可能性）");
                return;
            };
            let long_val = ["--pager=", "--html=", "--browser="]
                .iter()
                .find_map(|p| s.strip_prefix(p));
            let takes_prog = matches!(s, "-P" | "--pager" | "-H" | "--html" | "-B" | "--browser");
            let cluster = s.starts_with('-')
                && !s.starts_with("--")
                && s.len() > 2
                && s[1..].contains(['P', 'H', 'B']);
            if takes_prog || cluster || long_val.is_some() {
                let v = if let Some(v) = long_val {
                    Some(v.to_string())
                } else if cluster {
                    // `-aPcmd`: the rest of the token after the letter, or the next word.
                    let at = s[1..].find(['P', 'H', 'B']).unwrap_or(0) + 2;
                    let inline = &s[at..];
                    if inline.is_empty() {
                        k += 1;
                        args.get(k).and_then(|n| n.lit.clone())
                    } else {
                        Some(inline.to_string())
                    }
                } else {
                    k += 1;
                    args.get(k).and_then(|n| n.lit.clone())
                };
                match v {
                    Some(p) if depth < MAX_DEPTH => self.program(&p, depth + 1),
                    _ => self.undet(&joined(args), "man の pager が読めない"),
                }
            }
            k += 1;
        }
    }

    /// `docker`/`podman`: `exec` runs a command in a container that may mount
    /// the repo, so the command is judged as a program; the subcommands that
    /// run nothing are skipped (after the global options and their values);
    /// anything else is an unknown program.
    pub(super) fn docker(
        &mut self,
        base: &str,
        args: &[Arg],
        rest: &[Arg],
        src: &str,
        depth: usize,
    ) {
        let mut j = 0usize;
        while let Some(s) = args.get(j).and_then(|a| a.lit.as_deref()) {
            if DOCKER_GLOBAL_VALUE.contains(&s) {
                j += 2;
            } else if s.starts_with('-') {
                j += 1;
            } else {
                break;
            }
        }
        match args.get(j).and_then(|a| a.lit.as_deref()) {
            Some("exec") => {
                let mut k = j + 1;
                // podman `exec -l/--latest` takes no container argument.
                let mut latest = false;
                while let Some(s) = args.get(k).and_then(|a| a.lit.as_deref()) {
                    if DOCKER_EXEC_VALUE.contains(&s) {
                        k += 2;
                    } else if let Some(body) = s.strip_prefix('-') {
                        latest |= base == "podman"
                            && (s == "--latest" || (!s.starts_with("--") && body.contains('l')));
                        k += 1;
                    } else {
                        break;
                    }
                }
                let cmd = args.get(if latest { k } else { k + 1 }..).unwrap_or(&[]);
                if cmd.is_empty() {
                    return;
                }
                if depth + 1 > MAX_DEPTH {
                    self.undet(&joined(cmd), "再帰深さ上限を超えた");
                    return;
                }
                self.exec(cmd, &Stdin::default(), &Vec::new(), src, depth + 1);
            }
            // An inert subcommand is only trusted when no later word is a
            // running one (an unknown global option's value may have been
            // taken for the subcommand: `podman --x ps run ...`).
            Some(sub)
                if DOCKER_INERT.contains(&sub)
                    && !args[j + 1..].iter().any(|a| {
                        matches!(
                            a.lit.as_deref(),
                            Some("exec" | "run" | "create" | "compose" | "build" | "buildx")
                        )
                    }) => {}
            _ => self.unknown_program(base, args, rest),
        }
    }

    /// Unknown program: it may be a wrapper (`setsid git push`), unless it is
    /// known not to run its arguments.
    pub(super) fn unknown_program(&mut self, base: &str, args: &[Arg], rest: &[Arg]) {
        if super::web::args_are_data(base, args) {
            return;
        }
        for a in args {
            if let Some(tok) = mentions_sensitive(&a.raw) {
                self.undet(
                    &joined(rest),
                    &format!("未知のプログラム {base} が {tok} を引数に取る（wrapper の可能性）"),
                );
                return;
            }
        }
    }
}
