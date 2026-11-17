//! Table B B.1 (file commands) option parsing and target extraction — stage
//! S3b (user ruling 2026-11-18). Spec: `SUBSET.md` Table B "Common rules" and
//! the B.1 rows.
//!
//! [`extract`] answers, for one already-evaluated [`SimpleCommand`]:
//!
//! * [`Row::None`] — the command has no B.1 row (an unknown program, a
//!   wrapper, an interpreter, an empty `argv`, an undetermined command word,
//!   `sed` without `-i`). Its handling is unchanged by S3b;
//! * [`Row::Undetermined`] — the command has a B.1 row and a word is outside
//!   it: an option not in the row's list, an undetermined word that may be an
//!   option, a `dd` operand that is not `k=v`;
//! * [`Row::Targets`] — every (effect, target word value) pair the row names.
//!   The values are as written; the caller resolves them against
//!   [`SimpleCommand::cwd`].
//!
//! NOT modelled here (stated, not hidden): the B.2 redirect rows, the B.3
//! wrappers (`sudo rm …`, `command rm …` are [`Row::None`]: their verdict is
//! the legacy one, unchanged), B.4 git, B.5 interpreters; `rmdir -p`
//! ancestors (they only matter for a placed path, and S3b places none); a
//! `find` expression word whose value is undetermined (it is not read: it
//! may be an argument of a primary, and the primaries' arities are not
//! modelled).

use super::super::{Quoting, SimpleCommand, StdinSource, Value, Word};
use super::Effect;

/// What [`extract`] found for one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Row {
    None,
    Undetermined(String),
    Targets(Vec<(Effect, Value)>),
}

type Targets = Result<Vec<(Effect, Value)>, String>;

/// The B.1 row of `cmd`, if it has one (see the module doc).
pub(super) fn extract(cmd: &SimpleCommand) -> Row {
    let Some(first) = cmd.argv.first() else {
        return Row::None;
    };
    let Value::Literal(name) = &first.value else {
        return Row::None;
    };
    let base = name.rsplit('/').next().unwrap_or(name);
    let args = cmd.argv.get(1..).unwrap_or(&[]);
    let found = match base {
        "rm" => rm(args),
        "rmdir" => plain_delete("rmdir", args, &RMDIR),
        "unlink" => plain_delete("unlink", args, &NO_OPTS),
        "mv" => mv(args),
        "cp" => cp(args),
        "install" => install(args),
        "ln" => ln(args),
        "chmod" | "chown" | "chgrp" => metadata(base, args),
        "sed" if sed_in_place(args) => sed(args),
        "tee" => tee(args),
        "truncate" => truncate(args),
        "dd" => dd(args),
        "find" => find(args),
        "xargs" => xargs(cmd, args),
        _ => return Row::None,
    };
    match found {
        Ok(t) => Row::Targets(t),
        Err(why) => Row::Undetermined(why),
    }
}

/// The option list of one row.
struct Opts {
    /// Short options without an argument.
    flags: &'static str,
    /// Short options taking an argument (attached, or the next word).
    with_arg: &'static str,
    /// Short options whose argument can only be attached (`sed -i.bak`).
    attached_arg: &'static str,
    /// Long options as `(long, canonical)`: the GNU long spelling of a listed
    /// short option (`--append` → `-a`; its arity is the short option's), or
    /// a long-only option that takes a value (`--reference` → itself).
    long: &'static [(&'static str, &'static str)],
    /// GNU argument permutation: an option word after an operand is still an
    /// option. Off for `xargs`, whose first operand starts the wrapped
    /// command.
    permute: bool,
    /// A word that looks like an option but is an operand of this command
    /// (`chmod -w f`: the mode).
    operand_like: fn(&str) -> bool,
}

const fn opts(flags: &'static str, with_arg: &'static str) -> Opts {
    Opts {
        flags,
        with_arg,
        attached_arg: "",
        long: &[],
        permute: true,
        operand_like: never,
    }
}

fn never(_: &str) -> bool {
    false
}

/// `--help` / `--version`: accepted by every row (they print and exit; the
/// operands are still extracted and judged). S3b implementer addition, kept
/// so the CA-blastguard-015 help invocations stay Allow — recorded in
/// SUBSET.md for the user's confirmation.
const UNIVERSAL_LONG: &[&str] = &["--help", "--version"];

const NO_OPTS: Opts = opts("", "");
const RMDIR: Opts = Opts {
    long: &[("--parents", "-p")],
    ..opts("p", "")
};
const RM: Opts = Opts {
    long: &[
        ("--recursive", "-r"),
        ("--force", "-f"),
        ("--dir", "-d"),
        ("--verbose", "-v"),
    ],
    ..opts("rRfiIvd", "")
};
const TARGET_DIR: (&str, &str) = ("--target-directory", "-t");
const NO_TARGET_DIR: (&str, &str) = ("--no-target-directory", "-T");
const MV: Opts = Opts {
    long: &[
        ("--force", "-f"),
        ("--interactive", "-i"),
        ("--no-clobber", "-n"),
        ("--verbose", "-v"),
        NO_TARGET_DIR,
        TARGET_DIR,
    ],
    ..opts("finvT", "t")
};
const CP: Opts = Opts {
    long: &[("--recursive", "-r"), ("--archive", "-a"), TARGET_DIR],
    ..opts("rRa", "t")
};
const INSTALL: Opts = Opts {
    long: &[
        ("--directory", "-d"),
        ("--mode", "-m"),
        ("--owner", "-o"),
        ("--group", "-g"),
        TARGET_DIR,
    ],
    ..opts("d", "mogt")
};
const LN: Opts = Opts {
    long: &[
        ("--symbolic", "-s"),
        ("--force", "-f"),
        ("--no-dereference", "-n"),
        ("--verbose", "-v"),
        NO_TARGET_DIR,
        TARGET_DIR,
    ],
    ..opts("sfnvT", "t")
};
const CHOWN: Opts = Opts {
    long: &[("--recursive", "-R"), ("--reference", "--reference")],
    ..opts("R", "")
};
const CHMOD: Opts = Opts {
    operand_like: chmod_minus_mode,
    ..CHOWN
};
const SED: Opts = Opts {
    attached_arg: "i",
    long: &[
        ("--in-place", "-i"),
        ("--expression", "-e"),
        ("--file", "-f"),
    ],
    ..opts("", "ef")
};
const TEE: Opts = Opts {
    long: &[("--append", "-a"), ("--ignore-interrupts", "-i")],
    ..opts("aip", "")
};
const TRUNCATE: Opts = Opts {
    long: &[("--size", "-s"), ("--reference", "-r")],
    ..opts("", "sr")
};
const XARGS: Opts = Opts {
    permute: false,
    long: &[
        ("--null", "-0"),
        ("--no-run-if-empty", "-r"),
        ("--verbose", "-t"),
        ("--max-args", "-n"),
        ("--max-procs", "-P"),
        ("--delimiter", "-d"),
    ],
    ..opts("0rt", "nILPd")
};

/// What [`parse`] read.
struct Parsed {
    /// `(option, argument)`: a short option as `-x`, a long one as written
    /// before any `=`.
    options: Vec<(String, Option<Value>)>,
    operands: Vec<Word>,
}

impl Parsed {
    fn has(&self, o: &str) -> bool {
        self.options.iter().any(|(n, _)| n == o)
    }

    fn arg(&self, o: &str) -> Option<&Value> {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| n == o)
            .and_then(|(_, v)| v.as_ref())
    }
}

/// True when the word's value provably does not start with `-`, read off
/// its raw text: after at most one opening quote, the first character is a
/// literal that no expansion replaces (`"/tmp/$x"`, `~user/f`, `s/$a/b/`).
/// Anything else (`"$x"`, `$(…)`, `\-r`, `$'…'`) may be an option.
fn cannot_be_option(w: &Word) -> bool {
    let raw = w.raw.as_str();
    let body = raw
        .strip_prefix('"')
        .or_else(|| raw.strip_prefix('\''))
        .unwrap_or(raw);
    body.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '~' | '+'))
}

/// Parse `args` by `o` (SUBSET.md Table B common rules): an option not in
/// the list, or an undetermined word that may be an option, is `Err`; `--`
/// ends the options.
fn parse(cmd: &str, args: &[Word], o: &Opts) -> Result<Parsed, String> {
    let mut p = Parsed {
        options: Vec::new(),
        operands: Vec::new(),
    };
    let mut ended = false;
    // The first option not in the list (see the end of this function).
    let mut unlisted: Option<String> = None;
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        if ended {
            p.operands.push(w.clone());
            continue;
        }
        let s = match &w.value {
            Value::Literal(s) => s.as_str(),
            Value::Undetermined(r) => {
                if cannot_be_option(w) {
                    p.operands.push(w.clone());
                    ended |= !o.permute;
                    continue;
                }
                return Err(format!(
                    "`{cmd}` word `{}` is undetermined ({r}) and may be an option",
                    w.raw
                ));
            }
        };
        if s == "--" {
            ended = true;
            continue;
        }
        if s == "-" || !s.starts_with('-') || (o.operand_like)(s) {
            p.operands.push(w.clone());
            ended |= !o.permute;
            continue;
        }
        if s.starts_with("--") {
            let (name, eq) = match s.split_once('=') {
                Some((n, v)) => (n, Some(Value::Literal(v.to_string()))),
                None => (s, None),
            };
            if UNIVERSAL_LONG.contains(&name) && eq.is_none() {
                p.options.push((name.to_string(), None));
                continue;
            }
            let Some(&(_, canon)) = o.long.iter().find(|(l, _)| *l == name) else {
                unlisted.get_or_insert(format!("`{cmd}` option `{s}` not modelled"));
                continue;
            };
            let short = canon.strip_prefix('-').filter(|c| c.chars().count() == 1);
            let flag = short.is_some_and(|c| o.flags.contains(c));
            let optional = short.is_some_and(|c| o.attached_arg.contains(c));
            let v = match eq {
                Some(_) if flag => {
                    return Err(format!("`{cmd}` option `{s}` takes no value"));
                }
                v @ Some(_) => v,
                None if flag || optional => None,
                None => match args.get(i) {
                    Some(a) => {
                        i += 1;
                        Some(a.value.clone())
                    }
                    None => return Err(format!("`{cmd}` option `{s}` without its argument")),
                },
            };
            p.options.push((canon.to_string(), v));
            continue;
        }
        let body = s.get(1..).unwrap_or("");
        for (k, ch) in body.char_indices() {
            let rest = body.get(k + ch.len_utf8()..).unwrap_or("");
            let name = format!("-{ch}");
            if o.flags.contains(ch) {
                p.options.push((name, None));
                continue;
            }
            if o.attached_arg.contains(ch) {
                let v = (!rest.is_empty()).then(|| Value::Literal(rest.to_string()));
                p.options.push((name, v));
                break;
            }
            if o.with_arg.contains(ch) {
                let v = if rest.is_empty() {
                    match args.get(i) {
                        Some(a) => {
                            i += 1;
                            a.value.clone()
                        }
                        None => return Err(format!("`{cmd}` option `-{ch}` without its argument")),
                    }
                } else {
                    Value::Literal(rest.to_string())
                };
                p.options.push((name, Some(v)));
                break;
            }
            unlisted.get_or_insert(format!("`{cmd}` option `-{ch}` (in `{s}`) not modelled"));
            break;
        }
    }
    match unlisted {
        // Every word is a literal option without an argument (`rm -h`,
        // `tee --frobnicate`): no word is left for an unmodelled option to
        // turn into, or away from, a target, so there is nothing to judge.
        // S3b implementer addition (CA-blastguard-011's `rm -h` Allow),
        // recorded in SUBSET.md for the user's confirmation.
        Some(_) if p.operands.is_empty() && p.options.iter().all(|(_, v)| v.is_none()) => Ok(p),
        Some(why) => Err(why),
        None => Ok(p),
    }
}

fn values(ws: Vec<Word>, effect: Effect) -> Vec<(Effect, Value)> {
    ws.into_iter().map(|w| (effect, w.value)).collect()
}

fn rm(args: &[Word]) -> Targets {
    let p = parse("rm", args, &RM)?;
    let recursive = p.has("-r") || p.has("-R");
    Ok(values(p.operands, Effect::Delete { recursive }))
}

fn plain_delete(cmd: &str, args: &[Word], o: &Opts) -> Targets {
    let p = parse(cmd, args, o)?;
    Ok(values(p.operands, Effect::Delete { recursive: false }))
}

/// The operands split into (sources, destination): every operand and the
/// `-t DIR` argument, else all but the last and the last.
fn sources_and_dest(p: Parsed) -> (Vec<Word>, Option<Value>) {
    if let Some(dir) = p.arg("-t").cloned() {
        return (p.operands, Some(dir));
    }
    let mut ops = p.operands;
    let dest = ops.pop().map(|w| w.value);
    (ops, dest)
}

fn mv(args: &[Word]) -> Targets {
    let (sources, dest) = sources_and_dest(parse("mv", args, &MV)?);
    let mut t = values(sources, Effect::Delete { recursive: true });
    t.extend(dest.map(|d| (Effect::Truncate, d)));
    Ok(t)
}

fn cp(args: &[Word]) -> Targets {
    let p = parse("cp", args, &CP)?;
    let tree = p.has("-r") || p.has("-R") || p.has("-a");
    let (_, dest) = sources_and_dest(p);
    let mut t = Vec::new();
    if let Some(d) = dest {
        if tree {
            t.push((Effect::CreateTree, d.clone()));
        }
        t.push((Effect::Truncate, d));
    }
    Ok(t)
}

fn install(args: &[Word]) -> Targets {
    let p = parse("install", args, &INSTALL)?;
    if p.has("-d") {
        return Ok(values(p.operands, Effect::CreateTree));
    }
    let (_, dest) = sources_and_dest(p);
    Ok(dest.map(|d| (Effect::Truncate, d)).into_iter().collect())
}

fn ln(args: &[Word]) -> Targets {
    let p = parse("ln", args, &LN)?;
    if p.arg("-t").is_none() && p.operands.len() == 1 {
        // One operand: the link is its basename, in the cwd.
        let link = match p.operands.first().map(|w| &w.value) {
            Some(Value::Literal(s)) => Value::Literal(
                s.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(s)
                    .to_string(),
            ),
            Some(u) => u.clone(),
            None => return Ok(Vec::new()),
        };
        return Ok(vec![(Effect::Link, link)]);
    }
    let (_, dest) = sources_and_dest(p);
    Ok(dest.map(|d| (Effect::Link, d)).into_iter().collect())
}

/// `chmod -w f`: a symbolic mode that starts with `-` is the mode operand.
fn chmod_minus_mode(s: &str) -> bool {
    s.strip_prefix('-')
        .is_some_and(|m| !m.is_empty() && m.chars().all(|c| "rwxXst".contains(c)))
}

fn metadata(cmd: &str, args: &[Word]) -> Targets {
    let o = if cmd == "chmod" { &CHMOD } else { &CHOWN };
    let p = parse(cmd, args, o)?;
    let mut ops = p.operands.into_iter();
    if !p.options.iter().any(|(n, _)| n == "--reference") {
        // The mode / owner spec.
        let _ = ops.next();
    }
    Ok(values(ops.collect(), Effect::Metadata))
}

/// `sed` has a B.1 row only in place: a literal `-…i…` cluster or
/// `--in-place[=…]` before `--`.
fn sed_in_place(args: &[Word]) -> bool {
    for w in args {
        let Value::Literal(s) = &w.value else {
            continue;
        };
        if s == "--" {
            return false;
        }
        if s.starts_with("--in-place")
            || (!s.starts_with("--") && s.starts_with('-') && s.contains('i'))
        {
            return true;
        }
    }
    false
}

fn sed(args: &[Word]) -> Targets {
    // BSD `-i ''`: the empty word right after a bare `-i` is its suffix.
    let mut kept: Vec<Word> = Vec::with_capacity(args.len());
    let mut prev_bare_i = false;
    for w in args {
        let empty = matches!(&w.value, Value::Literal(s) if s.is_empty());
        if !(prev_bare_i && empty) {
            kept.push(w.clone());
        }
        prev_bare_i = matches!(&w.value, Value::Literal(s) if s == "-i");
    }
    let p = parse("sed", &kept, &SED)?;
    let scripted = p.has("-e") || p.has("-f");
    let mut ops = p.operands.into_iter();
    if !scripted {
        // The script operand.
        let _ = ops.next();
    }
    Ok(values(ops.collect(), Effect::Rewrite))
}

fn tee(args: &[Word]) -> Targets {
    let p = parse("tee", args, &TEE)?;
    let effect = if p.has("-a") {
        Effect::Append
    } else {
        Effect::Truncate
    };
    Ok(values(p.operands, effect))
}

fn truncate(args: &[Word]) -> Targets {
    let p = parse("truncate", args, &TRUNCATE)?;
    Ok(values(p.operands, Effect::Truncate))
}

/// The `key` of a `key=…` raw word, when it is a plain identifier.
fn kv_key(raw: &str) -> Option<&str> {
    let (k, _) = raw.split_once('=')?;
    (!k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(k)
}

fn dd(args: &[Word]) -> Targets {
    let mut of: Vec<Value> = Vec::new();
    let mut notrunc = false;
    for w in args {
        match &w.value {
            Value::Literal(s) => match kv_key(s) {
                Some("of") => of.push(Value::Literal(s.get(3..).unwrap_or("").to_string())),
                Some("conv") => notrunc |= s.contains("notrunc"),
                Some(_) => {}
                None => return Err(format!("`dd` operand `{s}` is not `key=value`")),
            },
            Value::Undetermined(r) => match kv_key(&w.raw) {
                Some("of") => of.push(Value::Undetermined(r.clone())),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "`dd` operand `{}` is undetermined ({r}) and not a `key=` word",
                        w.raw
                    ))
                }
            },
        }
    }
    let effect = if notrunc {
        Effect::Rewrite
    } else {
        Effect::Truncate
    };
    Ok(of.into_iter().map(|v| (effect, v)).collect())
}

fn find(args: &[Word]) -> Targets {
    let lit = |j: usize| match args.get(j).map(|w| &w.value) {
        Some(Value::Literal(s)) => Some(s.as_str()),
        _ => None,
    };
    let mut i = 0;
    loop {
        match lit(i) {
            Some("-H" | "-L" | "-P") => i += 1,
            Some("-D") => i += 2,
            Some(s) if s.starts_with("-O") => i += 1,
            _ => break,
        }
    }
    let mut roots: Vec<Value> = Vec::new();
    while let Some(w) = args.get(i) {
        if matches!(&w.value, Value::Literal(s)
            if s.starts_with('-') || matches!(s.as_str(), "(" | "!" | ")" | ","))
        {
            break;
        }
        roots.push(w.value.clone());
        i += 1;
    }
    let mut t = Vec::new();
    let mut delete = false;
    while let Some(w) = args.get(i) {
        i += 1;
        match &w.value {
            Value::Literal(s) if s == "-delete" => delete = true,
            Value::Literal(s)
                if matches!(s.as_str(), "-fprint" | "-fprint0" | "-fprintf" | "-fls") =>
            {
                if let Some(f) = args.get(i) {
                    t.push((Effect::Truncate, f.value.clone()));
                    i += 1;
                }
            }
            _ => {}
        }
    }
    if delete {
        if roots.is_empty() {
            roots.push(Value::Literal(".".into()));
        }
        t.extend(
            roots
                .into_iter()
                .map(|r| (Effect::Delete { recursive: true }, r)),
        );
    }
    Ok(t)
}

fn literal_word(item: &str, like: &SimpleCommand) -> Word {
    Word {
        raw: item.to_string(),
        value: Value::Literal(item.to_string()),
        quoting: Quoting::Single,
        span: like.span.clone(),
    }
}

fn undetermined_word(why: &str, like: &SimpleCommand) -> Word {
    Word {
        raw: String::new(),
        value: Value::Undetermined(why.to_string()),
        quoting: Quoting::Unquoted,
        span: like.span.clone(),
    }
}

/// The wrapped command with `argv`, judged by its own row.
fn wrapped_row(cmd: &SimpleCommand, argv: Vec<Word>) -> Targets {
    let sub = SimpleCommand {
        argv,
        ..cmd.clone()
    };
    match extract(&sub) {
        Row::None => Ok(Vec::new()),
        Row::Undetermined(why) => Err(why),
        Row::Targets(t) => Ok(t),
    }
}

/// The items `xargs` reads from a literal stdin, or `None` when the split is
/// not modelled (quotes / backslashes in the default mode, a `-d` that is not
/// one literal character).
fn xargs_items(s: &str, p: &Parsed) -> Option<Vec<String>> {
    let items: Vec<String> = if p.has("-0") {
        s.split('\0').map(str::to_string).collect()
    } else if let Some(d) = p.arg("-d") {
        let Value::Literal(d) = d else {
            return None;
        };
        let mut cs = d.chars();
        let (Some(c), None) = (cs.next(), cs.next()) else {
            return None;
        };
        s.split(c).map(str::to_string).collect()
    } else {
        if s.contains(['"', '\'', '\\']) {
            return None;
        }
        s.split_whitespace().map(str::to_string).collect()
    };
    Some(items.into_iter().filter(|i| !i.is_empty()).collect())
}

fn xargs(cmd: &SimpleCommand, args: &[Word]) -> Targets {
    let p = parse("xargs", args, &XARGS)?;
    let wrapped = p.operands.clone();
    let Some(head) = wrapped.first() else {
        // `echo`: no effect.
        return Ok(Vec::new());
    };
    if let Value::Undetermined(r) = &head.value {
        return Err(format!("`xargs` command word is undetermined ({r})"));
    }
    let literal_stdin = match &cmd.stdin {
        StdinSource::Literal(s) => Some(s.as_str()),
        _ => None,
    };
    if let Some(Value::Literal(repl)) = p.arg("-I") {
        let lines: Option<Vec<&str>> = literal_stdin.map(|s| {
            s.lines()
                .map(str::trim_start)
                .filter(|l| !l.is_empty())
                .collect()
        });
        let Some(lines) = lines else {
            // Each argument holding the replacement string comes from stdin.
            let argv = wrapped
                .iter()
                .map(|w| match &w.value {
                    Value::Literal(s) if s.contains(repl.as_str()) => {
                        undetermined_word("xargs -I replacement read from stdin", cmd)
                    }
                    _ => w.clone(),
                })
                .collect();
            return wrapped_row(cmd, argv);
        };
        let mut t = Vec::new();
        for line in lines {
            let argv = wrapped
                .iter()
                .map(|w| match &w.value {
                    Value::Literal(s) if s.contains(repl.as_str()) => {
                        literal_word(&s.replace(repl.as_str(), line), cmd)
                    }
                    _ => w.clone(),
                })
                .collect();
            t.extend(wrapped_row(cmd, argv)?);
        }
        return Ok(t);
    }
    if p.has("-I") {
        return Err("`xargs -I` replacement string is undetermined".into());
    }
    let mut argv = wrapped;
    match literal_stdin.map(|s| xargs_items(s, &p)) {
        Some(Some(items)) => argv.extend(items.iter().map(|i| literal_word(i, cmd))),
        Some(None) => argv.push(undetermined_word(
            "xargs operands: literal stdin split not modelled",
            cmd,
        )),
        None => argv.push(undetermined_word("xargs operands read from stdin", cmd)),
    }
    wrapped_row(cmd, argv)
}
