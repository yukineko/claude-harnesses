use super::{exclude, has_glob_meta, Ctx, Decision, Determination};

/// Write is new/overwrite both. We stay conservative and only Deny the clearly
/// destructive shapes: replacing a (non-config) file with empty content, or
/// overwriting git internals. Everything else is allowed.
/// Verdict for a tool call that would MODIFY a protected gate/hook/policy path,
/// or `None` when the target is not one.
///
/// The wording is shared by every call site (Write, Edit/MultiEdit/
/// NotebookEdit, truncating redirect, append redirect, `cp`/`mv` destination,
/// `tee`, `sed -i`, `sort -o`, `uniq` out) so that [`crate::rule_id`] gives the
/// whole class ONE stable signature.
///
/// # Why this is an `Ask` and not a `Deny`
///
/// The path match is certain; the HARM is not. blastguard can establish that
/// `.githooks/pre-push` decides whether a gate runs, but it cannot establish
/// what the incoming bytes DO to it — the resulting content is not derivable
/// from the tool call alone, and this crate performs no filesystem I/O
/// (`exclude.rs:409-413`). Adding a crate to a gate's crate list STRENGTHENS the
/// gate; inserting `exit 0` above it DISARMS it. Both arrive here as "an Edit
/// targets a protected path".
///
/// Collapsing those two into `Deny` recorded "blastguard knows this is bad" for
/// a case where it knew only "blastguard cannot tell", which is the same
/// two-answers-for-three-situations defect [`crate::model`] was written to
/// remove. It also made every such change reachable only by a human editing the
/// file by hand — which is strictly LESS auditable than an agent's edit, because
/// a hand edit leaves no diff, no test run and no commit trail.
///
/// This is not a softening. [`crate::interactive::ask_available`] resolves `Ask`
/// to `Deny` in every context where no human can answer — headless `claude -p`,
/// cron, condukt workers, SDK sessions — so the restrictive resolution CLAUDE.md
/// requires still holds wherever the autonomous-agent threat model applies. What
/// changes is only the interactive case, where the human who owns the gate gets
/// to see the diff and decide.
///
/// DESTRUCTION of a protected path is a different question and keeps its `Deny`:
/// see [`protected_disarm_deny`] and [`protected_tree_deny`], where the outcome
/// IS established by the command alone (a deleted file stops running; a
/// recursive delete takes every protected path under it).
///
/// # Symlinks (user ruling 2026-11-01)
///
/// The path is judged TWICE: lexically as written (case-fold, `..`, trailing
/// slash — [`exclude::is_workflow_path`] / [`exclude::is_protected_path`]), and
/// again at the path the kernel really writes, with every symlink resolved
/// ([`physical_landing`], base = the segment's raw cwd). Either match Asks, so
/// `wf/ci.yml` with `wf -> .github/workflows`, a file symlink onto
/// `.claude/settings.json`, and a chain of links are all the same write as the
/// direct spelling. A redirect after a `cd` is resolved from the `cd`'s
/// directory with its own `..` applied after the symlinks
/// ([`protected_path_block_at`]), so `cd link3 && echo x > ../settings.json`
/// is judged where it lands. When the resolution itself fails (a symlink loop, an
/// unreadable component, any I/O error other than "does not exist yet") the
/// write is Undetermined and Asks — "could not resolve" is never read as "not
/// a protected path" (CLAUDE.md §3). A target with an unexpanded `$`/backtick,
/// or a relative one with no known base, cannot be placed and keeps the
/// lexical verdict alone.
pub(super) fn protected_path_block(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    protected_path_block_at(ctx, action, path, path)
}

/// [`protected_path_block`] with the two judgements fed different spellings:
/// `path` is judged lexically, `physical` is resolved through its symlinks.
/// They differ for a redirect after a `cd` ([`physical_redirect_spelling`]):
/// the lexical form has `..` collapsed against the `cd` text, the physical one
/// keeps it so it is applied to the directory the `cd` really entered.
pub(super) fn protected_path_block_at(
    ctx: &Ctx<'_>,
    action: &str,
    path: &str,
    physical: &str,
) -> Option<Decision> {
    if let Some(d) = protected_path_lexical_block(ctx, action, path) {
        return Some(d);
    }
    match physical_landing(physical, ctx.raw_base.as_deref())? {
        Determination::Known(real) => protected_verdict(
            ctx,
            action,
            &format!("{path}, which resolves to {real}"),
            &real,
        ),
        Determination::Undetermined(u) => Some(Decision::ask(format!(
            "{action} target ({path}) could not be resolved through its symlinks ({u}) — \
blastguard cannot tell whether it lands on a gate/config file it protects or on a GitHub \
Actions workflow, so it refuses to guess"
        ))),
    }
}

/// [`protected_path_block`] without the symlink resolution: the path judged
/// only as written. Used where the target's base is NOT known (an unplaceable
/// redirect, which is asked about separately), so resolving it against the
/// session cwd would judge a different file.
pub(super) fn protected_path_lexical_block(
    ctx: &Ctx<'_>,
    action: &str,
    path: &str,
) -> Option<Decision> {
    protected_verdict(ctx, action, path, path)
}

/// The shared verdict: `judged` is the path tested against the workflow and
/// protected lists, `shown` is how it is named in the reason.
fn protected_verdict(ctx: &Ctx<'_>, action: &str, shown: &str, judged: &str) -> Option<Decision> {
    // GitHub Actions workflow files: configuring GHA needs an explicit user
    // request. Unlike the protected-gate paths below, a worktree checkout does
    // NOT exempt this (the file is pushed to the same remote Actions), and every
    // write site that already routes through this function (Write, Edit,
    // MultiEdit, redirects, tee, cp/mv, sed -i, sort -o, ...) inherits it.
    if exclude::is_workflow_path(judged) {
        return Some(Decision::ask(format!(
            "{action} targets a GitHub Actions workflow file ({shown}) — GitHub Actions \
configuration requires an explicit user request"
        )));
    }
    if exclude::is_protected_path(judged) && !ctx.in_worktree_checkout(judged) {
        Some(Decision::ask(format!(
            "{action} targets a protected gate/config path ({shown}) — it controls which hooks, \
gates or policies run, and blastguard cannot tell from the tool call whether this strengthens \
or disarms it, so it refuses to guess"
        )))
    } else {
        None
    }
}

/// Upper bound on symlinks followed while resolving one path (Linux
/// `MAXSYMLINKS` is 40; macOS stops at 32, so nothing the kernel can open is
/// cut short). Exceeding it is how a loop is detected.
const MAX_SYMLINK_HOPS: usize = 40;

/// The path the kernel really writes for `target`, every symlink resolved —
/// INCLUDING a final file symlink whose target does not exist yet (followed
/// through `read_link`, since a write through it creates the target) — with
/// any not-yet-existing tail kept as written. `..` is applied PHYSICALLY to the
/// resolved prefix, not lexically to the text.
///
/// `None` = not a path this can place (an unexpanded `$`/backtick, or relative
/// with no absolute base). `Undetermined` = the filesystem could not answer: a
/// symlink loop, a `read_link`/`lstat` error other than "does not exist".
fn physical_landing(target: &str, base: Option<&str>) -> Option<Determination<String>> {
    use std::path::{Component, Path, PathBuf};
    if target.is_empty() || target.contains('$') || target.contains('`') {
        return None;
    }
    let start = if target.starts_with('/') {
        PathBuf::from(target)
    } else {
        let base = base.filter(|b| b.starts_with('/'))?;
        Path::new(base).join(target)
    };
    /// One path component, owned (a `Component` borrows its path, and link
    /// targets are spliced in as the walk proceeds).
    enum Step {
        Root,
        Parent,
        Name(std::ffi::OsString),
    }
    fn push_front(pending: &mut std::collections::VecDeque<Step>, p: &Path) {
        for c in p.components().rev() {
            match c {
                Component::RootDir | Component::Prefix(_) => pending.push_front(Step::Root),
                Component::CurDir => {}
                Component::ParentDir => pending.push_front(Step::Parent),
                Component::Normal(n) => pending.push_front(Step::Name(n.to_os_string())),
            }
        }
    }
    let mut pending = std::collections::VecDeque::new();
    push_front(&mut pending, &start);
    let mut resolved = PathBuf::from("/");
    // How many trailing components of `resolved` do not exist. Nothing below a
    // missing component can be a symlink, so those are appended unexamined; a
    // `..` that climbs back out of them returns to examining (a writer that
    // creates parents, as `mkdir -p` would, then meets the real components).
    let mut missing_depth = 0usize;
    let mut hops = 0usize;
    while let Some(step) = pending.pop_front() {
        match step {
            Step::Root => {
                resolved = PathBuf::from("/");
                missing_depth = 0;
            }
            Step::Parent => {
                resolved.pop();
                missing_depth = missing_depth.saturating_sub(1);
            }
            Step::Name(name) => {
                let candidate = resolved.join(&name);
                if missing_depth > 0 {
                    resolved = candidate;
                    missing_depth += 1;
                    continue;
                }
                match std::fs::symlink_metadata(&candidate) {
                    Ok(m) if m.file_type().is_symlink() => {
                        hops += 1;
                        if hops > MAX_SYMLINK_HOPS {
                            return Some(Determination::undetermined(format!(
                                "more than {MAX_SYMLINK_HOPS} symlinks followed at `{}` — a \
symlink loop",
                                candidate.display()
                            )));
                        }
                        match std::fs::read_link(&candidate) {
                            // `resolved` stays the link's parent: a relative
                            // link target is resolved from there, an absolute
                            // one restarts at `/` via its RootDir component.
                            Ok(link) => push_front(&mut pending, &link),
                            Err(e) => {
                                return Some(Determination::undetermined(format!(
                                    "symlink `{}` could not be read ({e})",
                                    candidate.display()
                                )))
                            }
                        }
                    }
                    Ok(_) => resolved = candidate,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        missing_depth = 1;
                        resolved = candidate;
                    }
                    Err(e) => {
                        return Some(Determination::undetermined(format!(
                            "`{}` could not be inspected ({e})",
                            candidate.display()
                        )))
                    }
                }
            }
        }
    }
    Some(Determination::Known(resolved.display().to_string()))
}

/// Deny reason for a write that lands in a SYSTEM directory, or `None` when the
/// target is elsewhere.
///
/// Sits beside [`protected_path_block`] as the second reason a write is
/// surfaced no matter how recoverable it is, and is checked in the same place
/// for the same reason: [`crate::reversible`] can only see bytes that already
/// exist, and the payload here is the file's existence rather than its content
/// (`/etc/sudoers.d/evil` destroys nothing and grants root).
///
/// Deny rather than the neighbouring `Ask`: see
/// [`crate::scope::is_inside_system_dir`] for why the ambiguity that makes
/// `Ask` honest for a gate-config path has no counterpart here.
///
/// A target the session legitimately owns escapes — a project checked out at
/// `/opt/myproj` is a safe root like any other, and `confined_root` is what
/// decides that. The escape is asked only for paths this predicate already
/// matched, so it can never ADMIT a system path that was not confined.
///
/// The Deny carries [`crate::model::SYSTEM_HARM_MARK`], so a repeat of it is
/// never downgraded to an Ask (user ruling 2026-11-07).
///
/// It also answers the per-user persistence directory: see
/// [`launch_agents_new_file_ask`], checked first and NOT escapable through
/// `confined_root`. That answer is an `Ask`, not a `Deny`.
pub(super) fn system_path_block(action: &str, target: &str, ctx: &Ctx) -> Option<Decision> {
    if let Some(ask) = launch_agents_new_file_ask(action, target) {
        return Some(ask);
    }
    if !crate::scope::is_inside_system_dir(target) {
        return None;
    }
    if ctx.confined_root(action, &[target]).is_some() {
        return None;
    }
    Some(Decision::deny_system_harm(format!(
        "'{action} {target}' writes into a system directory - a NEW file there is itself the \
change (a `sudoers.d`/`paths.d` drop-in, a LaunchDaemon plist, a binary shadowing one on \
$PATH), so whether the old bytes were recoverable does not make it safe"
    )))
}

/// Ask for a write that creates a NEW file inside a per-user
/// `Library/LaunchAgents` directory, or `None` otherwise.
///
/// User ruling 2026-11-07 (backlog e34eb0be): a new plist there registers a
/// program launchd runs at every login (persistence), so it is `Ask` for the
/// direct and the interpreter forms alike (a subagent cannot answer an Ask, so
/// there it is Deny). The system-wide `/Library/LaunchAgents` lies inside a
/// system directory: this returns `None` for every system-directory path, so
/// [`system_path_block`] keeps answering those with its Deny, never this Ask.
///
/// Matching is on the absolute, normalised path: a `Library/LaunchAgents`
/// component pair followed by at least one more component, compared
/// ASCII-case-insensitively (the default macOS volume is case-insensitive, so
/// `library/launchagents` lands in the same directory). It is not tied to
/// `$HOME`: every user's LaunchAgents directory is the same hazard, and not
/// reading `$HOME` means an unset or odd `$HOME` cannot make this answer
/// `None`. A leading `~/` is expanded against `$HOME` when it is set; a
/// relative path is not judged here (same contract as
/// [`crate::scope::is_inside_system_dir`]).
///
/// Only a target that does NOT exist gets the Ask: an existing file there is
/// left to the caller's recoverability rules, which Deny an unrecoverable
/// overwrite — answering Ask first would downgrade that Deny. A target whose
/// existence cannot be read (`symlink_metadata` fails with anything other
/// than "not found") is also an Ask (CLAUDE.md §3).
fn launch_agents_new_file_ask(action: &str, target: &str) -> Option<Decision> {
    let expanded;
    let target = match target.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) if home.starts_with('/') => {
                expanded = format!("{}/{rest}", home.trim_end_matches('/'));
                expanded.as_str()
            }
            _ => return None,
        },
        None => target,
    };
    let norm = exclude::normalize(target);
    if !norm.starts_with('/') || crate::scope::is_inside_system_dir(&norm) {
        return None;
    }
    let parts: Vec<&str> = norm.split('/').filter(|c| !c.is_empty()).collect();
    let inside = parts
        .windows(3)
        .any(|w| w[0].eq_ignore_ascii_case("Library") && w[1].eq_ignore_ascii_case("LaunchAgents"));
    if !inside {
        return None;
    }
    match std::fs::symlink_metadata(&norm) {
        Ok(_) => None,
        Err(e) => {
            let why = if e.kind() == std::io::ErrorKind::NotFound {
                "a NEW file there".to_string()
            } else {
                format!("a file whose existence could not be read ({e})")
            };
            Some(Decision::ask(format!(
                "'{action} {target}' creates {why} in a LaunchAgents directory — launchd runs \
a plist placed there at every login (persistence), so blastguard asks before installing one"
            )))
        }
    }
}

/// [`system_path_block`] applied to the path the kernel will really write
/// ([`physical_landing`]) instead of the text. `Err(reason)` when the real
/// landing could not be determined (a symlink loop, an unreadable link, an
/// `lstat` error other than "does not exist").
///
/// Backlog 2287ebae: [`system_path_block`] judges the LEXICAL path, so
/// `: > <proj>/esc/etc/new.conf` with `esc -> /` was Allow — the file does not
/// exist (recoverability says `NothingToDestroy`) and the text names no system
/// directory, yet the kernel creates `/etc/new.conf`. The literal spelling of
/// the same file is Deny.
///
/// The landing is the same resolver the protected-path axis uses, so a FINAL
/// file symlink whose target does not exist yet is followed too (`sysln ->
/// /etc/<absent>`, and chains of such links): a write through it creates that
/// target. Resolving only the deepest existing ancestor (the previous
/// `canonicalize` walk) stopped at the dangling link and never saw `/etc`.
///
/// `Ok(None)` also covers a target that cannot be placed (an unexpanded
/// `$`/backtick, or relative with no known base) — the caller's existing rules
/// for those stand.
pub(super) fn real_system_path_block(
    action: &str,
    target: &str,
    ctx: &Ctx,
) -> Result<Option<Decision>, String> {
    match physical_landing(target, ctx.raw_base.as_deref()) {
        None => Ok(None),
        // macOS: the writable data volume is mounted at `/` through firmlinks,
        // and some of its symlinks (`/home -> /System/Volumes/Data/home`)
        // canonicalise to the volume's own path. That prefix is NOT the sealed
        // `/System`; the path below it is what the root view shows, so it is
        // judged as such (`/System/Volumes/Data/private/etc/x` is still
        // `/private/etc/x`, a system directory).
        Some(Determination::Known(real)) => {
            let rooted = match real.strip_prefix("/System/Volumes/Data/") {
                Some(rest) => format!("/{rest}"),
                None => real,
            };
            Ok(system_path_block(action, &rooted, ctx))
        }
        Some(Determination::Undetermined(u)) => Err(u.to_string()),
    }
}

/// Deny reason for an operation that DISARMS a protected gate/hook/policy path
/// without writing a single byte to it, or `None` when the target is not one.
///
/// Round 3. Every rule up to 0.2.18 asked "do bytes land ON this file?" — Write,
/// Edit, `>`/`>>`, `cp`/`mv`/`install`/`ln` DESTINATION, `sed -i`. A guard does
/// not have to be rewritten to be switched off; it only has to stop running.
/// Deleting it, moving it out of the directory the loader looks in, clearing its
/// exec bit, or restoring it from the index all leave the bytes untouched (or
/// remove them wholesale) and are invisible to a destination-only rule.
///
/// Kept as its own wording — and its own [`crate::rule_id`] id — rather than
/// folded into [`protected_path_block`], because "an agent WROTE to the file that
/// decides whether the gates run" and "an agent made that file stop being read"
/// are different recurring failures and want different signatures.
pub(super) fn protected_disarm_deny(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    if exclude::is_protected_path(path) && !ctx.in_worktree_checkout(path) {
        Some(Decision::deny(format!(
            "{action} disarms a protected gate/config path ({path}) without writing to it — \
it controls which hooks, gates or policies run, so blastguard refuses"
        )))
    } else {
        None
    }
}

/// Deny reason for an operation that destroys a whole DIRECTORY TREE holding
/// protected paths.
///
/// Separate from [`protected_disarm_deny`] because the target is the CONTAINER,
/// not the file: `.claude` matches no protected glob, yet `rm -rf .claude` takes
/// `.claude/settings.json` and every `.claude/hooks/*` with it. The verdict is a
/// Deny rather than an Ask because a recursive delete of a directory removes
/// EVERYTHING under it — the protected paths are a subset of what goes, so there
/// is nothing to guess about.
pub(super) fn protected_tree_deny(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    if (exclude::is_protected_path(path) || exclude::holds_protected_paths(path))
        && !ctx.in_worktree_checkout(path)
    {
        Some(Decision::deny(format!(
            "{action} destroys a directory tree holding protected gate/config paths ({path}) — \
it controls which hooks, gates or policies run, so blastguard refuses"
        )))
    } else {
        None
    }
}

/// Verdict for a non-recursive `chown`/`chgrp` whose target is, holds, or globs
/// into a protected gate/config path.
///
/// **`Ask`, not `Deny`, and that is the whole point of the rule.** Whether
/// re-owning a gate file actually disarms it is not derivable from the command
/// line: it depends on who the new owner is, on which account the loader runs
/// as, and on which loader it is (git refuses a repository whose ownership it
/// finds "dubious"; a `.claude/hooks/*` script is executed off its permission
/// bits, not its owner). A `Deny` would assert a disarm that has not been shown
/// — but `Allow` is not the neutral answer either, it is the OPPOSITE unbacked
/// claim, and it is the one this arm used to make.
/// [`crate::model::Decision::Ask`] is the answer that is true: per its own doc,
/// it "is NOT a verdict about the command, it is a refusal to guess about one".
///
/// This closes a MIRROR GAP with the `chmod` arm next door, which has classified
/// its targets since round 3 while this verb kept measuring blast radius (`-R`)
/// only — so `chown nobody .githooks/pre-commit` was a plain `Allow`.
pub(super) fn protected_reown_ask(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    if ctx.in_worktree_checkout(path) {
        return None;
    }
    let reaches = exclude::is_protected_path(path)
        || exclude::holds_protected_paths(path)
        || (has_glob_meta(path)
            && glob_literal_prefix(path)
                .map(|p| exclude::touches_protected(&p))
                .unwrap_or(false));
    if !reaches {
        return None;
    }
    Some(Decision::ask(format!(
        "{action} re-owns a protected gate/config path ({path}) — whether that disarms the \
gate depends on the new owner and on which account its loader runs as, neither of which is \
on the command line, so blastguard refuses to guess and asks"
    )))
}

/// Verdict for a RECURSIVE copy whose landing directory is, or holds, protected
/// paths.
///
/// The two cases are genuinely different questions and get different answers:
///
///   * the landing directory IS protected (`.claude/hooks/`, `.githooks/`) —
///     every file the copy creates lands inside a protected tree, so this is a
///     positively recognised hazard: **Deny**;
///   * the landing directory merely HOLDS protected paths (`.claude/`) —
///     whether the copy overwrites `.claude/settings.json` depends on the
///     SOURCE tree's contents, which exist only on disk at run time and are not
///     derivable from the command line: **Ask**.
///
/// The second is the textbook case for the third answer (`model.rs`: "it is NOT
/// a verdict about the command, it is a refusal to guess about one"). It is not
/// a softer Deny: with no human in the loop `Decision::hardened` collapses it to
/// a Deny, so the restrictive resolution CLAUDE.md requires holds either way —
/// what `Ask` buys is that the two situations stay distinguishable downstream
/// instead of both being recorded as "blastguard knows this is bad".
pub(super) fn protected_landing_block(ctx: &Ctx<'_>, action: &str, dir: &str) -> Option<Decision> {
    if ctx.in_worktree_checkout(dir) {
        return None;
    }
    if exclude::is_protected_path(dir) {
        return protected_path_block(ctx, action, dir);
    }
    if exclude::holds_protected_paths(dir) {
        return Some(Decision::ask(format!(
            "{action} lands a whole tree in {dir}, which holds protected gate/config paths — \
blastguard cannot tell from the command line which files the copy creates there, and refuses \
to guess"
        )));
    }
    None
}

/// The literal directory prefix of a wildcard operand: the components before
/// the first component that carries a glob metacharacter. `None` when the very
/// first component is already a pattern (`*`, `*.rs`) — there is nothing literal
/// left to judge, so the caller must not pretend it learned anything.
pub(super) fn glob_literal_prefix(tok: &str) -> Option<String> {
    let mut literal: Vec<&str> = Vec::new();
    for comp in tok.split('/') {
        if has_glob_meta(comp) {
            break;
        }
        literal.push(comp);
    }
    if literal.is_empty() {
        return None;
    }
    Some(literal.join("/"))
}

/// Deny reason for a WILDCARD operand aimed into a protected tree, or `None`.
///
/// Round 2 (adversarial verifier). `analyze_rm` skips glob operands before the
/// protected check — correctly, because a token containing `*` is not a literal
/// path and matching it against the protected globs can only ever FAIL and
/// thereby look clean — and it can afford to, because it has a blanket
/// "wildcard rm denies" backstop underneath. The `chmod` arm and the `mv` source
/// loop copied the first half of that reasoning and not the second: they handed
/// the raw `*`-bearing token to the protected check, got the guaranteed
/// non-match, and fell through to Allow. `chmod 000 .claude/*`,
/// `chmod 644 .claude/*` and `mv .claude/* /tmp/` were all ALLOW on 0.2.19,
/// while `chmod -x .githooks/*` denied only by the accident that the pattern
/// `.githooks/**` glob-matches the literal token `.githooks/*`.
///
/// What is knowable about a wildcard without running the shell is its LITERAL
/// PREFIX, and that is enough: `.claude/*` cannot expand to anything outside
/// `.claude`, so if the prefix is (or holds) protected paths the expansion
/// reaches them. This is a Deny rather than an Ask because it is the same
/// verdict the equivalent `rm` shape already gets, and a wildcard consuming a
/// protected directory's children is a positively recognised hazard, not an
/// unanalysable construct.
pub(super) fn protected_glob_deny(action: &str, tok: &str) -> Option<Decision> {
    if !has_glob_meta(tok) {
        return None;
    }
    let prefix = glob_literal_prefix(tok)?;
    if exclude::touches_protected(&prefix) {
        Some(Decision::deny(format!(
            "{action} is a wildcard ({tok}) expanding inside a protected gate/config tree — \
it controls which hooks, gates or policies run, so blastguard refuses"
        )))
    } else {
        None
    }
}
