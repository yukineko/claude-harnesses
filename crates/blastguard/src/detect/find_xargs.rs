use super::{
    analyze_code_interpreter, command_candidates, confined_ask, depth_exhausted, detect_bash,
    is_code_interpreter, is_shell, normalized_command, Ctx, Decision, VerdictAcc, MAX_SHELL_DEPTH,
};

/// find's own global options, which sit BEFORE the search roots and are not
/// roots themselves.
///
/// `-D` takes a separate value; the rest do not. Getting this wrong is a
/// weakening, not a cosmetic slip: reading `find -L /usr -delete` as
/// "no search root given, so the root is `.`" would place the walk in the
/// project tree while it really walks `/usr`.
fn find_search_roots<'a>(rest: &[&'a str]) -> Option<Vec<&'a str>> {
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-H" | "-L" | "-P" => i += 1,
            "-D" => i += 2,
            t if t.starts_with("-O") => i += 1,
            _ => break,
        }
    }
    let mut roots: Vec<&'a str> = Vec::new();
    while i < rest.len() {
        let t = rest[i];
        if t.is_empty() {
            return None;
        }
        if t.starts_with('-') || matches!(t, "(" | ")" | "!" | ",") {
            break;
        }
        roots.push(t);
        i += 1;
    }
    if roots.is_empty() {
        // GNU find defaults to the current directory when no path is given.
        Some(vec!["."])
    } else {
        Some(roots)
    }
}

/// Predicates that make a `find` expression name a SUBSET of the tree rather
/// than all of it.
///
/// Used only for the search-root-IS-a-safe-root case: `find . -name '*.o'
/// -delete` is ordinary work, `find . -delete` empties the project. Neither is
/// "safe" in an absolute sense — an approved Ask can still delete plenty — but
/// the second is unbounded within the tree and stays a refusal.
const NARROWING_PREDICATES: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-lname",
    "-ilname",
    "-mtime",
    "-mmin",
    "-ctime",
    "-cmin",
    "-atime",
    "-amin",
    "-newer",
    "-newermt",
    "-anewer",
    "-cnewer",
    "-size",
    "-empty",
    "-user",
    "-uid",
    "-group",
    "-gid",
    "-perm",
    "-type",
    "-xtype",
    "-links",
    "-inum",
    "-samefile",
];

/// True when the expression carries at least one [`NARROWING_PREDICATES`] entry.
fn find_is_narrowed(rest: &[&str]) -> bool {
    rest.iter().any(|t| NARROWING_PREDICATES.contains(t))
}

/// The safe root every `find` search root sits in, or `None`.
///
/// Two tiers, and the second is the one that needs justifying: a search root
/// that IS a safe root walks the whole tree (`.git` included), so it only
/// counts as bounded work when the expression names a subset of it.
fn find_confined_root(rest: &[&str], ctx: &Ctx<'_>) -> Option<String> {
    let roots = find_search_roots(rest)?;
    if let Some(root) = ctx.confined_root("find", &roots) {
        return Some(root);
    }
    if find_is_narrowed(rest) {
        return ctx.confined_root_or_root_itself("find", &roots);
    }
    None
}

pub(super) fn analyze_find(rest: &[&str], depth: usize, ctx: &Ctx<'_>) -> Decision {
    // The search roots bound everything `find` can touch, so they are what
    // decides whether this invocation is confined. Computed once, up front,
    // because both the `-delete` arm and every `-exec` arm need the same
    // answer — and because the `-exec` re-analysis below substitutes the
    // per-match placeholder with a path inside the same root.
    let confined = find_confined_root(rest, ctx);
    let mut acc = VerdictAcc::default();
    if rest.contains(&"-delete") {
        match &confined {
            Some(root) => {
                let roots = find_search_roots(rest).unwrap_or_default();
                let _ = acc.record(confined_ask("find -delete", root, &roots));
            }
            None => return Decision::deny("find -delete removes every matching file"),
        }
    }
    // Every -exec/-execdir/-ok/-okdir on the line, not just the first: a benign
    // early `-exec grep …` must not hide a later `-exec rm …`.
    for pos in rest
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(**t, "-exec" | "-execdir" | "-ok" | "-okdir"))
        .map(|(i, _)| i)
    {
        let tail = &rest[pos + 1..];
        // The command run for each match starts right after -exec/-ok, modulo
        // the same leading `VAR=val` assignments and benign wrappers
        // (`sudo`, `env`, `command`, …) that `analyze_segment` skips — resolving
        // it through `command_index` is what keeps `-exec sudo rm …` and
        // `-exec env sh -c …` in reach. A shell there
        // (`find … -exec sh -c "rm …"`) can run any destructive command.
        // -ok/-okdir are the interactive-confirmation twins of -exec/-execdir:
        // they run the same arbitrary per-match command, just gated behind a
        // y/n prompt first — that prompt does not make the payload any less
        // destructive. Normalised like every other command-word site, so
        // `-exec \rm` / `-exec "sh" -c …` cannot slip past on quoting alone.
        // D3: every candidate command-word position behind a wrapper is checked,
        // not just one computed index. See `command_candidates`.
        for ci in command_candidates(tail) {
            let c = normalized_command(tail[ci]);
            let c = c.as_str();
            if is_shell(c) {
                return Decision::deny(
                    "find -exec on a shell can run an arbitrary destructive command per match",
                );
            }
            // A non-shell interpreter with an inline-eval flag (`python3 -c`,
            // `perl -e`, `node -e`, …) is equally dangerous: the payload runs
            // per match. (The nested shell's own `-c` payload is already covered
            // by the `is_shell` arm above, which denies the whole invocation
            // without needing to look at the payload text at all.)
            // Audited by the SAME function as the bare top-level invocation
            // (`analyze_code_interpreter`), not by a second shape rule. Two
            // rules for one capability is how the mirrors drift apart: this one
            // used to deny on shape while the bare path audited the program, so
            // the identical command classified under two different rule ids
            // depending on whether a `find` wrapped it.
            if is_code_interpreter(c) {
                let verdict = analyze_code_interpreter(c, &tail[ci + 1..], depth, ctx);
                if !matches!(verdict, Decision::Allow) {
                    return verdict;
                }
            }
            // CA-blastguard-016 (false positive): this used to normalise EVERY
            // token in `rest` and deny if any of them came out as `rm`, so quoted
            // DATA matched — `find . -name '*.md' -exec grep -l 'rm' {} \;`, i.e.
            // searching for the literal string `rm`, is completely normal work and
            // was hard-blocked. Only a plausible COMMAND position is normalised
            // now.
            //
            // Narrowing justification (no fail-open): the token this checks is the
            // one `find` actually execs, resolved through the same wrapper-skipping
            // logic used everywhere else, so every genuinely destructive shape
            // (`-exec rm`, `-exec /bin/rm`, `-exec \rm`, `-exec sudo rm`) is still
            // denied. What is dropped is exactly the set of NON-command positions —
            // `-name` patterns, `-path` arguments and per-match argument data —
            // where a token named `rm` is a string being searched for, never a
            // program being run.
            if c == "rm" {
                match &confined {
                    // NOT a `return`: the payload may name absolute targets of
                    // its own (`-exec rm -rf /usr/lib`), and those are judged
                    // by the re-analysis below. Recording the Ask and carrying
                    // on is what keeps that Deny reachable — `VerdictAcc` ranks
                    // a Deny found later above this Ask.
                    Some(root) => {
                        let roots = find_search_roots(rest).unwrap_or_default();
                        let _ = acc.record(confined_ask("find -exec rm", root, &roots));
                    }
                    None => {
                        return Decision::deny("find -exec rm removes every matching file");
                    }
                }
            }
        }

        // BG-3 (fail-open regression, fixed): CA-blastguard-016 replaced the
        // blanket "normalise every token" scan with head-token resolution only.
        // That closed the `-exec grep -l 'rm'` false positive but re-opened
        // every WRAPPER bypass the blanket scan had caught — `-exec timeout 5
        // rm -rf {} +`, `-exec xargs rm -rf {} +`, `-exec stdbuf -o0 rm …`,
        // `-exec flock /tmp/l rm …` all became ALLOW, because the resolved head
        // was `timeout` / `xargs` / … and nothing looked further.
        //
        // The structural fix: re-analyse the whole `-exec` tail through
        // `detect_bash`, exactly as the `-c` and `eval` arms already re-analyse
        // their payloads. `find` really does exec this tail, so analysing it as
        // a command line is faithful — and it inherits every present and FUTURE
        // rule (rm, git, xargs, nested shells, interpreters, wrappers) instead
        // of a hand-maintained name list that rots the moment a new wrapper
        // ships. Depth-bounded like the other re-analysis arms.
        //
        // This does NOT reopen CA-blastguard-016: `detect_bash` analyses the
        // tail from its COMMAND WORD outward, so `-exec grep -l 'rm' {} \;`
        // still resolves to `grep` and the quoted DATA token `'rm'` is never in
        // a command position.
        //
        // Fail-open condition: the exec'd program is a wrapper whose command
        // word `command_index` cannot reach — an unknown wrapper name, or a
        // known one whose positional-operand count differs from
        // `wrapper_leading_operands`. That residue is strictly smaller than
        // before this fix, and it is the same residue the top-level path has.
        if depth < MAX_SHELL_DEPTH {
            // `{}` stands for a path `find` matched, and a match is always
            // UNDER one of the search roots. So when those roots are confined,
            // substituting a synthetic path inside the same safe root lets the
            // ordinary rules judge this payload with a PLACEABLE operand:
            // `-exec rm -rf {}` inside the project becomes an Ask, while
            // `-exec rm -rf /usr/lib` — an absolute target of its own — stays a
            // Deny. With no confined root the placeholder is left exactly as it
            // is, and `scope` refuses to place it at all.
            // `split_segments` splits on `;`, so a `\;`-terminated -exec
            // arrives here with its escape orphaned as a trailing `\` token
            // (and a `+`-terminated one with a trailing `+`). That token is
            // find's own punctuation, not an argument to the payload — left in
            // place it becomes an rm OPERAND, and an operand that is a bare
            // backslash can never be placed, so a perfectly confined
            // `-exec rm -rf {} \;` sank back to a Deny by way of its own
            // syntax. Measured before the strip: Deny("recursive rm (-r) can
            // delete an entire directory tree").
            let tail = match tail.last() {
                Some(&"\\") | Some(&";") | Some(&"+") => &tail[..tail.len() - 1],
                _ => tail,
            };
            let inline = match &confined {
                Some(root) => tail
                    .iter()
                    .map(|t| match *t {
                        "{}" | "'{}'" | "\"{}\"" => format!("{root}/blastguard-find-match"),
                        other => other.to_string(),
                    })
                    .collect::<Vec<String>>()
                    .join(" "),
                None => tail.join(" "),
            };
            if !inline.trim().is_empty() {
                if let Some(deny) = acc.record(detect_bash(&inline, depth + 1, ctx)) {
                    return deny;
                }
            }
        } else {
            // Cap reached: the recursion above did NOT run, so this segment was not
            // fully analysed. Record the unfinished-analysis Ask into `acc` rather
            // than returning early, so a Deny found by a later non-recursive rule in
            // this same function still outranks it (see `VerdictAcc::record`).
            let _ = acc.record(depth_exhausted());
        }
    }
    acc.finish()
}

/// `xargs` runs a command assembled from its trailing args (after xargs's own
/// option flags), appending the piped items. Without this branch a destructive
/// payload (`find … | xargs rm -rf`, `xargs -I{} sh -c "rm -rf {}"`) fell through
/// to the catch-all Allow arm. Re-analyse the inner command through `detect_bash`
/// so it reuses the rm / shell-`-c` / find logic (and the recursion bound).
pub(super) fn analyze_xargs(rest: &[&str], depth: usize, ctx: &Ctx<'_>) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return depth_exhausted();
    }
    match xargs_command_start(rest) {
        Some(start) => {
            let inner = rest[start..].join(" ");
            if inner.trim().is_empty() {
                Decision::Allow
            } else {
                detect_bash(&inner, depth + 1, ctx)
            }
        }
        None => Decision::Allow,
    }
}

/// Index in `rest` of the command word xargs will execute, skipping xargs's own
/// option flags (and the separate-token value of a value-taking short flag).
/// `--` ends option parsing. Returns None when no command word follows (xargs
/// with only flags just echoes its input — nothing destructive to re-analyse).
pub(super) fn xargs_command_start(rest: &[&str]) -> Option<usize> {
    // Value-taking xargs short flags whose value is the NEXT token when the flag
    // is given standalone (`-I {}`, `-n 1`, BSD `-J %`). Bundled forms (`-I{}`,
    // `-n1`, `-J%`) embed the value and consume only themselves. Covers both GNU
    // (I n L P s d E a) and BSD/macOS (J R S) — the default xargs on this host —
    // because a MISSED value flag would misread its value as the command word and
    // leak a destructive payload (`xargs -J % rm -rf %`) past re-analysis. An
    // EXTRA one is the same leak the other way: it swallows the command word
    // (GNU `-i`/`-e`/`-l` take only an attached value; see XARGS_VALUE_SHORT).
    const VALUE_SHORT: &str = XARGS_VALUE_SHORT;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            return if i + 1 < rest.len() {
                Some(i + 1)
            } else {
                None
            };
        }
        if let Some(stripped) = t.strip_prefix("--") {
            // Long flag: value-taking ones consume the next token unless `=`-joined.
            // NOT `--replace`, `--eof`, `--max-lines`: GNU gives those an
            // OPTIONAL value that is only ever `=`-attached, so the next token
            // is the command word (`xargs --replace sh -c {}` runs `sh`).
            // Listed here, they swallowed it.
            let takes = matches!(
                stripped.split('=').next().unwrap_or(""),
                "max-args"
                    | "max-procs"
                    | "max-chars"
                    | "delimiter"
                    | "arg-file"
                    | "process-slot-var"
            );
            i += if takes && !t.contains('=') { 2 } else { 1 };
            continue;
        }
        if t.starts_with('-') && t.len() >= 2 {
            // Short flag (bundle). A standalone value-taking flag (`-I`, `-n`)
            // consumes the next token; a bundle that embeds the value (`-I{}`,
            // `-n1`) or a boolean bundle (`-0rt`) consumes only itself.
            // `t.len() >= 2` above makes `last()` structurally Some, but the
            // compiler cannot see that, and an `unwrap()` here would be a panic
            // sitting inside a deny gate. The else arm is unreachable; it is
            // written to the RESTRICTIVE side anyway — advancing by 1 rather
            // than 2 re-examines the next token instead of skipping it, so a
            // command word can still be found rather than silently missed.
            let Some(last) = t.chars().last() else {
                i += 1;
                continue;
            };
            let standalone_value = t.len() == 2 && VALUE_SHORT.contains(last);
            // A bundle of boolean flags ENDING in a value flag (`-0a FILE`,
            // `-0n 1`) also takes the next token as that value. Read as
            // self-contained, `xargs -0a FILE sh` took `FILE` for the command
            // word and `sh` was never seen.
            let bundled_value = xargs_short_value_flag(t).is_some_and(|(_, next)| next);
            i += if standalone_value || bundled_value {
                2
            } else {
                1
            };
            continue;
        }
        return Some(i); // first non-flag token = the command word
    }
    None
}

/// Value-taking xargs short flags whose value may be the NEXT token (GNU
/// `I n L P s d E a`, BSD `J R S`).
///
/// NOT GNU `-i`, `-e`, `-l`: their value (`-i[REPL]`, `-e[EOF]`, `-l[N]`) is
/// OPTIONAL and only ever attached, so a separate next token is never theirs.
/// `-i` used to be listed here, and `xargs -i sh -c {}` read `sh` as its
/// value, never saw the shell, and was allowed. Those three live in
/// [`XARGS_OPTIONAL_ATTACHED`].
const XARGS_VALUE_SHORT: &str = "InLPsdEaJRS";

/// GNU xargs short flags with an optional ATTACHED value: the rest of the
/// bundle after one of these is its value, and it never takes the next token.
const XARGS_OPTIONAL_ATTACHED: &str = "iel";

/// For a short-option bundle `t` (`-0a`, `-aFILE`, `-n1`), the first
/// value-taking flag in it and whether its value is the NEXT token (true when
/// that flag is the bundle's last character). `None` for a bundle of boolean
/// flags only, for one whose first value flag is optional-attached (`-0i`,
/// `-ia`: everything after the `i` is its replace string), or for a token
/// that is not a short-option bundle.
fn xargs_short_value_flag(t: &str) -> Option<(char, bool)> {
    let body = t.strip_prefix('-')?;
    if body.is_empty() || body.starts_with('-') {
        return None;
    }
    let (at, c) = body
        .char_indices()
        .find(|&(_, c)| XARGS_VALUE_SHORT.contains(c) || XARGS_OPTIONAL_ATTACHED.contains(c))?;
    if XARGS_OPTIONAL_ATTACHED.contains(c) {
        return None;
    }
    Some((c, at + c.len_utf8() == body.len()))
}

/// True when xargs's own options (`opts`, the tokens before its command word)
/// name an argument FILE (`-a FILE`, `-aFILE`, `-0a FILE`, `--arg-file[=]FILE`):
/// the items then come from that file, not from stdin.
pub(super) fn xargs_reads_arg_file(opts: &[&str]) -> bool {
    opts.iter().any(|t| {
        t.starts_with("--arg-file") || xargs_short_value_flag(t).is_some_and(|(c, _)| c == 'a')
    })
}
