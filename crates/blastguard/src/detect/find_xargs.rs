use super::{
    analyze_code_interpreter, command_candidates, compound_views, confined_ask, depth_exhausted,
    detect_bash, git_subcommand_index, is_code_interpreter, is_exec_wrapper, is_shell,
    normalized_command, split_segments_with_separators, wrapped_command_heads, Ctx, Decision,
    SegmentSep, VerdictAcc, MAX_SHELL_DEPTH,
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

/// What one pipeline stage writes to its stdout, as far as the text shows.
enum StageOutput {
    /// Paths (or path prefixes the output stays under) the stage names: the
    /// operands of `echo`/`printf`, the search roots of `find`, the operands of
    /// `ls`.
    Paths(Vec<String>),
    /// Paths of the repository's own files (`git ls-files`, `git diff`): what
    /// `git ls-files -z | xargs -0 rm` deletes is tracked project content.
    InRepo,
    /// The output cannot be told from the text (a file's contents, a network
    /// response, an expansion, an unrecognised program). The string names it.
    Unknown(String),
}

/// Filters that pass a stage's items through unchanged in kind: they only
/// drop, reorder or duplicate lines, so the only paths that can come out are
/// ones that went in.
const PASS_THROUGH_FILTERS: &[&str] = &["sort", "uniq", "head", "tail", "grep", "egrep", "fgrep"];

/// Round 2 (backlog 3c750df2): `xargs` running a remover (`rm`/`unlink`) on the
/// items an EARLIER pipeline stage writes.
///
/// [`analyze_xargs`] judges only xargs's own tokens, so `echo /etc/passwd |
/// xargs rm` was judged as a bare `rm` with no operand and allowed: the path
/// the remover gets was never looked at. This reads the stage that feeds the
/// xargs segment (`line`/`seg_idx` locate it, see `analyze_segment`), walking
/// back through pass-through filters ([`PASS_THROUGH_FILTERS`] reading only
/// their stdin):
///
///   * the producer names a path at or below a system directory (`echo`/
///     `printf` operands, `find` search roots, `ls` operands, judged by
///     [`rm_system_harm_deny`](super::rm::rm_system_harm_deny) exactly as an
///     `rm` operand) — `Deny` (system harm);
///   * the producer's output cannot be known from the text — `Ask` (a subagent
///     gets the hardened `Deny`): the remover's operands are unknown, and
///     blastguard refuses to guess them;
///   * the producer names only non-system literal paths, or lists the
///     repository's own files (`git ls-files`, `git diff`, `git diff-tree`,
///     `git ls-tree`, with no `-C`/`--git-dir`/`--work-tree`) — no verdict
///     here (`Allow`); every other rule keeps its say.
///
/// A segment that is not the consumer of a `|`, xargs reading an argument file
/// (`-a FILE`), and an xargs whose command is not a remover are not judged
/// here. This only ever adds a verdict.
pub(super) fn xargs_pipe_feed(
    tokens: &[&str],
    line: &str,
    seg_idx: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    let Some(remover) = xargs_remover(tokens) else {
        return Decision::Allow;
    };
    let segs = split_segments_with_separators(line);
    let views = compound_views(line);
    let mut i = seg_idx;
    loop {
        // The stage before `i` feeds it only through a single `|`.
        let fed = i > 0
            && segs
                .get(i - 1)
                .is_some_and(|s| s.sep_after == SegmentSep::Pipe);
        if !fed {
            // Nothing in the text feeds this stage (the first stage of the
            // line, or a filter reading the line's own stdin): xargs reads what
            // the caller pipes in, which this rule cannot see either way.
            return Decision::Allow;
        }
        let text = match (views.get(i - 1), segs.get(i - 1)) {
            (Some(v), _) => v.command.clone(),
            (None, Some(s)) => s.text.clone(),
            (None, None) => {
                return xargs_feed_ask(&remover, "a pipeline stage the parser could not line up")
            }
        };
        let words: Vec<&str> = text.split_whitespace().collect();
        let stage = text.trim();
        // The program the stage finally runs, behind any exec wrapper (`sudo
        // find /usr`). A wrapper option that may be either its value or the
        // program (`sudo -u root find …`) gives more than one head; each is
        // judged, a system path behind ANY of them is the Deny, and a mix the
        // text does not settle is the Ask.
        let heads: Vec<usize> = wrapped_command_heads(&words)
            .into_iter()
            .filter(|&h| !is_exec_wrapper(&normalized_command(words[h])))
            .collect();
        if heads.is_empty() {
            return xargs_feed_ask(&remover, &format!("`{stage}` (a stage with no command)"));
        }
        let outputs: Vec<Option<StageOutput>> =
            heads.iter().map(|&h| stage_output(&words, h)).collect();
        if outputs.iter().all(Option::is_none) {
            // A pass-through filter: judge what feeds IT.
            i -= 1;
            continue;
        }
        let mut undetermined: Option<String> = None;
        for out in &outputs {
            match out {
                Some(StageOutput::Paths(paths)) => {
                    match placed_paths_verdict(paths, stage, &remover, ctx) {
                        d @ Decision::Deny(_) => return d,
                        Decision::Ask(why) => {
                            undetermined.get_or_insert(why);
                        }
                        Decision::Allow => {}
                    }
                }
                Some(StageOutput::InRepo) => {}
                Some(StageOutput::Unknown(what)) => {
                    undetermined.get_or_insert_with(|| xargs_feed_reason(&remover, what));
                }
                None => {
                    undetermined.get_or_insert_with(|| {
                        xargs_feed_reason(
                            &remover,
                            &format!("`{stage}` (a wrapped command whose program is ambiguous)"),
                        )
                    });
                }
            }
        }
        return match undetermined {
            Some(why) => Decision::Ask(why),
            None => Decision::Allow,
        };
    }
}

/// The verdict for the literal `paths` a producer stage `stage` names, fed to
/// `xargs <remover>`: `Deny` (system harm) when one lands at or below a system
/// directory ([`rm_system_harm_deny`](super::rm::rm_system_harm_deny)), `Ask`
/// when a relative one cannot be placed, else `Allow`.
///
/// The producer's text is the RAW segment (its relative paths are not
/// rewritten the way an `rm` operand is), so a relative path is placed against
/// the raw base — the directory the earlier `cd`s left (`cd /etc && find . |
/// xargs rm`). With no base to place it against, it cannot be told apart from
/// a system path.
fn placed_paths_verdict(paths: &[String], stage: &str, remover: &str, ctx: &Ctx<'_>) -> Decision {
    let mut placed = Vec::with_capacity(paths.len());
    for p in paths {
        if p.starts_with('/') || p.starts_with('~') {
            placed.push(p.clone());
            continue;
        }
        match ctx.raw_base.as_deref() {
            Some(base) => placed.push(format!("{}/{p}", base.trim_end_matches('/'))),
            None => {
                return xargs_feed_ask(
                    remover,
                    &format!("`{stage}` (a relative path with no known working directory)"),
                )
            }
        }
    }
    let ops: Vec<&str> = placed.iter().map(String::as_str).collect();
    match super::rm::rm_system_harm_deny(ctx, &ops, false) {
        Some(Decision::Deny(reason)) => Decision::deny_system_harm(format!(
            "`{stage}` feeds `xargs {remover}` paths from a system directory, and xargs runs the \
remover on them — {reason}"
        )),
        Some(other) => other,
        None => Decision::Allow,
    }
}

/// The remover (`rm`/`unlink`) an `xargs` among `tokens` runs, when one does
/// and its items come from stdin.
fn xargs_remover(tokens: &[&str]) -> Option<String> {
    for idx in command_candidates(tokens) {
        if normalized_command(tokens[idx]) != "xargs" {
            continue;
        }
        let rest = &tokens[idx + 1..];
        let Some(start) = xargs_command_start(rest) else {
            continue;
        };
        if xargs_reads_arg_file(&rest[..start]) {
            // The items come from the named file, not from the pipe.
            continue;
        }
        let inner = &rest[start..];
        for j in command_candidates(inner) {
            let word = normalized_command(inner[j]);
            if matches!(word.as_str(), "rm" | "unlink") {
                return Some(word);
            }
        }
    }
    None
}

fn xargs_feed_reason(remover: &str, what: &str) -> String {
    format!(
        "xargs runs `{remover}` on the items {what} writes to the pipe — those are the paths \
it deletes, and blastguard cannot tell from the command line what they are, so it refuses to \
guess"
    )
}

fn xargs_feed_ask(remover: &str, what: &str) -> Decision {
    Decision::Ask(xargs_feed_reason(remover, what))
}

/// What the stage `words`, running the program at `head_idx`, writes to
/// stdout. `None` for a pass-through filter (see [`PASS_THROUGH_FILTERS`])
/// reading only its stdin.
fn stage_output(words: &[&str], head_idx: usize) -> Option<StageOutput> {
    let head = normalized_command(words[head_idx]);
    let rest: Vec<&str> = words[head_idx + 1..]
        .iter()
        .copied()
        .filter(|w| !is_stage_redirect(w))
        .collect();
    let unknown = |why: &str| Some(StageOutput::Unknown(format!("`{head}` ({why})")));
    let paths = |ops: &[&str]| match literal_paths(ops) {
        Some(p) => Some(StageOutput::Paths(p)),
        None => unknown("an operand the shell expands"),
    };
    match head.as_str() {
        "echo" => {
            let ops: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|w| !matches!(*w, "-n" | "-e" | "-E" | "-ne" | "-en"))
                .collect();
            paths(&ops)
        }
        "printf" => paths(&rest),
        "find" => match find_search_roots(&rest) {
            Some(roots) => paths(&roots),
            None => unknown("a search root that cannot be read"),
        },
        "ls" => {
            let ops: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|w| !w.starts_with('-'))
                .collect();
            if ops.is_empty() {
                paths(&["."])
            } else {
                paths(&ops)
            }
        }
        "git" => {
            let sub_idx = git_subcommand_index(&rest);
            let relocated = rest[..sub_idx.unwrap_or(rest.len())].iter().any(|w| {
                w.starts_with("-C") || w.starts_with("--git-dir") || w.starts_with("--work-tree")
            });
            match sub_idx.map(|i| rest[i]) {
                Some("ls-files" | "diff" | "diff-tree" | "ls-tree") if !relocated => {
                    Some(StageOutput::InRepo)
                }
                _ => unknown("its output is not a listing of this repository's files"),
            }
        }
        f if PASS_THROUGH_FILTERS.contains(&f) => {
            if filter_reads_only_stdin(f, &rest) {
                None
            } else {
                unknown("it reads a file whose contents are not on the command line")
            }
        }
        _ => unknown("its output is not on the command line"),
    }
}

/// A redirect word in a producer stage (`2>/dev/null`, `>&2`): not an operand.
fn is_stage_redirect(w: &str) -> bool {
    w.contains('>') || w.starts_with('<')
}

/// True when the filter `f` reads only its stdin: every word is a flag or a
/// numeric flag value (`head -n 5`, `tail -n +2`, `sort -u`), plus — for the
/// grep family — exactly one pattern and no flag that names a pattern or a
/// file/directory to read (`-e`, `-f`, `-r`, `-R`, `-d`, `-D`, `--file`, …).
/// `sort -o`/`--output` and `--files0-from` are not pass-through either.
fn filter_reads_only_stdin(f: &str, rest: &[&str]) -> bool {
    let is_grep = matches!(f, "grep" | "egrep" | "fgrep");
    let numeric = |w: &str| {
        let w = w.strip_prefix('+').unwrap_or(w);
        !w.is_empty() && w.chars().all(|c| c.is_ascii_digit())
    };
    let mut positional = 0usize;
    for w in rest {
        if let Some(long) = w.strip_prefix("--") {
            let names_input = if is_grep {
                [
                    "file",
                    "recursive",
                    "dereference-recursive",
                    "regexp",
                    "directories",
                    "devices",
                ]
                .iter()
                .any(|p| long.starts_with(p))
            } else {
                long.starts_with("output") || long.starts_with("files0-from")
            };
            if names_input {
                return false;
            }
            continue;
        }
        if let Some(short) = w.strip_prefix('-') {
            if short.is_empty() || numeric(short) {
                continue; // `-` is stdin; `-5` is `head -5`
            }
            if is_grep
                && short
                    .chars()
                    .any(|c| matches!(c, 'f' | 'r' | 'R' | 'e' | 'd' | 'D'))
            {
                return false;
            }
            if f == "sort" && short.contains('o') {
                return false;
            }
            continue;
        }
        if numeric(w) {
            continue;
        }
        positional += 1;
    }
    positional == usize::from(is_grep)
}

/// The literal paths `words` spell (see [`literal_items`]); `None` when any of
/// them carries an expansion whose value the text does not show.
fn literal_paths(words: &[&str]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for w in words {
        out.append(&mut literal_items(w)?);
    }
    Some(out)
}

/// The items one `echo`/`printf`/operand word puts on the pipe as xargs splits
/// them: quotes removed, the `\n`/`\t`/`\0`/`\r` escapes read as item
/// separators. `None` for a word carrying an expansion (`$X`, `$(…)`,
/// `` `…` ``). Splitting MORE finely than xargs would only adds candidate paths
/// to judge — it can add a refusal, never hide one.
fn literal_items(word: &str) -> Option<Vec<String>> {
    if word.contains('$') || word.contains('`') {
        return None;
    }
    let mut s: String = word.chars().filter(|c| !matches!(c, '\'' | '"')).collect();
    for esc in ["\\n", "\\t", "\\0", "\\r"] {
        s = s.replace(esc, " ");
    }
    let s = s.replace('\\', "");
    Some(s.split_whitespace().map(str::to_string).collect())
}
