# `detect::ast` — supported subset and EFFECT × TARGET judgement

Task S1 `subset-spec` (2026-11-17). Specification only: the frozen types are in
`judge.rs` (`Effect`, `PathClass`, `Finding`, `Judgement`, `PathCtx`, `judge`,
`classify`); nothing here is wired into `detect` yet and no verdict changes in S1.
(Since S3b, `judge` is wired for the B.1 rows only; since S6-2, `classify`
answers the real Table C class but has no caller outside its tests — see the
Stage map.)

## Binding user rulings (2026-11-17)

1. brush-parser with an explicit SUPPORTED SUBSET allowlist of node and expansion
   kinds (Table A). Anything outside → `Undetermined(Cause::Unsupported)` → main
   Ask / subagent Deny. No per-corner-case bash modelling.
2. Judge by EFFECT × TARGET PATH via rule tables (Tables B, C), not by a
   command-name denylist. Unresolvable target → restrictive. Unknown program (no
   effect model) → Allow. Undetermined command word → Undetermined.
3. System-harm targets are always Deny.
4. perl / ruby / node inline programs (`-e` etc.) are OUT of the subset → Ask.
   Python gets a real parser later (S5).
5. `rm -r` (and other deletes) whose target statically resolves inside the session
   scratchpad or own worktree → Allow; unresolved or outside → unchanged (Ask/Deny).
6. AST analysis Undetermined → Ask; the old string harm detection is kept ONLY
   raise-only (may lift Ask → Deny, never lower to Allow).
7. Migrated detectors drop the old string-reading fallback (S6).

Follow-up rulings (2026-11-17, on the S1 open questions):

8. `Scratch` = every temp and cache deletion root: `/tmp`, `/private/tmp`,
   `$TMPDIR` and the cache roots `rm -r` already allows
   (`SafeRoots::classify_disposable`). Not `classify_temp` alone.
9. `OwnWorktree` = only the git checkout that contains the session cwd. Other
   worktrees keep today's verdict. **Correction (user ruling 2026-11-18):**
   today's verdict there is NOT a confined Ask for a recursive `rm`: a
   recursive `rm` whose operands resolve strictly inside a worktree storage
   root stays **Allow** even when the session cwd is outside that checkout
   (the `scope.rs` / `rm.rs` worktree-storage Allow, user ruling 2026-09-30,
   backlog 873651b9, wins). The earlier parenthetical "(confined Ask)" was
   wrong for that row. **Narrowing (user ruling 2026-11-20, backlog 606a93ab):**
   git working-tree discards (`reset --hard`, `checkout .` / `checkout --
   <path>` / `checkout --force`, `restore <path>` / `restore .`, `clean -f` /
   `clean -fd`, `switch --force`, `stash -u`/`-a`) in the `git -C <wt>` and
   `cd <wt> && git …` forms aimed at a linked worktree that does NOT contain
   the session cwd (no session cwd counts as "not containing") are Allow only
   when git positively reports that checkout clean; with uncommitted changes,
   or when cleanliness cannot be observed, they are Ask (subagent Deny). The
   own checkout keeps its Allow. Implemented in `detect/git.rs`
   (`managed_worktree_git`, `managed_discard`, `linked_checkout_is_clean`).
   Cleanliness is judged for the WHOLE checkout, not per pathspec.
10. A program whose text is not in the command (`bash script.sh`) is
    Undetermined → Ask (subagent Deny): an interpreter running a file is an
    exception to "unknown program → Allow". When the same command writes that
    file, the written body is judged recursively instead (S4) — only when
    the write and the run are joined by `&&` (ruling 20).
11. The remaining open questions stay legacy (today's behaviour) and are
    revisited at S6; see "Open questions".

S2 rulings (user ruling 2026-11-17, spec change made before the S2 tests):

12. `$HOME` / `${HOME}` and `~` are Known values: the `HOME` of the hook's own
    environment (Table A.3).
13. `.` / `source` of a file stay **Ask** (main) / Deny (subagent) until a
    quarantine plugin exists (Table B.5); S4 does not judge the sourced body.

S2 supplement rulings (user ruling 2026-11-17, after the S2 RED verification):

14. The legacy string layer stays in full, **raise-only**, until S6 — for
    every row, not only the harm readings. A Table C Allow that a legacy
    reading answers Ask / Deny today (`dd of=`, `truncate`, `find -delete` on
    Scratch) stays at the legacy verdict until S6 retires that reading.
    **ruling 14 replaced per rule (2026-11-18): rm legacy reading retired
    when new layer is Known** (user ruling 2026-11-18: retire the legacy
    layer rule by rule). For `rm`, when every target of the command is
    statically resolved and lands in a Table C Allow class (Scratch /
    OwnWorktree), the legacy `rm` reading (`detect/rm.rs` `analyze_rm`) no
    longer raises it: `detect/rm/new_layer.rs`. Scratch included by user
    ruling 2026-11-18 ("仕様どおり Allow": a quoted in-scratch literal gets
    its unquoted spelling's verdict). Every other `rm` (an unmodelled
    option, an Undetermined value, a `legacy` / Unresolved / System /
    Protected / Workflow class) keeps the legacy reading as before. Other
    legacy readings of the same command line still apply.
15. Python keeps the legacy readings until S5 (the Python parser).
    **Narrowed by the S4 rulings (2026-11-18, ruling 18):** a python3 program
    FILE run (`python3 x.py`) and `python3 -m mod` are Undetermined from S4
    on (B.5 beats this ruling); `-c` and a literal stdin program stay legacy.
16. `.` / `source` become Ask (main) / Deny (subagent) **together with the
    quarantine plugin**, not in S4. Until then their verdict is legacy
    (today's). This refines ruling 13's timing.
17. Direct execution of a program file (`./run.sh`, `scripts/foo.sh`, an
    absolute path) whose body is not in the command text is Undetermined →
    Ask (main) / Deny (subagent), like ruling 10's `bash script.sh` (S4).

S4 rulings (user ruling 2026-11-18):

18. S4 lands as a standalone B.5 layer (`b5.rs`), not waiting for the S3
    judge; the final verdict is `max(B.5, legacy)` — the legacy layer only
    raises, a legacy Deny / Ask is never lowered. The B.5 Undetermined
    shapes (interpreter program file, also behind `env` / `env -i` /
    `nohup` / `timeout`; perl / ruby / node `-e`; a non-literal stdin
    program; `bash < x.sh`; `eval`; direct exec of a project path; and, per
    the Table B common rules, an undetermined command word) are Ask
    from the layer itself — distinct from a whole-line parse failure, which
    gives no B.5 opinion (legacy decides). A python3 program file run and
    `python3 -m` are Undetermined (B.5 beats ruling 15) until S5.
19. A binary under a system / PATH dir (`/bin`, `/usr/bin`, `/usr/local/bin`,
    `/opt/homebrew/bin`, …; `b5.rs` `SYSTEM_BIN_DIRS`) is judged by basename
    like the bare name; bare names (`make`, `cargo`, unknown tools) get no
    B.5 opinion.
20. A body written in the same command and then run is judged as the program
    ONLY when the write and the run are joined by `&&` (a benign body is
    Allow, a harmful one Deny); `;`, a newline, or `false && write; run` →
    Undetermined. A `.py` written then run stays Undetermined (S5). The
    reason names the program file, never a data operand; a data operand
    gets the program's verdict. A body run by `sh` gets the `sh -c` reading.
21. Literal process-substitution or pipe content (all `echo` / `printf`
    literal producers) is read like a literal stdin body: a shell body is
    judged recursively; a perl / ruby / node / python body gives no B.5
    opinion (legacy decides). A bare shell reading the inherited stdin gives
    no B.5 opinion (the Bash tool's stdin is empty) — at the top level only:
    inside a `-c PROG` body, "inherited" is the enclosing shell's stdin
    (`bash -c 'bash' < x.sh` reads `x.sh`, backlog b838630f), judged as if
    the shell read it directly (`File` / `Fd` / `FromCommand` /
    `Undetermined` → Undetermined; a `Literal` → judged by the full detector
    as `<shell> -c '<body>'`; `b5.rs::inherited_stdin`).
    Backlog 40dc52c0 (user ruling 2026-11-20, option A): content a
    determined producer PIPELINE delivers is read the same way — one
    pipeline whose first stage is `echo` with determined words (no option,
    no backslash) or `seq` with 1–3 unsigned decimal literals, and whose
    later stages are `cat` / `sort` with no operand and no option (`sort`
    only over lines that are all digits or all equal, the orders every
    locale agrees on); a double-quoted `$( )` whose program is itself such a
    pipeline is its output with trailing newlines removed
    (`bash <(echo hi | cat)`, `bash <(seq 10 | sort)`,
    `bash <(echo "$(echo hi)")`, `echo hi | cat | bash`;
    `payload/determined_output.rs`, `b5.rs::determined_delivered_program`).
    Only when the user's top-level line, every derived text on the way, AND
    the text being judged define no function, make no assignment and run
    only `echo` / `seq` / `cat` / `sort` and shells / interpreters
    (`resolution_fixed`); the chain value starts from the user's line and is
    only ever narrowed by each derived text (`readings.rs::chain_names_fixed`,
    `Ctx::names_fixed` narrowed in `detect_bash`), never widened, so
    `cat(){ …; }; eval 'echo hi | cat | bash'` and
    `bash -c 'PATH=/tmp/x:$PATH; bash -c "echo hi | cat | bash"'` keep their
    Ask. Any other
    producer or filter, option, file operand or undetermined word (an
    unquoted `$( )`, a backquote, `$N`) keeps the Undetermined.
22. (T1) A program file that is git-tracked AND unmodified gets no B.5
    opinion (legacy decides), for interpreter file runs and direct exec.
    Unmodified = clean in the work tree and index (`git ls-files
    --error-unmatch` + `git status --porcelain`, from the hook's cwd, exit
    status checked) AND not written by any other part of the same command
    (any unknown command counts as a possible writer). `-m`, an untracked /
    modified / staged / absent file, a path outside the repo (a symlink
    leaving it included), a `-c` body, an injecting env var on the line
    (`PATH`, `PYTHON*`, `LD_*`, …) and anything git cannot answer stay
    Undetermined.
23. (T2) A program read from a file descriptor (`bash <&'$fd'`) is
    Undetermined.
24. (T3) `bash -n` / `sh -n` (noexec; also with `x` / `v` / `e` / `u`)
    gets no B.5 opinion. `-ni` (bash ignores `-n` when interactive) and
    `x.sh -n` (a script argument) stay Undetermined; `-n -c BODY` judges
    BODY as if it ran.

Vocabulary: **Undetermined** = `Judgement::Undetermined(cause)`, resolved to
Ask (main) / Deny (subagent, `interactive::ask_available`). **Unresolved** =
`PathClass::Unresolved`, one target that could not be placed (Table C row).
**legacy** in a cell = S3 must reproduce today's verdict of the existing rule
named there, pinned by its existing tests; the ruling does not change it.

## Table A — supported node / expansion kinds

Everything not listed as supported is `Unsupported`. "Value Undetermined" means the
script is still `Known`: the word's `Value` is `Undetermined` and feeds Table C as
`Unresolved` (or, for a command word, makes the judgement Undetermined).

### A.1 Parse stage (`parse.rs`)

| Input | Result |
|---|---|
| > `MAX_PARSE_BYTES` (8 KiB) | `TooLong` → Deny (ruling 2026-11-16) |
| nesting > `MAX_PARSE_DEPTH` (16), over-counting pre-scan | `TooDeep` → Deny (ruling 2026-11-16) |
| zsh-only syntax (`${(flags)…}`, `=(cmd)`, …) | `ZshOnly` → Undetermined |
| brush-parser error / unterminated heredoc | `Parse` → Undetermined |
| panic in brush-parser or the evaluator | `Panic` → Undetermined |

### A.2 Commands and lists (`eval.rs`)

| Node (`brush_parser::ast`) | Status |
|---|---|
| `Program`, `CompoundList`, separators `;` and `&` (`Context::Background`) | supported |
| `AndOr` `&&` / `||` (both arms emitted, states joined) | supported |
| `Pipeline` incl. `!` (stages get `Context::Pipeline`, stdin piped) | supported |
| `Command::Simple` (prefix assignments, argv, redirects) | supported |
| `CompoundCommand::BraceGroup`, `Subshell` | supported |
| `IfClause`, `CaseClause`, `WhileClause`, `UntilClause` (fixpoint) | supported |
| `ForClause` (literal list ≤ 256 unrolled; else body once, var Undetermined) | supported |
| `Arithmetic` `(( ))`, `ArithmeticForClause`, `let` | supported only when every name read is bound to a literal on the line (`arith.rs`); else `Unsupported` |
| `Command::ExtendedTest` `[[ ]]` | supported (operands evaluated; arithmetic operators as above) |
| `Command::Function` (definition) | **Unsupported** under `analyze` (every existing consumer, unchanged). Under `analyze_with_function_definitions` (`eval/function.rs`, task b1-ast-function-definition-flag) it is **parsed**: the name must be a literal and the body is evaluated once, late-bound (every variable, cwd and stdin Undetermined), its commands are emitted for judgment, the name is recorded in `Script::function_names`, and the state is opaque after the definition. A redirection on the body or a non-literal name is Unsupported, and so is any unsupported construct in the body. Two points are fail-closed by design: a literal assignment in the body may come out Unsupported, and `HOME` in the body is Undetermined. The flagged `Known` script is for `heredoc_data` only, through `analyze_with_function_definitions` (the one caller; it keeps every quoted heredoc inside a function body, located by `Script::function_body_ranges`, the index ranges of the body commands). Every other consumer still treats a function-definition line as Unsupported. Opening another consumer needs its own RED |
| `CompoundCommand::Coprocess` | **Unsupported** |
| work budget (`MAX_WORK` 100 000) / `MAX_COMMANDS` (10 000) exceeded | **Unsupported** |

### A.3 Word pieces (`eval.rs::pieces`, `eval/words.rs`)

| Piece | Status |
|---|---|
| `Text`, `SingleQuotedText`, `DoubleQuotedSequence`, `EscapeSequence` | supported (literal) |
| `AnsiCQuotedText` `$'…'` | supported if decoded; else value Undetermined |
| `GettextDoubleQuotedSequence` `$"…"` | value Undetermined |
| `TildeExpansion` `~` (also after `=` / `:`) | value Known: the hook environment's `HOME` (user ruling 2026-11-17, ruling 12); `~user` is not covered by the ruling → value Undetermined. **Implemented (S3) only for a bare `~` that starts the word**; `~` after `=` / `:` inside a word is still Undetermined (restrictive; not yet implemented) |
| `$HOME` / `${HOME}` | value Known: the hook environment's `HOME` (user ruling 2026-11-17, ruling 12), unless `HOME` is assigned on the line (then the named rule below) |
| `$x` / `${x}` named | literal iff bound to a literal on the line; else value Undetermined |
| positional / special parameters (`$1`, `$@`, `$?`, …) | value Undetermined |
| other parameter operations (`${x:-v}`, `${#x}`, `${x/a/b}`, …) | value Undetermined; **Unsupported** if it holds `$( )` / backquote or reads an undetermined name in arithmetic |
| `CommandSubstitution` `$( )`, backquotes | body evaluated as a nested script (`Context::CommandSubstitution`); value Undetermined |
| `ArithmeticExpression` `$(( ))` | value Undetermined; **Unsupported** under the A.2 arithmetic rule |
| unquoted expansion subject to splitting / globbing, unquoted glob / brace | value Undetermined. **Literal-dir glob (user ruling 2026-11-19)**: a Table B target word that is an unquoted glob whose prefix up to the last `/` is a literal directory (no `..` component, no expansion / quote / escape / brace / `~`) and whose glob metacharacters are only in the final component (not starting with `.`) is PLACED by that parent directory's Table C class (`rm -rf /tmp/zqxv*` → `/tmp` → Scratch; `chown yuki src/*.rs` → `src` → Project). A bare glob's parent is the cwd; a relative prefix resolves against the command's cwd like any relative target. A glob in a non-final component (`a/*/b`), a `..` (`../*`), an expansion in the prefix (`$D/*`, `~/*`) or a parent that cannot be resolved stays Undetermined → Unresolved → Ask (`judge/table_b.rs::literal_dir_glob`). The word value itself stays Undetermined: the Table B "undetermined word that may be an option" rule still applies to a glob that may start with `-` (`rm *.o`) |
| process substitution `<( )` / `>( )` | body evaluated (`Context::ProcessSubstitution`) |

### A.4 Redirections

| Redirect | Status |
|---|---|
| `<` `>` `>>` `>|` `<>` `<&` `>&` (`RedirOp::*`), `&>` `&>>` | supported; target `RedirTarget::Path` / `Fd` |
| heredoc `<<` / `<<-` (quoted, or unquoted without expansions → literal body) | supported; body Undetermined if unquoted with expansions |
| here-string `<<<` | supported |
| negative fd | **Unsupported** |

### A.5 Builtins with state effects (evaluator, not Table B)

`cd` (literal, `&&`-tied) moves the tracked cwd; `cd -` / bare / options / `pushd`
/ `popd` → cwd Undetermined. **User ruling 2026-11-19:** (1) the evaluator holds
the hook's cwd (the payload `cwd`, `SafeRoots::session_cwd`, set per detection
by `eval/cwd.rs::with_hook_dir` in `readings.rs::detect_bash_every_reading`) as a
known value, so a relative `cd` from it (`cd .githooks`) is resolved
(`Cwd::Known`); with no payload cwd it stays Undetermined. (2) A relative path
with a `..` component — a `cd` operand, or a relative target / redirect after a
known `cd` — is resolved on the REAL path (`eval/cwd.rs::real_join`): the cwd
must be its own canonical path, every component up to the last `..` must be an
existing directory that is not a symlink, `..` is its real parent, and the
components after the last `..` are joined lexically. A missing target, a walk
through a symlink (`cd link && rm ../x`), or a failed canonicalize → Undetermined
(Ask / Deny), as before. `eval`, `source`, `.`, `trap`, `alias`,
`shopt`, `builtin`, `command`, an undetermined command word, `declare`-family
options, non-plain `set` make the state opaque (later values Undetermined).
**Exception (user ruling 2026-11-18, r21):** `. F` whose value is exactly
`<hook HOME>/.cargo/env` (the literal `. "$HOME/.cargo/env"`, `HOME` not
reassigned; unwrapped, no prefix assignment) is rustup's PATH setup only and
does NOT make the state opaque (`eval.rs::builtin_effects`, `is_cargo_env`).
Every other `.` / `source` — `source "$HOME/.cargo/env"` included — keeps the
state opaque (reading A unchanged).
For **judgement**, `eval` is Undetermined (it runs code not in the script) until
S4 models it; `source` / `.` become Ask (main) / Deny (subagent) together with
the quarantine plugin and are legacy until then (rulings 13, 16; B.5).

## Table B — effect × target extraction

Common rules for every row:

* Option parsing is per command; an option not in the row's list, or an
  Undetermined option word, → `Judgement::Undetermined(Unsupported("<cmd> option
  <o> not modelled"))`. `--` ends options.
* A target word with an Undetermined value → `PathClass::Unresolved`.
* A relative target: resolved against `SimpleCommand::cwd` exactly as
  `RedirTarget::Path` / `eval::resolve_at` (literal as written when no `cd`
  applies → placed against `PathCtx::hook_cwd`; joined after a known `cd`,
  a `..` component on the real path per user ruling 2026-11-19 (A.5);
  Undetermined after an undetermined `cd`).
* A relative target after a `cd` that may have failed (`cd x; cmd rel`,
  `cd x || true; cmd rel`) — user ruling 2026-11-18: when every path that
  reaches the command has a known cwd (the `cd` target, the hook's cwd, a
  previous literal `cd`) and there are at most 4 of them
  (`MAX_CWD_CANDIDATES` in `eval/cwd.rs`; `Cwd::OneOf`,
  `SimpleCommand::cwd_candidates`),
  the command is judged once per candidate cwd and the most restrictive
  verdict stands; it is Allow only when every candidate is Allow. While
  `classify` is not wired into `judge` (S6-2 implemented it, unwired),
  "judged at a candidate" means: the whole detector
  (legacy layer + this judge) on the synthetic text `<argv>` (the hook's cwd)
  or `cd <dir> && <argv>` (a known directory), so the existing predicates
  place the operand exactly as for a literal spelling there
  (`judge::judge_candidates`; an Ask / Deny there is a
  `PathClass::AtCandidate` finding, a system-harm Deny stays system harm).
  More than 4 candidates, a candidate that is not known (`cd "$X"`, `cd -`,
  a relative `cd` from an unknown hook cwd, …), an argv word that is not a
  spellable literal, an unknown hook cwd, or more than 32 candidate runs in
  one script → `Unresolved` (Ask / Deny), never Allow. `cd x && cmd rel`
  reaches `cmd` only through the successful `cd`, so its cwd is `x` (one
  candidate, unchanged). Every reader other than the judge (`targets.rs`,
  `RedirTarget::Path`, `ProgramSource::File`) still sees such a cwd as
  `Undetermined`.
* A command not in this table (and not a wrapper / interpreter) has no effect
  model → no finding (Allow); its redirects are still judged (B.2). An
  interpreter running a program file is NOT such a command (ruling 10, B.5).
* Undetermined command word (`argv[0]`) → `Judgement::Undetermined`.

### B.1 File commands

| Command | Effect | Targets | Notes |
|---|---|---|---|
| `rm` | `Delete{recursive: -r/-R/--recursive}` | every operand | `-f -i -I -v -d` modelled (`-v`: man rm "Be verbose when deleting files" — output only) |
| `rmdir`, `unlink` | `Delete{recursive:false}` | every operand | `rmdir -p`: every ancestor component too |
| `mv` | sources `Delete{recursive:true}`; destination `Truncate` | sources = all operands but the last (all with `-t DIR`); dest = last / `-t DIR` | `-f -i -n -v -T -t` (`-v`: man mv "showing files after they are moved" — output only) |
| `cp` | destination `Truncate`; with `-r/-R/-a` also `CreateTree` | last operand / `-t DIR` | sources are reads (no finding). Effect-neutral (user ruling 2026-11-18; man cp): `-f` "remove it and create a new file" — same destination; `-i` prompt before overwriting — same or none; `-n` "Do not overwrite" — same or none; `-p` preserve times/mode/owner — metadata of the same destination; `-v` verbose — output only. GNU long: `--force --interactive --no-clobber --verbose` (`--preserve[=…]` not listed) |
| `rsync --delete` / `--del` / `--delete-*` (user ruling 2026-11-18) | `Delete{recursive:true}` | the local destination (last operand; remote `host:path` / `rsync://` and one-operand list mode: no target) | judged like an `rm -rf` operand: Scratch / OwnWorktree → Allow (`rm/new_layer.rs::rsync_dest_allows`), else the `rm -r -f <dest>` verdict (`rm.rs::analyze_rsync_delete`); rsync options are not parsed (no Undetermined for unlisted ones), the system / home-asset Deny of `system_paths.rs` stands |
| `install` | destination `Truncate` | last operand / `-t DIR`; `-d`: every operand `CreateTree` | `-m -o -g` take an argument |
| `ln` | `Link` | link name = last operand / `-t DIR` (one operand: basename in cwd) | link target is not a target; `-s -f -n -v -T -t` (`-v`: man ln "showing files as they are processed" — output only) |
| `chmod`, `chown`, `chgrp` | `Metadata` (recursive with `-R`) | operands after the mode / owner spec | `--reference=F` reads F; `-v` / `--verbose` (user ruling 2026-11-18; man chmod / chown "Cause … to be verbose, showing filenames" — output only) |
| `sed -i` / `--in-place` | `Rewrite` | input files (script operand dropped unless `-e`/`-f` given) | BSD `-i ''` / `-i.bak` suffix; a script containing `w`/`W`/`e` or `s///w`/`s///e`: legacy until S6 (Open question 2). Effect-neutral (user ruling 2026-11-18): `-E` / `-r` / `--regexp-extended` (man sed: extended regular expressions — regex syntax only), `-n` / `--quiet` / `--silent` (suppresses the echo to stdout — output only), `-s` / `--separate` (GNU sed manual: files as separate streams — addressing only, same files; not in BSD sed). Note: BSD `sed -i -E …` reads `-E` as the backup suffix (a `<file>-E` beside the same file) — same directory, not modelled as a separate target |
| `perl -i` | — | — | **out of subset**: perl inline program → Undetermined (ruling 4) |
| `tee` | `Truncate` (`Append` with `-a`) | every operand | `-a -i -p` |
| `truncate` | `Truncate` | every operand | `-s` takes an argument; `-r RFILE` reads |
| `dd` | `of=F`: `Truncate` (`Rewrite` with `conv=notrunc`) | `F` | `if=` is a read; any other operand form not `k=v` → Unsupported |
| `find` | `-delete`: `Delete{recursive:true}` on each search root (descendants); `-fprint* F` / `-fls F`: `Truncate F` | search roots = words before the first expression word (`-H -L -P -D x -O*` skipped) | `-exec/-execdir/-ok/-okdir CMD … {} ;|+`: legacy (`find_xargs.rs`) until S6 (Open question 1) |
| `xargs` | the wrapped command's effect | wrapped command's own literal operands + the stdin-supplied operands → `Unresolved` unless stdin is `StdinSource::Literal` | `-0 -n N -I R -L N -P N -d D -r -t` |

### B.2 Redirections (the shell's effect, judged for every command)

| Redirect | Effect | Target |
|---|---|---|
| `>`, `>|`, `&>`, `N>` | `Truncate` | `RedirTarget::Path` value |
| `>>`, `&>>`, `N>>` | `Append` | `RedirTarget::Path` value |
| `<>` | `Rewrite` | `RedirTarget::Path` value |
| `<`, `<<`, `<<<`, `<&`, `>&N`, process substitution | no effect | — (`/dev/null` etc. are `Other`) |
| `>&word` with a non-fd word | `Truncate` | the word, as a path |
| compound-command redirects (`{ …; } > f`, `done > f`) | as above | the extra empty-argv `SimpleCommand` |

**Relink layer consumer (backlog dc053fcf, 2026-11-19).** The string-level relink
layer (`detect/relink.rs`) reads the same values through [`targets::resolve`]: a
relinking command (`mv`, `cp`, `ln`, …) whose destination or source word is not a
literal but has values the evaluator proved (a `for` variable over a literal list,
unrolled as above) is judged once per value, and the strictest answer stands. A
word with no proven values (`$(ls)`, a glob, an undetermined variable) keeps the
`Ask`. Values are combined per word, not per iteration, so a combination of two
values of different words may be judged although no single iteration runs it; that
only adds judgements, it never removes one. The **source of a symlink**
(`ln -s`, `cp -s`) is never replaced by a proven value: the evaluator places a
value against the command's cwd (`resolve_at`), but a symlink holds its source
text and resolves it from the directory holding the link, so `ln -s $f /private/L`
with `f=etc` would be read as `<cwd>/etc` instead of `/private/etc`. Such a source
stays spelled and the write through the link stays an `Ask` (backlog dc053fcf
retry, 2026-11-19). An `ln`/`cp` with a long option the relink layer does not
know by its full name — an abbreviation GNU getopt_long accepts (`ln --sym`,
`cp --sym`) or any other unmodelled `--` option — is not interpreted: it is
opaque (never read as a hard link or a plain copy), its sources are not
replaced, and a write below its destination is an `Ask`.

### B.3 Wrappers (unwrap, then judge the wrapped command by its own row)

| Wrapper | Skipped before the wrapped command | Notes |
|---|---|---|
| `env` | options `-i -u NAME -0 -C DIR`, then `NAME=V` words | `-S` (split string) → Unsupported; `-C DIR` sets the wrapped cwd |
| `sudo`, `doas` | options; `-u/-g/-C/-D/-h/-p/-r/-t/-U USER` take an argument | `sudo -s/-i` with no command = an implicit shell, judged like a bare shell (B.5 `b5.rs`, user ruling 2026-11-20 Q3); a literal stdin program (here-string, here-document) of a shell behind the wrapper is also judged by the full detector as `<shell> -c '<body>'`; `-l/--list`, `-v/--validate`, `-K/--remove-timestamp`, `-V/--version` alone run nothing, with a command word after them → Unsupported; `sudo -e` (sudoedit) → Unsupported |
| `nice` (`-n N`), `ionice` (`-c -n -t`), `nohup`, `time` (`-p`), `stdbuf` (`-i -o -e`), `setsid`, `caffeinate` (`-d -i -m -s -u -t N -w PID`), `chrt`, `taskset` (mask/list arg), `timeout` (`DURATION`, `-s SIG`, `-k D`, `--foreground`, `--preserve-status`), `unbuffer`, `arch` (`-arch A`, `-x86_64`, `-arm64`), `flock` (lock file, `-c CMD` → Unsupported) | as listed | the flock lock file is created (`Truncate`-free create: no finding) |
| `command`, `builtin` | as `eval::unwrap_wrappers` (`-p`, `--`); `command -v/-V` = lookup, no effect | |
| `exec` | `-a NAME -c -l` | `exec` with redirects only: B.2 applies to the shell |
| `su` | — | `su -c` / any `su` → Unsupported |
| `xargs` | see B.1 | |

### B.4 git (path effects only)

`git -C DIR` / `--work-tree=DIR` set the base; `--git-dir`, `-c k=v` are skipped;
an Undetermined value → `Unresolved`. Target "work tree" = the base (`-C` DIR,
else the command's cwd).

| Subcommand | Effect | Target |
|---|---|---|
| `reset --hard` (also `--merge`, `--keep`) | `GitDiscard` | the work tree |
| `clean` (`-f`; `-d` → recursive; `-x/-X` ignored files too) | `Delete{recursive: -d}` | each pathspec, else the work tree (search-root semantics: `IsRoot` acceptable) |
| `checkout -- <paths>` / `checkout .` / `checkout <tree-ish> -- <paths>` | `GitDiscard` | each pathspec |
| `restore [--worktree] <paths>` (default worktree); `--staged` only | `GitDiscard`; staged-only: no effect | each pathspec |
| `add -A` / `--all` / `add .` / `add -u` | `GitStage` | each pathspec, else the work tree |
| other subcommands | no path effect here | non-path git rules (push, branch -D, hooksPath, …) stay in `detect/git.rs` (legacy, outside this table) |

### B.5 Interpreters (`program_source`)

| Command | Program source | Judgement |
|---|---|---|
| `bash` / `sh` / `zsh` / `dash` / `ksh` `-c PROG` | `InlineLiteral` | PROG analysed and judged recursively (S4); Undetermined PROG → Undetermined |
| same, stdin (`bash`, `-s`, `-`, `/dev/stdin`) | `Stdin(StdinSource)` | `Literal` body — and the literal output of a process substitution / previous pipe stage / a brace group `{ echo a; printf b; } |` whose members are all literal `echo` / `printf` joined by `;`, a newline or `&&` (their outputs concatenated; any other member, `||`, `&`, a redirect, a nested pipe or group stays `FromCommand`; `ast/support.rs::brace_group_stdin`), or the computed output of a determined producer pipeline (`echo` / `seq` into `cat` / `sort`, a double-quoted `$( )` of one; ruling 21, backlog 40dc52c0, `b5.rs::determined_delivered_program`) — judged recursively (S4, ruling 21); a literal piped in by a stage that is not one simple command right before the `|` (a brace group, wherever the pipeline sits: `( … )`, `if`, a loop body) is ALSO judged by the full detector as `<shell> -c '<body>'`, because the legacy layer that judges a literal body's effects may not read that group there (perl / ruby / node: Undetermined; python: S5; `b5.rs::piped_by_compound_stage`); `FromCommand` / `File` / `Fd` / `Undetermined` → Undetermined; `Inherited` → no B.5 opinion at the top level (ruling 21), inside a `-c PROG` body the enclosing shell's stdin, judged as above (`b5.rs::inherited_stdin`, backlog b838630f) |
| same, script file (`bash x.sh`); likewise any interpreter running a program file (`python3 x.py`, `perl x.pl`, …) | `File(Value)` | **Undetermined** → Ask / Deny (ruling 10). Exception (shells and direct exec only): when an earlier command of the same script writes that file (matched by resolved path) with a determined body (`cat > x.sh <<'EOF' … EOF`) and the write and the run are joined by `&&` only, that body is judged as the program (S4, ruling 20). A git-tracked, unmodified file → no B.5 opinion (ruling 22). `-n` noexec → no B.5 opinion (ruling 24) |
| `eval` | `Undetermined` | Undetermined (S4) |
| direct execution of a program file (`./run.sh`, `scripts/foo.sh`, `/abs/run.sh`) | `File(Value)` | **Undetermined** → Ask / Deny (ruling 17, S4), same exceptions as the script-file row (ruling 20, ruling 22) |
| `source`, `.` (reading a file) | `File(Value)` | **Ask** (main) / Deny (subagent) together with the quarantine plugin; legacy until then (rulings 13, 16); not judged recursively in S4 |
| `python*` program file / `-m` | `File` / `Undetermined` | **Undetermined** from S4 (ruling 18, beats ruling 15); a git-tracked, unmodified file → no B.5 opinion (ruling 22; never `-m`); S5 does NOT read program files (they stay here) |
| `python*` `-c` / literal stdin (here-document, here-string, literal pipe) | as `program_source` | **S5**: parsed (crate `progeffect`), each effect judged on its shell spelling — see "Python programs (S5)"; parse failure / dynamic dispatch → Undetermined; a non-literal stdin → Undetermined (S4) |
| `perl` / `ruby` `-e`/`-E`, `-i`, `node` `-e`/`-p`, non-literal stdin | — | **out of subset** → Undetermined (ruling 4, S4); a literal stdin / process-substitution / pipe body → no B.5 opinion, legacy decides (ruling 21) |
| wrapper whose operand is an interpreter or Undetermined | `Undetermined` | unwrap (B.3), then the interpreter's row |

## Table C — PathClass × Effect → verdict

`PathClass` is computed only from existing predicates (named on each variant in
`judge.rs`); first matching class wins in this row order. Main column / subagent
column. The subagent column is always the main column with Ask → Deny
(`interactive::ask_available`); no cell differs otherwise.

| PathClass (predicate) | Delete (rec / non-rec) | Truncate / Append / Rewrite | Metadata | Link | CreateTree | GitDiscard | GitStage |
|---|---|---|---|---|---|---|---|
| **System** (`harm_mark::harms_system` + `protected::real_system_path_block`; escapes via `Ctx::confined_root`) | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† |
| **Protected** (`exclude::is_protected_path`, `protected::physical_landing`; lifted inside `classify_worktree_checkout` Inside) | Deny / Deny (`protected_disarm_deny`, `protected_tree_deny`) | Ask / Deny (`protected_path_block`) | Ask / Deny | Ask / Deny | Ask / Deny | legacy | legacy |
| **Workflow** (`exclude::is_workflow_path`) | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny |
| **Persistence** (`launch_agents_new_file_ask`, new file only) | — | Ask / Deny | — | Ask / Deny | Ask / Deny | — | — |
| **Scratch** (`SafeRoots::classify_disposable` Inside: `/tmp`, `/private/tmp`, `$TMPDIR`, cache roots — ruling 8) | Allow / Allow (ruling 5) | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow |
| **OwnWorktree** (the checkout containing the session cwd — ruling 9; see notes) | Allow / Allow (ruling 5) | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | legacy (`managed_worktree_git` → `ManagedWt::Own`: Allow) | Allow / Allow |
| **OtherWorktree** (`SafeRoots::classify_worktree` Inside, not OwnWorktree) | legacy (ruling 9 as corrected 2026-11-18: today's verdict — recursive `rm` strictly inside a worktree storage root is Allow); `git clean -f` (any `-d`/`-x`): see the GitDiscard cell | legacy | legacy | legacy | legacy | legacy, narrowed by user ruling 2026-11-20 (backlog 606a93ab; `managed_worktree_git` → `ManagedWt::Other`, `managed_discard`): Allow only when git positively reports the checkout clean (status + `ls-files -v`; `--ignored` too for `clean -x`/`-X`), else **Ask / Deny** — uncommitted changes, or the probe fails / times out | legacy |
| **Project** (`SafeRoots::classify` Inside / IsRoot) | legacy (`deletion::judge_rm` recoverability; non-rec: legacy rm arm) | legacy | legacy (`chmod -R` confined Ask) | legacy | legacy (scratch-place: managed → unchanged) | legacy (`reversible`) | legacy (`git_stage`) |
| **Other** (resolved, `Placement::Outside`) | legacy (ruling 5: unchanged Ask/Deny) | legacy | legacy | legacy | Ask / Deny (scratch-place rule) | legacy | legacy |
| **Unresolved** | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny |

† `Decision::deny_system_harm` (never downgraded on repeat). Ruling 3.

Notes:

* OwnWorktree is composed from existing predicates only: the target is
  `SafeRoots::classify_worktree_checkout` `Inside { root: R }` and the session
  cwd (`SafeRoots::session_cwd`) is `R` or inside `R` by the same predicate.
  Any `Undetermined` → not OwnWorktree.
* `IsRoot` of a Scratch / worktree storage root (`rm -rf /tmp`) is NOT Scratch /
  OwnWorktree; it falls through to Project / Other (today: `judge_rm` refuses the
  root of a temp / worktree area → Deny).
* A finding set is folded most-restrictive-first: any Deny → `Judgement::Deny`
  (system_harm if any System Deny); else any Ask → `Judgement::Ask`; else Allow.
* An Unresolved Delete in Table C is Ask/Deny, but ruling 5 says unresolved
  deletes stay "unchanged". Where today's rule answers Deny (e.g. recursive `rm`
  of an unplaceable operand), the raise-only legacy layer keeps that Deny.

## Deny-target table (Q3/Q4, backlog 7275b9d9)

User rulings 2026-11-14 (Q3, Q4), design approved 2026-11-19. ONE table
(`ast/judge/deny_targets.rs::TABLE`), wired as its own raise-only layer
(`ast/mod.rs::deny_target_verdict`, folded in
`readings.rs::detect_bash_every_reading` after `table_b_verdict`). It is
matched ONLY against the path VALUES the evaluator resolved — never against
the raw or a reconstructed command text:

* the B.1 targets of every command with a B.1 row (resolved with
  `eval::resolve_at`; a relative value is also joined to the session cwd
  when no `cd` applies, and its spelling as written is matched too, since
  every non-home row is `**/`-anchored);
* the B.2 writing redirects (`>`, `>|`, `>>`, `&>`, `&>>`, `<>`, `>&path`).

An S5 Python `Fs` effect reaches it through its shell spelling (`os.replace`
→ `mv --`, `os.chmod` → `chmod MODE --`, `open(.., 'wb')` → `: >`); there is
no second table.

| Row | Patterns (component-anchored, case-insensitive) | Effects denied | Lifted inside a linked worktree checkout |
|---|---|---|---|
| Q3 home directory itself | `/Users/*`, `/home/*` | Delete (`rm`, `mv` source), Metadata, Link | no |
| Q3 `.ssh` | `**/.ssh`, `**/.ssh/**` | every write effect | no |
| Q3 `.gitconfig` | `**/.gitconfig` | every write effect | no |
| Q4 `.githooks` | `**/.githooks`, `**/.githooks/**` | every write effect | yes |
| Q4 gate scripts | `**/scripts/check-*.py` | every write effect | yes |
| Q4 plugin hooks | `**/crates/*/hooks`, `**/crates/*/hooks/**` | every write effect | yes |
| Q4 deployed plugin | `**/.claude/plugins/cache/**/{bin,hooks}` and below | every write effect | no |

* "Every write effect" includes `chmod` / `chown` / `chgrp`. User ruling
  2026-11-20 「書込み Deny・exec 付与は許可」: on the four Q4 rows a `chmod`
  that ONLY adds exec bits — every word literal, no option but a lone `--`,
  MODE `[ugoa]*+x` clauses (`+x`, `u+x`, `a+x`, `ug+x`) or octal `755` /
  `775` — is not raised (today's verdict stands). Every other mode change
  (`777`, `o+w`, `-x`, `000`, `-R …`, `+X`) on any row, and any `chmod` on a
  Q3 row, is Deny (the legacy chmod arm denies only modes that remove read /
  exec). Reads (`cat`, `<`, a `cp` source, `open(.., 'r')`) are never
  matched.
* Each target is matched as written (`//`, `/./`, leading `./`, trailing `/`
  collapsed) and with `..` resolved lexically; a match in either counts.
  `/tmp/sshx/f` and `/tmp/notscripts/check-x.txt` do not match (`*` never
  crosses `/`).
* No safe root, scratch / temp / cache root exempts a row. The only place a
  row does not raise is the existing protected-path lift (user ruling
  2026-10-03): the three repository-relative Q4 rows strictly inside a linked
  worktree checkout (`SafeRoots::classify_worktree_checkout_value` Inside).
* Verdict: a match → **Deny** (main and subagent). The table failing to load
  while a write target exists, a B.1 target value that is `Undetermined`
  with no match elsewhere (the same Ask Table B already gives it), or a
  panic → Ask (main) / Deny (subagent). Otherwise the layer has no opinion
  (`Allow`, folded `Deny > Ask > Allow`), so it never lowers another layer's
  verdict.
* Not judged by this layer (the legacy layers still judge them): an
  `Undetermined` redirect target, an unmodelled wrapper (`sudo`,
  `env`, …), `analyze` = `Undetermined`, a glob that might expand to a row
  (`/x/.ss*`; matched as spelled), the symlink-resolved landing, and a
  recursive delete of an ancestor of a row other than the row directories
  themselves (`rm -rf <repo>/scripts`).

## Undetermined handling (raise-only legacy layer)

1. `analyze(text)` = `Undetermined(cause)` or `judge(..)` = `Undetermined(cause)`
   → the verdict is **Ask** (main) / **Deny** (subagent), except `TooLong` /
   `TooDeep` → Deny (ruling 2026-11-16).
2. The legacy string harm detection (the readings in `detect.rs` /
   `readings.rs`) still runs on that path, but its result may only **raise**:
   legacy Deny lifts Ask → Deny (keeping `system_harm`); legacy Ask or Allow
   leaves the Ask. It is never consulted to lower a verdict to Allow.
3. When `judge` is Known (Allow / Ask / Deny), S3 keeps the legacy readings
   — all of them, not only the harm readings (ruling 14) — raise-only too
   until S6 retires them for the migrated rows; a migrated row's
   detector no longer has a string-reading fallback (ruling 7).
4. A `judge` panic is caught and is `Undetermined(Panic)` → rule 1.

## Python programs (S5)

### Rulings (quoted)

* 2026-11-17, structural direction: no per-issue patches; embedded Python
  gets a real parser, and its effects are judged with the same effect ×
  target rules (Tables B / C) as shell — `python3 -c`, `python3 - <<'EOF'`
  and Python fed through stdin.
* Parser: rustpython-parser 0.4 with `default-features = false` +
  `num-bigint`. malachite is LGPL and must not be linked
  (`cargo tree -p progeffect -e normal | grep -i malachite` prints nothing).
* Shared crate: the per-language parsers and the effect × target rules live
  in ONE shared lib crate (`crates/progeffect`), used by blastguard
  (embedded programs) and by the future script-quarantine plugin; versioned
  like harness-core (Cargo.toml only, registered in
  `scripts/check-version-bumped.py` `SHARED_CRATES`).
* Text vs code is decided by a real parser, never by heuristics: string
  literals, comments and the literal parts of f-strings are DATA; only calls
  are effects.
* Undetermined → Ask (main) / Deny (subagent): dynamic dispatch (`getattr`,
  `__dict__`, `exec` / `eval` / `compile`, `__import__`, `importlib`,
  `globals` / `vars` / `locals`, aliasing that cannot be followed, a
  non-literal argv) and a parse error. A call with no effect-capable name is
  Allow; anything that can run code or touch the file system by a name not
  listed is Undetermined. When in doubt, Undetermined.
* System harm stays Deny through any wrapper.
* `python3 x.py` stays B.5 Undetermined (S4); S5 covers only inline programs
  (`-c`, a stdin here-document, a literal piped in).
* Legacy string readings for python are retired ONLY where the parser gives
  a Known verdict; where it gives Undetermined the legacy reading is kept,
  raise-only (the S3b pattern).

### Effects and their shell spelling (`progeffect::Effect::shell_spelling`)

Each effect is judged by the FULL detector on the shell command that has
exactly that effect (prefixed with `cd 'DIR' && ` when the command's cwd is a
literal directory; a `OneOf` / undetermined cwd with any effect →
Undetermined). One rule table, no drift: this is the `judge_candidates`
precedent while `classify` is not wired into `judge`.

| Python call (after import / alias resolution) | Effect | Judged as |
|---|---|---|
| `os.system(S)`, `os.popen(S[, 'r'])`, `subprocess.getoutput(S)` / `getstatusoutput(S)`, `subprocess.*(S, shell=True)` | Shell | `S` itself (the command `/bin/sh -c S` runs); with a literal cwd `cd 'D' && { S⏎}` |
| `subprocess.run` / `call` / `check_call` / `check_output` / `Popen` with a literal list (or string) | Exec | the quoted argv; `shell=True` with a list → `sh -c …` |
| `open` / `io.open` / `codecs.open` / `gzip|bz2|lzma.open` with mode `w` / `x` / `+` (`r+` read as truncate) / `a`; `Path.write_text` / `write_bytes`; `Path.open('w')`; `os.open` unless `O_RDONLY` | Truncate / Append | `: > 'P'` / `: >> 'P'` |
| `os.remove` / `unlink`, `Path.unlink` | Delete | `rm -- 'P'` |
| `shutil.rmtree` | Delete (recursive) | `rm -rf -- 'P'` |
| `os.rmdir`, `Path.rmdir` | RemoveDir | `rmdir -- 'P'` |
| `os.rename` / `replace` / `renames`, `shutil.move`, `Path.rename` / `replace` | Move | `mv -- 'A' 'B'` |
| `shutil.copy` / `copy2` / `copyfile` / `copytree` | Copy | `cp [-R] -- 'A' 'B'` |
| `os.chmod` / `lchmod`, `Path.chmod` (integer literal mode) | Chmod | `chmod MODE -- 'P'` |
| `os.chown` / `lchown`, `shutil.chown` (literal owner) | Chown | `chown 'O' -- 'P'` |
| `os.symlink` / `link`, `Path.symlink_to` / `hardlink_to` | Link | `ln [-s] -- 'T' 'L'` |
| `os.mkdir` / `makedirs`, `Path.mkdir` | MakeDir | `mkdir -p -- 'P'` |
| `os.utime`, `Path.touch` | Touch | `touch -- 'P'` |
| `os.truncate(P, N)` | Resize | `: > 'P'` for 0, else `truncate -s N -- 'P'` |

Operands: string literals, f-strings without fields, `+` concatenation, a
name bound once by a module-level `name = value`, `sys.argv[i]` of a literal
command-line word (`['-c', …]` for `-c`, `['-', …]` / `['']` for stdin),
`str()`, `os.fspath`, `os.path.join`, `pathlib.Path(…)` and `/` joins.
Anything else → Undetermined.

### Undetermined (the program is not read)

* a parse error, a NUL byte, a non-UTF-8 coding cookie, a program over
  16 KiB, nesting deeper than 200;
* any reference to `exec`, `eval`, `compile`, `__import__`, `getattr`,
  `setattr`, `delattr`, `globals`, `locals`, `vars`, `breakpoint`, `help`,
  `__builtins__`, `__loader__`, `__spec__`; a dunder attribute other than
  `__name__` / `__doc__` / `__file__` / `__version__` / `__qualname__`
  (`os.__dict__[…]`, backlog c1873b73); `sys.modules`;
* an import of a module outside the standard library, a relative import,
  `from m import *`; a non-ASCII identifier (Python NFKC-normalises them);
* an import alias whose name is ALSO bound some other way (an alias is
  followed only when every binding of the name is the same import);
* an effect-capable function or module used as a value (`f = os.system`), a
  store into a module (`os.environ[…] = …`, `sys.argv = …`);
* a call into an effect-capable standard-library module that is not
  modelled (`os.fork`, `importlib.*`, `pickle.loads`, `shutil.unpack_archive`, …);
  a method named like a process / file-system operation on a receiver this
  reading does not resolve (`x.system(…)`, `p.unlink()`, `df.to_csv(path)`);
* an effect whose command / path is not determined; `subprocess` keywords
  that change what or how it runs (`executable`, `env`, `cwd`, `input`,
  `stdin`, `preexec_fn`, …); `os.popen(…, 'w')`; `dir_fd`; `opener`;
* the program both writes a file and runs a program;
* `python -i` / `python -x`; a line that sets `PATH`, `PYTHON*`, `LD_*`, … ;
* a non-ASCII directory component in a path of a program that uses a
  `\N{…}` escape (backlog 47b20647 pinned `open('/\N{SNOWMAN}tc/hosts','w')`
  as undetermined; S5 does not lower that on its own authority).

### Retiring the legacy readings

`ast/s5.rs::prepare` hands the legacy readings a NEUTRAL text in which each
Known program is `pass` (the `-c` operand, or the here-document body). A
program is NOT retired (its legacy readings keep running on the original
text, raise-only) when it is Undetermined; sits in a command / process
substitution, inside another command's word / redirect / here-document body,
or is a non-last pipeline stage; has an effect (or an output redirect) while
another command on the line may run a file (write-then-run stays with the
legacy readings — and the harness-5b-owned written-file reading,
dc27f669 / bf90fa9d, is untouched); is a `-c` program that runs a command
while the line feeds Python's stdin; or when the neutral text does not
re-analyse to the same commands.

### S5b follow-ups (user rulings 2026-11-19)

* (a) **Import shadowing.** `python -c` / a program on stdin puts the
  directory it runs in first on `sys.path`. A program that imports `M`
  (`import M…` / `from M… import`, top-level name, anywhere in the program)
  while that directory holds `M.py`, a directory `M/`, or a compiled
  `M.pyc` / `M.*.so` / `M.pyd` (names compared ignoring ASCII case) is
  **Undetermined**. So is one whose directory cannot be listed (an IO error,
  the hook's cwd absent from the payload, a cwd the text does not determine).
  A directory that does not exist holds nothing that shadows. Listing:
  `harness_core::boundary::read_dir_entries_checked` (`s5.rs::shadowed_import`).
* (a') **Same-line shadowing (S5c, user ruling 2026-11-20).** "When a command
  EARLIER on the same line may write `<mod>.py` / `<mod>/` (or any of the
  shadow spellings S5b already checks) into the directory a later python3
  program runs in, for a module that program imports → Undetermined (Ask).
  An earlier write whose target is undetermined → also not Allow."
  Implemented in `s5.rs::same_line_shadow` from the existing target
  extraction only: output redirects (`>` `>>` `>|` `<>` `>&word` `&>` `&>>`,
  B.2 `RedirTarget::Path`, the program's own included — the shell opens them
  first) and the creating targets of the B.1 rows (`Truncate` / `Append` /
  `Rewrite` / `Link` / `CreateTree`: `cp` / `mv` / `install` destination,
  `ln` link name, `tee`, `truncate`, `dd of=`, `sed -i`, `find -fprint`)
  plus `mkdir` (`-p -v -m`) and `touch` (`-a -c -f -h -m`, `-A -d -r -t`),
  rows that exist only for this reader (`judge/table_b.rs::entry_writes`;
  the S3b judge is unchanged), after the `b5` wrappers, resolved by
  `eval::resolve_at`. An entry of a program directory whose name shadows an
  imported module (same spellings and case rule as (a)), or any path under
  such an entry, → Undetermined. Also Undetermined: a write target that
  cannot be placed, a modelled command with an unmodelled option, a wrapper
  that cannot be unwrapped, a write to the program directory itself (`cp x
  .`: the new entry's name is not read), a real path that cannot be read.
  Targets and directories are compared as spelled and through the real path
  of their longest existing ancestor, the directory prefix ignoring ASCII
  case. "Earlier": the commands that start before the program ends in the
  text; every command of the line when the program is not top-level (a
  pipeline stage runs concurrently) or the line has a loop or a function.
* (c) **`bash -c` / `sh -c`.** The literal script of a plain `bash -c SCRIPT`
  / `sh -c SCRIPT` (after the `b5` wrappers, `sudo` / `doas` included) is judged by S5
  exactly like the same text written directly, and its Known programs are
  retired from the outer line's legacy readings (the script word is replaced
  by its neutral text). Only this shape is read (`s5.rs::shell_c_script`):
  `bash` / `sh` bare or in a system bin directory, exactly `-c SCRIPT` (no
  `$0` / positional words, no other option), nothing feeding its stdin, the
  hook's cwd or one literal directory, and no command on the line that
  assigns a variable or runs `export` / `declare` / `typeset` / `local` /
  `readonly` / `env` / `unset` / `source` / `.` / `eval` / `alias` / `enable`
  / `shopt` / `set` (the script is analysed on its own). Any other shape keeps
  the legacy readings only, as before.
* (d) **`sh -c 'echo "$HOME"'`.** A shell's `-c` payload candidates are the
  word boundaries after `-c`, no longer every whitespace position
  (`payload.rs::dash_c_payloads` → `interpreter_payloads_after`, backlog
  1943843c): the mid-word fragment `"$HOME"'` was read as a command line whose
  command word was `$HOME`. A word whose quote never closes keeps every
  position.

### Known gaps (S5)

* A here-string / piped literal program is analysed but never retired.
* Python nested in a `bash -c` outside the shape above, or in any other word,
  is judged by the legacy readings only.
* Wrappers other than those `b5` unwraps (`xargs`, `su`, …) get no S5
  opinion.
* Import shadowing: a module imported by an imported module, and `.pth` /
  `PYTHONPATH` / site-packages entries are not detected. Same-line (S5c): a
  shadowing entry written by a program with no effect model (`git checkout`,
  `tar`, `unzip`, `curl -o`, an unknown program), by `xargs` wrapping
  `mkdir` / `touch`, as a `sed -i` backup suffix, or by a command outside the
  `bash -c` whose script runs the program, is not detected.

## Stage map

| Stage | Implements |
|---|---|
| S1 `subset-spec` | this document; frozen types in `judge.rs` (stubs, not wired) |
| S2 `red-tests` | an independent author (not the S3 implementer; CLAUDE.md §2(a)) writes a RED test for each Table B row and each Table C cell that is not `legacy`, each observed failing against the S1 stub before S3 makes it GREEN |
| S3 `path-cutover` | Table A enforcement for judged commands; Table B B.1 (all rows except `perl -i` which is already Undetermined), B.2, B.3, B.4; Table C (all rows); `classify` from the existing predicates; wire `judge` into `detect` with the "Undetermined handling" layer |
| S3 status (2026-11-18) | done: ruling 12 — `$HOME` / `${HOME}` / leading `~` are the hook environment's `HOME`, in the evaluator (`eval.rs` `Env::initial`) and in the one static resolver every legacy reading sees (`readings/static_bindings.rs`), so a `$HOME` spelling is judged exactly as its absolute spelling. NOT done (at S3; `judge` is wired for B.1 since S3b, `classify` implemented but unwired since S6-2): `judge` / `classify` were still the S1 stubs at S3 and were not wired (open question raised to the user: under raise-only every Known Allow cell is inert, so wiring only adds Asks — including `Undetermined` for every unmodelled option such as `cp -p` — whose effect on commands allowed today is not measured by the block-log replay) |
| S3b `tableb-judge` (user ruling 2026-11-18) | done: `judge` wired into the final decision (`ast/mod.rs::table_b_verdict`, folded raise-only in `detect/readings.rs::detect_bash_every_reading`) ONLY for commands with a **B.1** row (`ast/judge/table_b.rs`). For such a command an option not in its row, an undetermined word that may be an option, or a `dd` operand that is not `key=value` → `Undetermined` → Ask (main) / Deny (subagent); a target word whose value the evaluator cannot determine (unquoted glob other than a literal-dir glob, `~user`, `$( )`, unbound / positional parameter, relative path after an undetermined `cd`, `xargs` operand read from non-literal stdin) → `Unresolved` → Ask / Deny. A literal-dir glob (user ruling 2026-11-19, Table A.3) is placed by its parent directory, like a literal target there (no finding while `classify` is not wired into `judge`; the legacy layer judges it); for a non-recursive `rm` the legacy arm judges such a glob placed in the project / scratch / own worktree like the named files it expands to (`detect/rm.rs::nonrec_glob_placed`; the `/tmp` root 4-character head rule of 2026-11-05 still applies). Commands without a B.1 row, and `analyze` = `Undetermined`, are unchanged; unknown programs stay Allow; the legacy layer is not lowered (a legacy Deny / Ask, with its reason, wins). See "S3b notes" |
| S4 `stdin-program` | Table B B.5 rows for shells: `-c` inline, stdin (`StdinSource::Literal` bodies), `eval`; the script-file row and the direct-execution row (Undetermined, and the recursive judgement of a body the same command writes — rulings 10, 17); wrapper → interpreter unwrap. `source` / `.` are NOT in S4 (ruling 16) |
| S4 status (2026-11-18) | **landed** as the standalone B.5 layer `b5.rs` (rulings 18–21), raising the legacy verdict in `readings.rs::detect_bash_every_reading`; `judge.rs` stays unwired. Wrappers unwrapped: `env`, `nohup`, `timeout`, `nice`, `time`, `exec`, `command`; since backlog e1bb5f7e (2026-11-20, ruling Q1) also `sudo`, `doas` (B.3 row; `-s/-i` alone = implicit shell). Known gaps (legacy alone judges them): other wrappers (`xargs`, `su`, `flock`, `find -exec`, …) and `BASH_ENV` / `ENV` assignments |
| S5 `python-parser` | Table B B.5 python row (a real Python parser; effects × targets of the parsed program judged by Table C); legacy python readings until then (ruling 15) |
| S5 status (2026-11-19) | **landed**: shared crate `crates/progeffect` (rustpython-parser 0.4, `default-features = false` + `num-bigint`; no malachite) reads the program; `ast/s5.rs::prepare` judges every effect with the full detector on its shell spelling and retires the legacy readings of Known programs (`readings.rs`). See "Python programs (S5)" |
| S6 `retire-readings` | remove the string-reading fallbacks of every migrated row (`targets.rs::resolve` callers, `readings.rs` re-readings); until S6 the whole legacy layer is raise-only (ruling 14), so Table C Allow cells a legacy reading answers Ask / Deny today turn GREEN here; revisit the deferred Open questions 1–4 |
| S6-2 status (2026-11-20) | `judge::classify` answers the Table C class from the existing predicates (`rm/new_layer.rs` `home_harm` / `system_spellings` / `is_protected` / `own_worktree`, `Ctx::confined_root`, `protected::{real_system_path_block, physical_landing, launch_agents_new_file_ask}`, `harm_mark::is_raw_disk`, the `SafeRoots` value readings) in the `PathClass` precedence; `..`, an undetermined target / cwd and an `Undetermined` predicate → `Unresolved`, except the two worktree predicates, whose `Undetermined` falls through (never less restrictive: `OwnWorktree` is an Allow cell, the rest are `legacy`). NOT wired at S6-2: no caller outside `judge/classify_tests.rs` (since S6-4: `detect/known_allow.rs`); `judge` still gives a placed target no finding and no verdict changes (user ruling 2026-11-20) |
| S6-3 status (2026-11-20) | parity only, no verdict change. `targets.rs` indexes every evaluated top-level command whose command word is the literal `X` / `/bin/X` / `/usr/bin/X` (was: `rm` only) with its argv and its B.1 row (`table_b::extract`); `tied_operand_values(name, rest)` is the former `rm_operand_values` for any name, `tied_b1_targets(name, rest)` hands back the tied commands' (Effect, target, cwd) — `None` when nothing is tied or a tied command has no / an undetermined B.1 row. `detect/known_allow.rs::known_allow(cmd, rest, ctx)`: `true` only for ≥ 1 target, every target a `Literal`, every `classify` class Scratch / OwnWorktree; NOT called by any detector at S6-3 (first caller: S6-4, below). `rm` reads the generic tie (`tied_operand_values("rm", …)`) but keeps `new_layer`'s own option gate (`-I` / `--` stay legacy, user ruling) and its own predicate `value_allows`, because `classify` is not the same predicate — read from the code, no test observes them yet: `classify` lifts Protected strictly inside a linked worktree checkout (`new_layer` never does), exempts a system path `confined_root` places in a safe root and limits `/dev` to raw disks (`new_layer`: no escape, all of `/dev`), does not refuse a final `.` component (`new_layer` does), and additionally consults `real_system_path_block` / `physical_landing` (narrower). `rsync --delete` is unchanged (`rsync_dest_allows`): rsync has no B.1 row in `table_b::extract`, so `known_allow("rsync", …)` is always `false` |
| S6-4 status (2026-11-20) | **landed** (retire only, no new Ask / Deny): the legacy `truncate` arm in `detect.rs` answers Allow when `known_allow("truncate", rest, ctx)` is `true`; otherwise its legacy verdict (`shape_deny_or_confined_ask`) is unchanged. Three changes make that true where Table C says Allow and only there: (a) the per-command tie no longer reads the separate argument of a listed option (`truncate -s 0`: the `0`; `table_b::option_argument_words`, every row parsed by one plain `parse`, the `-t DIR` target argument excluded; `sed`, `dd`, `find`, `xargs`: none) as an operand spelling — before, a `0` anywhere in a target path untied the command; `rm` has no option with an argument, so its tie is unchanged; (b) a final-component literal-dir glob in Scratch is a `Literal` placed by its parent (user ruling 2026-11-19, every Table B row) and so Allow; a glob in a middle component stays `Unresolved` (legacy); (c) user ruling 2026-11-20 「Deny のまま（推奨）」: `known_allow` is `false` when any target names command-running config by the UNLIFTED predicates (`judge::names_command_running_config`: `rm::new_layer::is_protected`, any Q3/Q4 deny-target row without the checkout lift, a `hooks/hooks.json` at any depth; on the value and its physical landing; unplaceable → refused), so `classify`'s Protected lift inside a linked worktree never makes `.git`, `.claude/settings*.json`, `.githooks/…`, `crates/*/hooks/…`, `hooks/hooks.json` Allow — their legacy verdict stays (`classify` itself is unchanged). Pinned by `tests/s6_4_truncate_retire_20261120.rs` |
| S6-5 status (2026-11-20) | **landed** (retire only, no new Ask / Deny): the legacy `dd` arm in `detect.rs` answers Allow when `known_allow("dd", rest, ctx)` is `true`; otherwise the legacy `of=` reading's Deny is unchanged. The `dd` B.1 row (`table_b::dd`): `of=F` → target `F` (`Truncate`, `Rewrite` with `conv=notrunc`), `if=` and every other `key=value` word is not a target, a non-`key=value` word is `Undetermined`. Its operands are whole `k=v` words, so a `0` / `1` path component never equals an option value and `option_argument_words` needs no `dd` entry (none). One row change: an `of=` word whose value is `Undetermined` because it is a glob now goes through `literal_dir_glob` (whole word, then `of=` stripped), so `of=DIR/*.txt` under a literal Scratch dir is a `Literal` placed by `DIR` (user ruling 2026-11-19, every Table B row); a middle-component glob stays `Unresolved`. A same-line literal assignment (`F=…; dd of=$F`) is resolved by the evaluator to a `Literal` and judged by that value (standing ruling: text-determined values are resolved) — Scratch → Allow, `/etc/hosts` → Deny. Command-running config and Protected inside the own worktree keep Deny (ruling 2026-11-20 「Deny のまま」, the S6-4 (c) check). Precondition (0.2.792, p0 fail-open): the legacy arm and `harm_mark` read `of=` from the shell-unquoted words (`rm::dd_of_operands`), not the raw token, and a word that may expand to `of=` is an `of=` write. Pinned by `tests/s6_5_dd_retire_20261120.rs` |
| S6-6 status (2026-11-20) | **landed** (retire only, no new Ask / Deny): the legacy `tee` arm (`detect/tee.rs::analyze_tee`, moved out of `detect.rs`) answers Allow when `known_allow("tee", rest, ctx)` is `true`; otherwise its legacy verdict is unchanged. The `tee` B.1 row (`table_b::tee`): every operand a target, `Truncate`, `Append` with `-a`; options `-a` / `--append`, `-i` / `--ignore-interrupts`, `-p`, as option clusters too (`-ai`, `-ap`), any other option (`-x`, `--output-error`) makes the row `Undetermined` (not Known). Two tie changes: (a) the legacy tokens still carry the line's redirections (`tee F > /dev/null`: `>`, `/dev/null`), which the AST argv has not, so the tee arm ties `rest` without them (a bare operator and its target, every other `<` / `>` token; the redirection stays the redirect rules'); (b) `targets.rs::Index::tie` accepts one more occurrence of an operand spelling — the whole unquoted value word of an assignment whose Literal value is that spelling, when every tied operand evaluated to it — because the index text is the line after the legacy walk substituted a same-line literal assignment into `$F` (`F=/t/f; tee "$F"` is indexed as `F=/t/f; tee /t/f`); for `tee` only — every other row keeps its pinned legacy verdict for that shape (`F=<Scratch>; truncate -s 0 $F` stays ask/deny, `tests/s6_4_truncate_retire_20261120.rs` `control_non_literal`) until a ruling extends it. Command-running config and Protected in the own worktree keep their legacy verdict (the S6-4 (c) check). Precondition (0.2.814, fail-opens): a glob word is not an absent target (final-component literal-dir glob → its directory's class; unplaced → Ask), and `tee -a` / `>>` onto own-worktree command-running config is Ask (main) / Deny (subagent), user ruling 2026-11-20. Pinned by `tests/s6_6_tee_retire_20261120.rs` |
| quarantine plugin (separate) | `source` / `.` → Ask (main) / Deny (subagent), introduced together with the script-quarantine plugin (ruling 16) |

### S3b notes (2026-11-18)

* NOT wired in S3b (verdict unchanged, legacy only): B.2 redirects, B.3
  wrappers (`sudo rm …`, `command rm …`), B.4 git, B.5 interpreters, the
  "undetermined command word → Undetermined" rule, and every Table C cell of
  a target whose value IS determined — `classify` was the S1 stub at S3b and,
  though S6-2 implemented it, is still not called by `judge`, so a
  placed target gets no finding and the raise-only legacy layer alone judges
  it (S3b `Judgement::Allow` = "nothing undetermined found"). Exception (user
  ruling 2026-11-18): a relative target whose cwd is one of a finite set is
  judged per candidate (Table B "Common rules"), which can produce
  `Judgement::Deny`.
* Option lists: the B.1 rows above, plus the GNU long spelling of a listed
  short option (`--append` = `-a`, `--target-directory=DIR` = `-t DIR`, …;
  list in `table_b.rs`). Options after operands count as options (GNU
  permutation), except for `xargs`. An undetermined word counts as a possible
  option unless its raw text provably starts with a literal that is not `-`
  (`"/tmp/$x"`, `~user/f`). `chmod -w f`: a `-[rwxXst]+` word is the mode.
  `sed` has a row only when a literal `-…i…` / `--in-place` word is present;
  BSD `-i ''` is modelled.
* **Implementer additions, decided by user ruling 2026-11-18:**
  1. `--help` / `--version` are accepted by every B.1 row — **accepted**;
  2. ~~an unlisted option is NOT Undetermined when every word after the
     command is a literal option without an argument (`rm -h`)~~ —
     **removed**: an unlisted literal option is Undetermined → Ask (main) /
     Deny (subagent) even with no operand (`rm -h`, `rm -P`);
  3. a GNU long spelling of a listed short option is that option
     (`--append` = `-a`) — **accepted**.
* `find`: an undetermined word in the expression is not read (it may be the
  argument of a primary; primary arities are not modelled), so `find . "$x"`
  where `$x` is `-delete` is NOT caught by S3b (legacy verdict unchanged).
* `rmdir -p` ancestors are not extracted (they only matter for a placed path).

## Open questions

Decided by the follow-up rulings 8–10 (2026-11-17): scratchpad scope →
`Scratch` row; own-worktree scope → `OwnWorktree` / `OtherWorktree` rows;
`bash script.sh` → B.5 script-file row. Unresolved targets — non-delete writes
(`echo x > "$f"`) and deletes alike — are Ask / Deny per ruling 2 as Table C
states (accepted 2026-11-17; where today's rule answers Deny, the raise-only
layer keeps that Deny).

Deferred (ruling 11): **legacy (today's behaviour) until S6, revisited there.**
S3 does not implement these.

1. **`find -exec … {}`**: may `{}` inherit the search root's class
   (descendants), or is it always Unresolved?
2. **sed scripts that write / execute** (`w`, `W`, `e`, `s///w`, `s///e`): needs
   a sed-script tokenizer — inside the "no corner-case modelling" ruling or not?
3. **Project / Other / OtherWorktree cells marked legacy**: should Table C state
   explicit verdicts (e.g. Truncate of an Other path = Allow) instead of
   reproducing the current rules?
4. **Non-path detectors** (git push / branch -D, gh, high_blast tiers,
   osascript, …): stay as legacy detectors beside `judge`, or migrate into
   Table B?
