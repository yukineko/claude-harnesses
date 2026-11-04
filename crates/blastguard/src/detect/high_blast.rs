use super::{
    compound_views, git_subcommand_index, has_glob_meta, has_short, is_short_flag,
    normalized_command, upload_flag_operand, Decision, VerdictAcc,
};

/// The CONSERVATIVE "high-blast Ask tier": novel, high-blast-radius /
/// hard-to-reverse command shapes that no rule above recognises — verified by
/// running them through this module BEFORE this tier existed and observing
/// `Decision::Allow` (see `crates/blastguard/tests/high_blast_tier.rs`, whose
/// RED phase pins that measurement) — so they fall all the way through to a
/// silent Allow today. Folded into `detect_bash`'s accumulator immediately
/// before `acc.finish()`, mirroring the `acc.record(analyze_...egress(...))`
/// calls earlier in that function: an existing `Deny` anywhere on the line
/// still outranks this `Ask` (`Decision`/`VerdictAcc` ranking, `Deny > Ask >
/// Allow`), so nothing here can DOWNGRADE an already-denied command.
///
/// Covers categories (A) git force-push and (B) schemeless curl/wget upload
/// only. Category (C) — an outside-tree `rm`/`unlink` — is NOT scanned here:
/// judging whether an operand escapes the tree requires the SAME cwd
/// tracking (`CwdState`/`advance_cwd_and_rewrite`) `detect_bash`'s per-segment
/// loop already threads through `cd`/`pushd`, and re-deriving that from a
/// fresh top-level scan of the raw, un-rewritten command text would get it
/// wrong for exactly the case a pinned test guards
/// (`cwd_prefix_bypass_closure.rs`'s
/// `relative_operand_that_escapes_the_tracked_dir_stays_allowed`: `cd
/// .githooks && rm ../src/build.o` must stay `Allow`, because the `..` walks
/// back OUT of `.githooks` into an ordinary place — a naive "operand starts
/// with `../`" scan on the untouched segment cannot see that). See
/// `high_blast_outside_tree_rm`, called from the per-segment loop below on
/// the already-cwd-resolved `effective_seg` instead.
///
/// Every arm is deliberately narrow — this hook runs on EVERY Bash call, so
/// the primary risk of adding to it is over-blocking ordinary work, not
/// under-blocking a destructive one. See each helper's doc comment for the
/// exact boundary and what was measured to already be covered (left alone)
/// vs. genuinely uncovered (this tier's actual target).
pub(super) fn analyze_high_blast_tier(cmd: &str) -> Decision {
    let mut acc = VerdictAcc::default();
    // The command each segment runs with compound syntax removed, so
    // `then git push --force` is read as `git`, not as the keyword `then`.
    for view in compound_views(cmd) {
        let tokens: Vec<&str> = view.command.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let head = normalized_command(tokens[0]);
        let rest = &tokens[1..];
        let verdict = match head.as_str() {
            "git" => high_blast_git_force_push(rest),
            "curl" | "wget" => high_blast_schemeless_upload(&head, rest),
            _ => None,
        };
        if let Some(d) = verdict {
            if let Some(deny) = acc.record(d) {
                return deny;
            }
        }
    }
    acc.finish()
}

/// (A) `git push` carrying a history-rewrite force flag: `--force`, `-f`, or
/// `--force-with-lease` (bare or with an attached `=<refspec>`) — OR a
/// refspec argument whose source side begins with a literal `+` (`+main`,
/// `+refs/heads/x:refs/heads/x`), which forces that one ref exactly as the
/// flag forms do, just spelled without the flag (a pre-flag git idiom that
/// still works today). A NORMAL (non-force) `git push` only ever
/// fast-forwards the remote and never has a rule of its own (`analyze_git`'s
/// `match` has no `"push"` arm at all, so it falls to the `_ =>
/// Decision::Allow` catch-all) — measured: `git push`, `git push origin
/// main` are `Allow` both before and after this tier. Force-pushing can
/// overwrite or discard commits already on the remote, i.e. rewrite shared
/// history, which is exactly the class of hard-to-reverse operation this
/// tier exists to catch.
///
/// `--force-if-includes` is deliberately NOT treated as a force flag by
/// itself: it is a SAFETY modifier that only takes effect alongside
/// `--force`/`--force-with-lease` (it makes the force conditional on having
/// seen the remote tip), so a line carrying it alone is not a force push.
///
/// The refspec scan is bound tightly to just the leading-`+` source-side
/// shape: [`refspec_source_is_force`] only inspects non-flag tokens (so
/// `-u`/`--set-upstream` and its operand, a plain remote name, a plain
/// branch/tag name, and a non-force colon refspec like `HEAD:main` all stay
/// untouched) — see that helper's doc comment for the exact boundary.
fn high_blast_git_force_push(rest: &[&str]) -> Option<Decision> {
    let idx = git_subcommand_index(rest)?;
    if normalized_command(rest[idx]) != "push" {
        return None;
    }
    let tail = &rest[idx + 1..];
    let forces = tail.iter().any(|t| {
        *t == "--force" || *t == "--force-with-lease" || t.starts_with("--force-with-lease=")
    }) || has_short(tail, 'f');
    if forces {
        return Some(Decision::ask(
            "`git push` with --force/-f/--force-with-lease can overwrite or discard commits \
already on the remote — this rewrites shared history",
        ));
    }
    if let Some(refspec) = tail.iter().find(|t| refspec_source_is_force(t)) {
        return Some(Decision::ask(format!(
            "`git push` refspec `{refspec}` has a leading `+` on its source side, which forces \
that ref exactly like --force/-f — this rewrites shared history"
        )));
    }
    None
}

/// True if `tok` is a non-flag `git push` argument whose refspec SOURCE side
/// (the part before the first `:`, or the whole token if there is no `:`)
/// begins with a literal `+`. That `+` is git's own force-refspec syntax
/// (`git push origin +main`, `git push origin +src:dst`) — it forces the
/// update of that one ref exactly as `--force`/`-f` does for the whole
/// invocation, just spelled on the refspec instead of as a flag.
///
/// Deliberately does NOT match: any `-`-prefixed token (flags, including
/// `-u`/`--set-upstream` and any operand a caller passed to a value-taking
/// flag — this function is only ever called on ALREADY-non-flag tokens by
/// its one call site, but the guard is kept here too so the boundary is
/// self-evident from the helper alone); a plain remote/branch/tag name
/// (`main`, `v1.0`, `origin` — no leading `+`); or a colon refspec whose
/// source side does NOT start with `+` (`HEAD:main`, `src:dst` — measured:
/// these are the exact non-force colon forms this tier must leave `Allow`).
fn refspec_source_is_force(tok: &str) -> bool {
    if tok.starts_with('-') {
        return false;
    }
    let source = tok.split(':').next().unwrap_or(tok);
    source.starts_with('+')
}

/// (B) `curl`/`wget` uploading local file content ([`upload_flag_operand`]'s
/// upload-flag shapes: `-d @file`/`--data-binary @file`/`-F name=@file`/
/// `-T file`/`--upload-file file`, …) to a URL with NO literal `://` scheme.
/// `fetch_exfil_upload` already denies this exact upload shape when a scheme
/// IS present; curl and wget both default a bare `host/path` operand to
/// `http://`, so the upload still happens — measured: `curl -d @data.json
/// example.com/upload` is `Allow` today (the scheme-gated `fetch_exfil_upload`
/// never even looks at the flags), while the scheme-carrying twin
/// `curl -d @data.json https://example.com/upload` is already `Deny`.
///
/// Gated explicitly on the ABSENCE of a `://` token (rather than relying on
/// `fetch_exfil_upload`'s Deny having already fired and short-circuited
/// before this tier runs) so the emitted reason is never wrong about which
/// case applies.
fn high_blast_schemeless_upload(cmd: &str, rest: &[&str]) -> Option<Decision> {
    if rest.iter().any(|t| t.contains("://")) {
        return None;
    }
    let op = upload_flag_operand(rest)?;
    Some(Decision::ask(format!(
        "`{cmd}` uploads `{op}` (a file reference) to a URL with no explicit scheme — this is \
the same upload shape denied when a scheme is present, and curl/wget both default an \
unqualified host/path to http://"
    )))
}

/// (C) a plain, non-recursive, non-wildcard `rm`/`unlink` of a single operand
/// that climbs OUT of the tree via a leading `../` — with NO preceding
/// `cd`/`pushd` (or one whose target could not be resolved) to explain where
/// that `..` actually lands.
///
/// `seg` must be the CWD-RESOLVED segment (`effective_seg` from
/// `detect_bash`'s per-segment loop, i.e. after `advance_cwd_and_rewrite`),
/// not the raw one — see `analyze_high_blast_tier`'s doc comment for why:
/// `rewrite_relative_operands` already collapses a `../` that a tracked `cd`
/// resolves back into an ordinary place (`cd .githooks && rm ../src/build.o`
/// rewrites to `rm src/build.o`, no leading `..` left to trip this), so
/// checking the resolved segment gets that case right for free instead of
/// re-deriving it. `CwdState::Root` (no `cd` yet) and `CwdState::Unknown`
/// (unresolvable `cd`) both leave the segment untouched, so a bare
/// `rm ../sibling/secret.env` with no explaining context still reads its own
/// literal `..`.
///
/// Deliberately does NOT trigger on an absolute path (`rm /etc/passwd`,
/// `rm /tmp/scratch.txt`): `analyze_rm`'s own non-regression test
/// (`ordinary_single_file_rm_stays_allowed`) pins that a single, non-recursive
/// `rm` of ANY named file — including an absolute one — stays `Allow` unless
/// the target is a recognised PROTECTED path; that is this crate's existing,
/// tested design for `rm`, not a gap this task discovered, and widening past
/// it here would directly conflict with that pin (measured: adding an
/// absolute-path arm broke `ordinary_single_file_rm_stays_allowed` on
/// `rm /tmp/scratch.txt`, which is ordinary scratch-file cleanup, not a novel
/// hazard).
///
/// Measured, not assumed, that `rm -rf`/wildcard forms need no exclusion
/// here at all: `rm -rf <anything>` is ALREADY an unconditional `Deny`
/// regardless of the target's location (`analyze_rm`'s destructive-shape
/// branch has no location check; see `denies_recursive_and_wildcard_rm`), so
/// that Deny always short-circuits `detect_bash` before this tier ever runs
/// — the recursive/wildcard guard below exists only so this function's own
/// verdict is never wrong in isolation, not to prevent a double-count.
pub(super) fn high_blast_outside_tree_rm(seg: &str) -> Option<Decision> {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let (head_tok, rest) = tokens.split_first()?;
    let cmd = normalized_command(head_tok);
    if cmd != "rm" && cmd != "unlink" {
        return None;
    }
    if cmd == "rm" {
        let recursive = rest.iter().any(|t| {
            (is_short_flag(t) && (t.contains('r') || t.contains('R'))) || *t == "--recursive"
        });
        let dir_flag = rest
            .iter()
            .any(|t| (is_short_flag(t) && t.contains('d')) || *t == "--dir");
        let wildcard = rest
            .iter()
            .filter(|t| !t.starts_with('-'))
            .any(|o| has_glob_meta(o));
        if recursive || dir_flag || wildcard {
            // Already fully covered (Deny), unconditionally, by `analyze_rm`.
            return None;
        }
    }
    for operand in rest.iter().filter(|t| !t.starts_with('-')) {
        if operand.starts_with("../") || *operand == ".." {
            return Some(Decision::ask(format!(
                "`{cmd} {operand}` deletes a file outside the current tree, with no earlier cd \
to explain where `..` lands — blastguard cannot confirm the target and refuses to guess"
            )));
        }
    }
    None
}
