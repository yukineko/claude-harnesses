//! The Q3/Q4 deny-target table (user rulings 2026-11-14, design approved
//! 2026-11-19; backlog 7275b9d9). Spec: `SUBSET.md` "Deny-target table".
//!
//! ONE table of path patterns, matched ONLY against path operand VALUES the
//! AST evaluator resolved (`Value::Literal` of a Table B B.1 target, after
//! [`super::super::eval::resolve_at`], and of a writing redirect) — never
//! against the raw or a reconstructed command text. An S5 Python `Fs` effect
//! reaches it through its shell spelling (`os.replace(a, b)` is judged as
//! `mv -- 'a' 'b'`), so the same table serves both; there is no second copy.
//!
//! RAISE-ONLY: [`verdict`] answers `Deny` (a target matched), `Ask` (the
//! table could not be loaded and a write target exists, a B.1 target value
//! is `Undetermined` and nothing matched — Table B asks about the same value
//! in the same pass — or the judge panicked,
//! `super::super::deny_target_verdict`), or `Allow`, which here
//! means "this layer has no opinion"; the caller folds it with
//! `Deny > Ask > Allow`, so no entry can ever lower another layer's verdict.
//!
//! What it judges, per target: every Table B B.1 effect (delete, the `mv`
//! source, `cp` / `mv` / `install` destination, `chmod` / `chown` / `chgrp`,
//! `ln` link name, `tee`, `truncate`, `dd of=`, `sed -i`, `find -delete`) and
//! the redirects `>` `>|` `>>` `&>` `&>>` `<>` and `>&` with a path. Reads
//! (`cat`, `<`, a `cp` source, Python `open(.., 'r')`) are not targets and
//! are never matched.
//!
//! `chmod` (user ruling 2026-11-20 「書込み Deny・exec 付与は許可」): on a Q4
//! gate-script row (`exec_add_allowed`) a `chmod` that ONLY adds exec bits
//! (`+x`, `u+x`, `a+x`, `ug+x`, octal `755` / `775`, no option such as `-R`;
//! [`chmod_adds_exec_only`]) is not raised and keeps today's verdict. Every
//! other mode change (`777`, `o+w`, `-x`, `000`, `-R …`) on any row, and any
//! `chmod` on a Q3 row, is Deny.
//!
//! Matching is by path COMPONENT (globset with `literal_separator`, so `*`
//! never crosses `/`) and case-insensitive (macOS default filesystems fold
//! case, and folding can only match more): `/tmp/sshx/f` and
//! `/tmp/notscripts/check-x.txt` do not match. Each path is matched in two
//! spellings, as written (with `//`, `/./`, a leading `./` and a trailing `/`
//! collapsed) and with `..` resolved lexically; a match in EITHER counts.
//! A relative value is also joined to the session cwd when no `cd` applies.
//!
//! Exemptions: none of the session safe roots, scratch / temp roots or the
//! cache roots exempt an entry (the ruling). The ONE location that does not
//! raise is the existing protected-path lift (user ruling 2026-10-03): a
//! repository-relative gate entry (`lift_in_checkout`: `.githooks`,
//! `scripts/check-*.py`, `crates/*/hooks`) strictly inside a linked worktree
//! checkout ([`SafeRoots::classify_worktree_checkout_value`] `Inside`) is that
//! checkout's own tracked file. The Q3 entries and the plugin cache are never
//! lifted. An undetermined placement is not lifted.
//!
//! NOT judged here (stated, not hidden; the legacy layers keep judging them):
//! which row an `Undetermined` target value names (it is an Ask, above), an
//! `Undetermined` redirect target (B.2 is the legacy layer's), a
//! command under an unmodelled wrapper (`sudo`, `env`, `command`, …), a
//! command line whose analysis is `Undetermined`, a glob whose expansion
//! might name an entry (`/x/.ss*`; the spelling is matched as written), the
//! symlink-resolved landing of a target, and a recursive delete of an
//! ANCESTOR of an entry other than the entry directories themselves
//! (`rm -rf <repo>/scripts`).

use std::sync::OnceLock;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use harness_core::verdict::Determination;

use super::super::{RedirOp, RedirTarget, Script, SimpleCommand, Value};
use super::{table_b, Effect};
use crate::model::Decision;
use crate::scope::{Placement, SafeRoots};

/// Phrase every deny-target Deny carries ([`crate::rule_id`] keys
/// `ast-deny-target` on it).
pub(crate) const DENY_TARGET: &str = "is on blastguard's Q3/Q4 deny-target table";

/// Which effects an [`Entry`] denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effects {
    /// Every write effect.
    All,
    /// The home directory itself: the effects that act on the directory
    /// entry (delete / `mv` away, mode change, link replacement). Creating a
    /// file INSIDE it (`cp f ~`, a `mv` destination) is not an effect on the
    /// directory, and stays with the other layers.
    OnEntry,
}

/// One row of the table.
struct Entry {
    /// Shown in the reason.
    what: &'static str,
    /// The user ruling the row encodes.
    ruling: &'static str,
    patterns: &'static [&'static str],
    effects: Effects,
    /// Not raised strictly inside a linked worktree checkout (see the module
    /// doc).
    lift_in_checkout: bool,
    /// A `chmod` that ONLY adds exec bits ([`chmod_adds_exec_only`]) is not
    /// raised (user ruling 2026-11-20, Q4 gate scripts only).
    exec_add_allowed: bool,
}

/// The table. Adding a row extends the shell judge and the S5 Python judge
/// at once.
const TABLE: &[Entry] = &[
    Entry {
        what: "the home directory itself (`/Users/<n>`, `/home/<n>`)",
        ruling: "Q3, user ruling 2026-11-14",
        patterns: &["/Users/*", "/home/*"],
        effects: Effects::OnEntry,
        lift_in_checkout: false,
        exec_add_allowed: false,
    },
    Entry {
        what: "an `.ssh` directory or a file in one",
        ruling: "Q3, user ruling 2026-11-14 (protected even outside $HOME)",
        patterns: &["**/.ssh", "**/.ssh/**"],
        effects: Effects::All,
        lift_in_checkout: false,
        exec_add_allowed: false,
    },
    Entry {
        what: "a `.gitconfig`",
        ruling: "Q3, user ruling 2026-11-14 (protected even outside $HOME)",
        patterns: &["**/.gitconfig"],
        effects: Effects::All,
        lift_in_checkout: false,
        exec_add_allowed: false,
    },
    Entry {
        what: "a gate script in `.githooks/`",
        ruling: "Q4, user ruling 2026-11-14",
        patterns: &["**/.githooks", "**/.githooks/**"],
        effects: Effects::All,
        lift_in_checkout: true,
        exec_add_allowed: true,
    },
    Entry {
        what: "a gate script `scripts/check-*.py`",
        ruling: "Q4, user ruling 2026-11-14",
        patterns: &["**/scripts/check-*.py"],
        effects: Effects::All,
        lift_in_checkout: true,
        exec_add_allowed: true,
    },
    Entry {
        what: "a plugin hook in `crates/*/hooks/`",
        ruling: "Q4, user ruling 2026-11-14",
        patterns: &["**/crates/*/hooks", "**/crates/*/hooks/**"],
        effects: Effects::All,
        lift_in_checkout: true,
        exec_add_allowed: true,
    },
    Entry {
        what: "a deployed plugin `bin/` or `hooks/` under `.claude/plugins/cache/`",
        ruling: "Q4, user ruling 2026-11-14",
        patterns: &[
            "**/.claude/plugins/cache/**/bin",
            "**/.claude/plugins/cache/**/bin/**",
            "**/.claude/plugins/cache/**/hooks",
            "**/.claude/plugins/cache/**/hooks/**",
        ],
        effects: Effects::All,
        lift_in_checkout: false,
        exec_add_allowed: true,
    },
];

/// The compiled table: one glob set, and for each of its patterns the index
/// of its [`TABLE`] row.
struct Compiled {
    set: GlobSet,
    row_of: Vec<usize>,
}

fn compile() -> Determination<Compiled> {
    let mut b = GlobSetBuilder::new();
    let mut row_of = Vec::new();
    for (row, e) in TABLE.iter().enumerate() {
        for pat in e.patterns {
            match GlobBuilder::new(pat)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
            {
                Ok(g) => {
                    b.add(g);
                    row_of.push(row);
                }
                Err(err) => {
                    return Determination::undetermined(format!(
                        "deny-target pattern `{pat}` does not compile ({err})"
                    ))
                }
            }
        }
    }
    match b.build() {
        Ok(set) => Determination::known(Compiled { set, row_of }),
        Err(err) => Determination::undetermined(format!(
            "the deny-target pattern set would not build ({err})"
        )),
    }
}

fn compiled() -> &'static Determination<Compiled> {
    static SET: OnceLock<Determination<Compiled>> = OnceLock::new();
    SET.get_or_init(compile)
}

/// `p` with the POSIX no-ops collapsed: `//`, `/./`, a leading `./`, a
/// trailing `/` (not of `/` itself).
fn collapse(p: &str) -> String {
    let mut s = p.to_string();
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    while s.contains("/./") {
        s = s.replace("/./", "/");
    }
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_suffix("/.") {
        s = if rest.is_empty() {
            "/".into()
        } else {
            rest.into()
        };
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// `p` (collapsed) with `..` resolved lexically; a `..` that would escape a
/// relative path is kept, at the root of an absolute path it is dropped.
fn resolve_parents(p: &str) -> String {
    if !p.split('/').any(|c| c == "..") {
        return p.to_string();
    }
    let absolute = p.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => match stack.last() {
                Some(last) if *last != ".." => {
                    stack.pop();
                }
                None if absolute => {}
                _ => stack.push(".."),
            },
            _ => stack.push(c),
        }
    }
    let joined = stack.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Is `effect` one `effects` denies?
fn denies(effects: Effects, effect: Effect) -> bool {
    match effects {
        Effects::All => true,
        Effects::OnEntry => matches!(
            effect,
            Effect::Delete { .. } | Effect::Metadata | Effect::Link
        ),
    }
}

/// What a redirect does to its path, or `None` for a read / non-path target.
fn redirect_effect(op: RedirOp) -> Option<Effect> {
    match op {
        RedirOp::Out | RedirOp::Clobber | RedirOp::OutErr | RedirOp::DupOut => {
            Some(Effect::Truncate)
        }
        RedirOp::Append | RedirOp::AppendOutErr => Some(Effect::Append),
        RedirOp::InOut => Some(Effect::Rewrite),
        RedirOp::In | RedirOp::DupIn | RedirOp::HereDoc | RedirOp::HereString => None,
    }
}

/// One write target: its effect, its resolved literal path, and whether it is
/// the operand of a `chmod` that only adds exec bits
/// ([`chmod_adds_exec_only`]).
type Target = (Effect, String, bool);

/// True when `c` is `chmod MODE [--] FILE…` whose MODE only ADDS exec bits
/// (user ruling 2026-11-20 「書込み Deny・exec 付与は許可」): every word is a
/// literal, no option other than a lone `--` (so `-R`, `-v`, `--reference`
/// are not), and MODE is either symbolic clauses `[ugoa]*+x…` joined by `,`
/// (`+x`, `u+x`, `a+x`, `ug+x`; not `+X`, `=`, `-`, or any other permission
/// letter), or the octal install modes `755` / `775` (leading zeros allowed).
/// Anything else is a mode change the deny-target table raises.
fn chmod_adds_exec_only(c: &SimpleCommand) -> bool {
    let mut words = Vec::with_capacity(c.argv.len());
    for w in &c.argv {
        let Value::Literal(v) = &w.value else {
            return false;
        };
        words.push(v.as_str());
    }
    let Some((&name, rest)) = words.split_first() else {
        return false;
    };
    if name.rsplit('/').next() != Some("chmod") {
        return false;
    }
    let rest: Vec<&str> = rest.iter().copied().filter(|w| *w != "--").collect();
    let Some((&mode, files)) = rest.split_first() else {
        return false;
    };
    if files.is_empty() || files.iter().any(|f| f.starts_with('-')) {
        return false;
    }
    mode_adds_exec_only(mode)
}

/// The MODE half of [`chmod_adds_exec_only`].
fn mode_adds_exec_only(mode: &str) -> bool {
    if !mode.is_empty() && mode.bytes().all(|b| b.is_ascii_digit()) {
        let digits = mode.trim_start_matches('0');
        return digits == "755" || digits == "775";
    }
    !mode.is_empty()
        && mode.split(',').all(|clause| {
            let Some((who, perms)) = clause.split_once('+') else {
                return false;
            };
            who.bytes().all(|b| matches!(b, b'u' | b'g' | b'o' | b'a'))
                && !perms.is_empty()
                && perms.bytes().all(|b| b == b'x')
        })
}

/// Every write target of `c` (see [`Target`]).
///
/// A B.1 target VALUE that is `Undetermined` (it may name a row) is pushed
/// to `undetermined` with its reason, and [`verdict`] answers Ask for it
/// unless a row already denies. Table B (`table_b_verdict`) asks about the
/// same value in the same pass, so this adds no new Ask; it keeps this layer
/// from reading "could not place" as "no target".
fn write_targets(c: &SimpleCommand, undetermined: &mut Vec<String>) -> Vec<Target> {
    let mut out = Vec::new();
    let exec_only = chmod_adds_exec_only(c);
    if let table_b::Row::Targets(t) = table_b::extract(c) {
        for (effect, value) in t {
            let exec_only = exec_only && effect == Effect::Metadata;
            let written = value.clone();
            match (super::super::eval::resolve_at(&c.cwd, value), written) {
                (Value::Literal(p), _) => out.push((effect, p, exec_only)),
                // A relative path whose cwd is not one known directory: the
                // table's `**/` rows place it anywhere, so its spelling is
                // still matched (Table B asks about the placement itself).
                (Value::Undetermined(_), Value::Literal(p)) => out.push((effect, p, exec_only)),
                (Value::Undetermined(why), Value::Undetermined(_)) => {
                    undetermined.push(format!("{effect:?} target cannot be placed: {why}"));
                }
            }
        }
    }
    for r in &c.redirects {
        if let (Some(effect), RedirTarget::Path(Value::Literal(p))) =
            (redirect_effect(r.op), &r.target)
        {
            out.push((effect, p.clone(), false));
        }
    }
    out
}

/// The spellings of `path` the table is matched against (see the module
/// doc), and the absolute one the worktree lift is placed by, if any.
fn spellings(path: &str, session_cwd: Option<&str>) -> (Vec<String>, Option<String>) {
    let mut v = Vec::new();
    let collapsed = collapse(path);
    let resolved = resolve_parents(&collapsed);
    let absolute = if collapsed.starts_with('/') {
        Some(collapsed.clone())
    } else {
        session_cwd
            .filter(|d| d.starts_with('/'))
            .map(|d| collapse(&format!("{d}/{collapsed}")))
    };
    v.push(collapsed);
    v.push(resolved);
    if let Some(a) = &absolute {
        v.push(a.clone());
        v.push(resolve_parents(a));
    }
    v.dedup();
    (v, absolute)
}

/// True when `path` (as [`verdict`] spells it) matches ANY row of the table
/// with the worktree lift NOT applied, whatever the effect — or the table
/// cannot be loaded (fail-closed). For `judge::names_command_running_config`
/// (stage S6-4, user ruling 2026-11-20 「Deny のまま（推奨）」); it raises
/// nothing itself.
pub(crate) fn matches_any_row_unlifted(path: &str, session_cwd: Option<&str>) -> bool {
    let Determination::Known(table) = compiled() else {
        return true;
    };
    let (spelled, _) = spellings(path, session_cwd);
    spelled.iter().any(|s| table.set.is_match(s))
}

/// True when a file directly in the directory `parent` whose name matches
/// the final-component glob `last` COULD match a row of the table, lift not
/// applied (backlog 2e704124, for `judge::names_command_running_config`):
/// for some pattern `<pp>/<pl>`, a spelling of `parent` ([`spellings`])
/// matches `<pp>` and `last` may intersect `<pl>` ([`globs_may_intersect`];
/// `**` intersects every name), or `parent` itself matches a pattern ending
/// in `/**`. Over-approximates (fail-closed): a pattern or a spelling that
/// does not compile, or a table that cannot be loaded, is `true`.
pub(crate) fn may_match_child_unlifted(
    parent: &str,
    last: &str,
    session_cwd: Option<&str>,
) -> bool {
    if !matches!(compiled(), Determination::Known(_)) {
        return true;
    }
    let (spelled, _) = spellings(if parent.is_empty() { "/" } else { parent }, session_cwd);
    for pat in TABLE.iter().flat_map(|e| e.patterns.iter()) {
        let Some((pp, pl)) = pat.rsplit_once('/') else {
            return true;
        };
        let pp = if pp.is_empty() { "/" } else { pp };
        let matcher = |g: &str| {
            GlobBuilder::new(g)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
                .map(|g| g.compile_matcher())
        };
        let (Ok(dir), Ok(whole)) = (matcher(pp), matcher(pat)) else {
            return true;
        };
        let child = pl == "**" || globs_may_intersect(last, pl);
        if spelled
            .iter()
            .any(|s| (child && dir.is_match(s)) || (pl == "**" && whole.is_match(s)))
        {
            return true;
        }
    }
    false
}

/// True when some file name may match both of the final-component globs `a`
/// and `b` (`*`, `?`, `[…]`; case-insensitive). Exact when either has no
/// metacharacter (the other is matched against it); otherwise only a
/// literal prefix or suffix that cannot agree proves them disjoint, and
/// anything else is `true` (fail-closed). A glob that does not compile is
/// `true`.
pub(crate) fn globs_may_intersect(a: &str, b: &str) -> bool {
    const META: [char; 3] = ['*', '?', '['];
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let matches = |glob: &str, name: &str| {
        GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .map_or(true, |g| g.compile_matcher().is_match(name))
    };
    if !b.contains(META) {
        return matches(&a, &b);
    }
    if !a.contains(META) {
        return matches(&b, &a);
    }
    let prefix = |s: &str| s[..s.find(META).unwrap_or(s.len())].to_string();
    let suffix = |s: &str| s[s.rfind(['*', '?', '[', ']']).map_or(0, |i| i + 1)..].to_string();
    let (pa, pb, sa, sb) = (prefix(&a), prefix(&b), suffix(&a), suffix(&b));
    (pa.starts_with(&pb) || pb.starts_with(&pa)) && (sa.ends_with(&sb) || sb.ends_with(&sa))
}

/// True when `abs` is strictly inside a linked worktree checkout.
fn in_linked_checkout(scope: &SafeRoots, abs: Option<&str>) -> bool {
    let Some(abs) = abs else { return false };
    matches!(
        scope.classify_worktree_checkout_value(abs),
        Determination::Known(Placement::Inside { .. })
    )
}

/// The deny-target verdict of `script` (see the module doc).
pub(crate) fn verdict(script: &Script, scope: &SafeRoots) -> Decision {
    let mut targets = Vec::new();
    let mut undetermined = Vec::new();
    for c in &script.commands {
        targets.extend(write_targets(c, &mut undetermined));
    }
    if targets.is_empty() && undetermined.is_empty() {
        return Decision::Allow;
    }
    let table = match compiled() {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => {
            return Decision::ask(format!(
                "blastguard could not load its deny-target table ({why}), so it cannot tell \
                 whether this command writes a protected path and asks rather than guess"
            ))
        }
    };
    let mut reasons = Vec::new();
    for (effect, path, exec_only) in &targets {
        let (spelled, absolute) = spellings(path, scope.session_cwd());
        let mut rows: Vec<usize> = spelled
            .iter()
            .flat_map(|s| table.set.matches(s))
            .map(|i| table.row_of[i])
            .collect();
        rows.sort_unstable();
        rows.dedup();
        for row in rows {
            let e = &TABLE[row];
            if !denies(e.effects, *effect) {
                continue;
            }
            if *exec_only && e.exec_add_allowed {
                continue;
            }
            if e.lift_in_checkout && in_linked_checkout(scope, absolute.as_deref()) {
                continue;
            }
            reasons.push(format!(
                "{effect:?} of `{path}`: {} {DENY_TARGET} ({}); writing, moving, replacing, \
                 chmod-ing (other than only adding exec bits to a Q4 gate script), deleting \
                 or relinking it is refused, reading it is not",
                e.what, e.ruling
            ));
        }
    }
    if !reasons.is_empty() {
        reasons.dedup();
        Decision::deny(reasons.join("; "))
    } else if !undetermined.is_empty() {
        Decision::ask(format!(
            "{} — so blastguard cannot tell whether it is on its Q3/Q4 deny-target table and \
             asks rather than guess",
            undetermined.join("; ")
        ))
    } else {
        Decision::Allow
    }
}

#[cfg(test)]
#[allow(clippy::panic, reason = "test assertions")]
mod tests {
    use super::*;

    fn rows(p: &str) -> Vec<&'static str> {
        let Determination::Known(t) = compiled() else {
            panic!("table did not load")
        };
        let (s, _) = spellings(p, None);
        let mut r: Vec<usize> = s
            .iter()
            .flat_map(|x| t.set.matches(x))
            .map(|i| t.row_of[i])
            .collect();
        r.sort_unstable();
        r.dedup();
        r.into_iter().map(|i| TABLE[i].what).collect()
    }

    #[test]
    fn the_table_compiles_in_full() {
        let Determination::Known(t) = compiled() else {
            panic!("deny-target table did not compile")
        };
        let n: usize = TABLE.iter().map(|e| e.patterns.len()).sum();
        assert_eq!(t.row_of.len(), n);
    }

    #[test]
    fn entries_match_by_component_not_substring() {
        for hit in [
            "/Users/somebody",
            "/home/somebody/",
            "/Users/somebody/.ssh/authorized_keys",
            "/x/anydir/.ssh/id_rsa",
            "/x/anydir/.ssh",
            "/x/anydir/.gitconfig",
            "/r/.githooks/pre-commit",
            ".githooks",
            "/r/scripts/check-worktree-isolation.py",
            "scripts/check-x.py",
            "/r/crates/foo/hooks/hook.sh",
            "/h/.claude/plugins/cache/x/y/1.0/bin/tool",
            "/h/.claude/plugins/cache/x/y/1.0/hooks",
            "/r/a/../.ssh/k",
            "/r//scripts/./check-a.py",
            "/R/.SSH/k",
        ] {
            assert!(!rows(hit).is_empty(), "`{hit}` must match a row");
        }
        for miss in [
            "/tmp/sshx/f",
            "/tmp/x.ssh/f",
            "/tmp/notscripts/check-x.txt",
            "/r/scripts/check-x.sh",
            "/r/scripts/sub/check-x.py",
            "/r/crates/foo/src/hooks.rs",
            "/r/crates/foo/bar/hooks/x",
            "/Users",
            "/Users/somebody/Desktop/f",
            "/tmp/.gitconfig.bak",
            "/h/.claude/plugins/cache/x/y/1.0/skills/s.md",
        ] {
            assert!(
                rows(miss).is_empty(),
                "`{miss}` must not match: {:?}",
                rows(miss)
            );
        }
    }

    #[test]
    fn the_home_directory_row_denies_only_effects_on_the_entry() {
        assert!(denies(Effects::OnEntry, Effect::Delete { recursive: true }));
        assert!(denies(Effects::OnEntry, Effect::Metadata));
        assert!(denies(Effects::OnEntry, Effect::Link));
        assert!(!denies(Effects::OnEntry, Effect::Truncate));
        assert!(denies(Effects::All, Effect::Truncate));
    }

    #[test]
    fn only_exec_adding_modes_are_exec_only() {
        for m in ["+x", "u+x", "a+x", "ug+x", "u+x,g+x", "755", "0755", "775"] {
            assert!(mode_adds_exec_only(m), "`{m}` only adds exec");
        }
        for m in [
            "777", "o+w", "-x", "000", "+X", "u=x", "+rx", "644", "700", "", "u+", "+x,o+w",
        ] {
            assert!(!mode_adds_exec_only(m), "`{m}` is not exec-only");
        }
    }

    #[test]
    fn reads_are_not_redirect_targets() {
        assert_eq!(redirect_effect(RedirOp::In), None);
        assert_eq!(redirect_effect(RedirOp::HereDoc), None);
        assert_eq!(redirect_effect(RedirOp::Out), Some(Effect::Truncate));
        assert_eq!(redirect_effect(RedirOp::Append), Some(Effect::Append));
    }
}
