mod claim_ledger;
mod config;
mod divergence;
mod driver;
mod github;
mod hooks;
mod install;
mod liveness;
mod lock;
mod store;
mod task;

use anyhow::Result;
use clap::{Parser, Subcommand};
use harness_core::boundary;
use harness_core::hook::{read_stdin, run_hook, HookInput};
use harness_core::verdict::{Determination, Required};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "backlog", about = "Cross-project task queue for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a new task to the backlog
    Add {
        /// Task title
        #[arg(long)]
        title: String,

        /// Project path
        #[arg(long)]
        project: String,

        /// Tags (can be specified multiple times)
        #[arg(long = "tag", action = clap::ArgAction::Append)]
        tags: Vec<String>,

        /// Priority shortcut: p0, p1, or p2 (added as a tag)
        #[arg(long)]
        priority: Option<String>,

        /// Notes
        #[arg(long, default_value = "")]
        notes: String,

        /// Ordering weight (higher surfaces first within a priority tier).
        /// Supply a compass opportunity's weight here so the queue order tracks
        /// opportunity impact, not just priority + insertion time. Default 0.0
        /// preserves the legacy (priority, created_at) order.
        #[arg(long, default_value_t = 0.0)]
        weight: f64,

        /// Bypass the duplicate-content guard: add even when an existing
        /// pending/failed task, or a live cross-session claim, already holds
        /// this title+project's content hashkey.
        #[arg(long)]
        force: bool,
    },

    /// List tasks
    List {
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,

        /// Filter by project path
        #[arg(long)]
        project: Option<String>,

        /// Filter by status: pending | done | failed (NB: not "open" — that is
        /// hypothesis's vocabulary, a different binary)
        #[arg(long)]
        status: Option<String>,

        /// Emit the matching tasks as a JSON array (for machine consumers like
        /// autoflow) instead of the human table.
        #[arg(long)]
        json: bool,

        /// Return every project IN THIS STORE, not just the cwd-resolved one.
        /// The store is per repo (`<root>/.backlog/tasks.toml`), so this is
        /// NOT a cross-repo search: tasks filed against a different repo live
        /// in that repo's own file and there is no index to walk. Ignored if
        /// `--project` is also given (`--project` wins, not a union).
        #[arg(long)]
        all: bool,
    },

    /// Show the next highest-priority pending task. Scoped, by default, to the
    /// project this checkout resolves to — the same default `list` uses.
    Next {
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,

        /// Filter by project path
        #[arg(long)]
        project: Option<String>,

        /// Rank over every project IN THIS STORE, not just the cwd-resolved
        /// one. Same meaning as `list --all`: the store is per repo
        /// (`<root>/.backlog/tasks.toml`), so this is NOT a cross-repo search,
        /// and it is ignored if `--project` is also given (`--project` wins,
        /// not a union). Note this can return a task belonging to another
        /// checkout, which is why it is opt-in.
        #[arg(long)]
        all: bool,

        /// Atomically reserve the returned task (CA-backlog-001): marks it
        /// `claimed` under the tasks-file lock in the same critical section
        /// that selects it, so two concurrent `next --claim` callers cannot
        /// both be handed the same pending task. Without this flag, `next`
        /// keeps its pre-existing pure-read behavior (no lock, no mutation).
        #[arg(long)]
        claim: bool,
    },

    /// Mark a task as done
    Done {
        /// Task ID
        id: String,
    },

    /// Mark a task as failed
    Fail {
        /// Task ID
        id: String,

        /// Failure reason
        #[arg(long)]
        reason: Option<String>,
    },

    /// Reconcile this store against its GitHub issues (one-way: local wins)
    Sync {
        /// Perform the reconciliation. Without this flag `sync` only reports
        /// the drift and touches nothing — GitHub-visible writes are opt-in.
        #[arg(long)]
        apply: bool,

        /// Cap how many actions to perform in one run (0 = no cap).
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },

    /// Edit a task's fields
    Edit {
        /// Task ID
        id: String,

        /// New title
        #[arg(long)]
        title: Option<String>,

        /// New tags (replaces existing tags)
        #[arg(long = "tag", action = clap::ArgAction::Append)]
        tags: Vec<String>,

        /// New notes
        #[arg(long)]
        notes: Option<String>,

        /// New status
        #[arg(long)]
        status: Option<String>,
    },

    /// SessionStart hook: reads stdin JSON and injects pending tasks as context
    SessionStart,

    /// Install hooks into ~/.claude/settings.json
    Install {
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove hooks from ~/.claude/settings.json
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage the per-project EXCLUSIVE lock (~/.backlog/locks/<project>.lock).
    ///
    /// Note: driving the queue does NOT require this lock. Per-task exclusivity
    /// is provided by `next --claim`, which reserves a task in the same critical
    /// section that selects it, so several sessions can drive one project's
    /// queue concurrently. Use `driver register` to announce that you are
    /// driving (non-exclusive); take this lock only when you genuinely need to
    /// exclude every other session from a project.
    Lock {
        #[command(subcommand)]
        action: LockAction,
    },

    /// Announce/observe drivers of a project's queue WITHOUT excluding them.
    ///
    /// Any number of sessions may be registered for the same project at once.
    /// This answers "is a driver active for this project" (what autoflow and
    /// daily need) without serializing whole sessions the way the exclusive
    /// lock does.
    Driver {
        #[command(subcommand)]
        action: DriverAction,
    },
}

#[derive(Subcommand)]
enum DriverAction {
    /// Register this session as a driver of `--project`. Never fails because
    /// another session is already registered.
    Register {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        project: String,
    },

    /// Refresh this session's registration so it is not reaped as stale.
    /// Upserts: a registration that was already reaped is written back, because
    /// a session that heartbeats is alive right now.
    Heartbeat {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        project: String,
    },

    /// Remove this session's registration (no-op if not registered).
    Unregister {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        project: String,
    },

    /// Print the registered drivers as JSON. With `--project`, that project's
    /// drivers only; without it, every project's. Always states `active`
    /// explicitly — including `"undetermined": true` when the registry could
    /// not be read, which is NOT reported as "no drivers".
    Status {
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum LockAction {
    /// Acquire the lock (errors if already active)
    Acquire {
        /// Session ID
        #[arg(long)]
        session_id: String,

        /// Project path
        #[arg(long)]
        project: String,

        /// Steal the lock even from a live holder (強制奪取). Without this, a
        /// dead holder's lock is reaped automatically but a live one errors.
        #[arg(long)]
        force: bool,
    },

    /// Release the lock (no-op if none)
    Release {
        /// Project path (must match the project passed to `acquire`)
        #[arg(long)]
        project: String,
    },

    /// Print the project's liveness as JSON, or "none".
    ///
    /// This reports the UNION of the exclusive lock and the non-exclusive
    /// driver registry, because `/flow` announces itself via `driver register`
    /// rather than by taking the lock: reporting only the lock would answer
    /// "none" while drivers were running. `kind` says which source answered,
    /// and `drivers`/`driver_count` list every live driver. The legacy
    /// contract is unchanged — `none` = nothing driving, `"stale": true` =
    /// only a dead holder, any other object = active.
    ///
    /// With `--project`, reports that project only. Without it, scans every
    /// project — the "is any driver active anywhere" scan `daily` depends on.
    Status {
        #[arg(long)]
        project: Option<String>,
    },

    /// Refresh the lock's heartbeat, keeping a long-running session's hold
    /// alive past the stale TTL. No-op if the lock isn't held by session_id.
    Heartbeat {
        /// Session ID (must match the current holder for this to take effect)
        #[arg(long)]
        session_id: String,

        /// Project path (must match the project passed to `acquire`)
        #[arg(long)]
        project: String,
    },

    /// Probe the current holder's PROGRESS (git HEAD + transcript growth), not
    /// mere liveness, and print the verdict + signals + ages. `reap_eligible`
    /// (verdict `stalled`) is the ONLY state in which a plain `acquire` will
    /// reap this holder; `progressing`/`undetermined` protect it. Each probe
    /// advances the multi-sample state machine, so a genuine stall accrues to
    /// `stalled` across repeated probes over the window.
    Probe {
        /// Project path (must match the project passed to `acquire`)
        #[arg(long)]
        project: String,

        /// Emit the full report as JSON (default: a one-line human summary).
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// Resolve the project filter for the read commands that are scoped to "this
/// checkout" by default (`list` and `next`).
///
/// Precedence, identical for every caller: an explicit `--project` wins; then
/// `--all` drops the project filter entirely; otherwise the cwd is resolved to
/// its CANONICAL project identity via `store::canonical_project_id` — a linked
/// worktree normalizes to the MAIN working tree it belongs to (mirroring
/// `canonicalize_project`'s write-side normalization), so this matches what
/// `add --project <main tree>` would have stored even when this process runs
/// from a worktree rather than the main tree itself.
///
/// A checkout whose project scope cannot actually be determined (a dangling
/// worktree `.git` link, an unreadable gitfile, …) returns `Err`, which
/// `main` surfaces as a non-zero exit with a diagnostic on stderr. It must NOT
/// degrade to `None` ("every project in this store") nor to an empty result:
/// both are indistinguishable, to a downstream reader, from a correctly
/// answered query, which is the fail-open CLAUDE.md §3 forbids.
///
/// `command` names the caller only for the diagnostic text; it does not change
/// the decision. Sharing this function is what keeps `list` and `next` from
/// disagreeing about what "this project" means.
fn default_project_scope(
    project: Option<String>,
    all: bool,
    command: &str,
) -> Result<Option<String>> {
    if project.is_some() {
        return Ok(project);
    }
    if all {
        return Ok(None);
    }
    let cwd = std::env::current_dir()?;
    match store::canonical_project_id(&cwd).require() {
        Required::Determined(root) => Ok(Some(root.to_string_lossy().into_owned())),
        Required::Blocked(verdict) => {
            let why = verdict
                .reason()
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            Err(anyhow::anyhow!(
                "cannot determine this checkout's project scope for the default \
                 `backlog {command}` (pass --project explicitly, or --all to bypass \
                 scoping entirely): {why}"
            ))
        }
    }
}

/// The project IDENTITY the claim ledger is keyed by — the one string every
/// checkout of this project must agree on, or the ledger splits per checkout
/// and stops excluding anything (the defect it exists to close, one level up).
///
/// It is deliberately NOT the store's location (that follows the checkout by
/// design) and NOT the raw `--project` string (two checkouts spell the same
/// project differently). Both branches below end at a string that
/// `lock::project_slug` hashes, and `project_slug` canonicalizes again, so a
/// worktree and its main tree land on the identical slug:
///   - an explicit `--project`: canonicalized WITH the resolved/guessed
///     marker. A GUESSED label is refused — keying the ledger by a guess would
///     silently give this checkout a private ledger, i.e. no exclusion at all,
///     which is exactly the shape of the bug (`unresolved` here means a
///     substituted fallback, not merely "the path is absent" — an absent
///     cross-machine label normalizes identically everywhere and is fine).
///   - the default scope (`--all`, where there is no `--project` to key on, or
///     any other unscoped claim): this checkout's own canonical identity,
///     which normalizes a linked worktree to its main working tree.
///
/// An identity that cannot be determined returns `Err` — a non-zero exit with
/// the reason on stderr — rather than claiming under a guessed key.
fn claim_identity(effective_project: Option<&str>) -> Result<String> {
    let Some(p) = effective_project else {
        let cwd = std::env::current_dir()?;
        return match store::canonical_project_id(&cwd).require() {
            Required::Determined(root) => Ok(root.to_string_lossy().into_owned()),
            Required::Blocked(verdict) => {
                let why = match verdict.reason() {
                    Some(r) => r.as_str().to_string(),
                    None => "unknown".to_string(),
                };
                Err(anyhow::anyhow!(
                    "cannot determine this checkout's project identity, so a claim could not be \
                     recorded project-wide and would be invisible to other checkouts; refusing \
                     to claim: {why}"
                ))
            }
        };
    };
    let canonical = store::canonicalize_project_with_marker(p);
    if canonical.unresolved {
        return Err(anyhow::anyhow!(
            "the project identity for {p} could not be resolved (a fallback label was \
             substituted), so a claim recorded under it may not be seen by other checkouts of \
             the same project; refusing to claim"
        ));
    }
    Ok(canonical.label)
}

/// The project the divergence check asks about — always THIS CHECKOUT, even
/// when the listing was widened.
///
/// `--all` widens which tasks are *listed* from the resolved store; it does not
/// change the question `divergence::check` answers ("is this checkout's work
/// sitting in a store I am not reading"), so it must not silently turn into a
/// bypass of that check. An explicit `--project` is different: the caller named
/// the project they are asking about, so that is the scope compared.
///
/// `None` only when the listing is unscoped AND this checkout's identity could
/// not be determined; the legacy store is then compared unscoped, which can
/// only make the check MORE conservative, never less.
fn divergence_scope(effective_project: Option<&str>) -> Option<String> {
    if let Some(p) = effective_project {
        return Some(p.to_string());
    }
    let cwd = std::env::current_dir().ok()?;
    match store::canonical_project_id(&cwd).require() {
        Required::Determined(root) => Some(root.to_string_lossy().into_owned()),
        Required::Blocked(_) => None,
    }
}

/// Refuse to answer "the queue is empty" when the answer is really "you are
/// reading a different file" (backlog 5ba13c3e).
///
/// `Undetermined` becomes `Err`, which `main` reports as a non-zero exit with
/// the reason on stderr and nothing on stdout — the identical shape
/// `default_project_scope` already uses for an undetermined project scope, and
/// the shape `autoflow::backlog::find_open` reads as
/// `Determination::Undetermined` instead of "no work". `Warn` stays on exit 0
/// on purpose: see `divergence`'s module docs for the consumer-by-consumer
/// reason a non-zero exit there would DESTROY real items rather than protect
/// them.
fn guard_store_divergence(tasks_path: &std::path::Path, project: Option<&str>) -> Result<()> {
    let scope = divergence_scope(project);
    match divergence::check(tasks_path, scope.as_deref()) {
        divergence::Divergence::None => Ok(()),
        divergence::Divergence::Warn(msg) => {
            eprintln!("{msg}");
            Ok(())
        }
        divergence::Divergence::Undetermined(msg) => Err(anyhow::anyhow!(msg)),
    }
}

fn run(cli: Cli) -> Result<()> {
    let cfg = config::Config::load();
    // The store's LOCATION is always resolved from THIS PROCESS'S OWN cwd,
    // never from `--project` (CLAUDE.md §8). `--project` names which project a
    // task BELONGS to — a value the caller supplies and this binary does not
    // control — not where the running checkout's own tasks.toml lives. Passing
    // `command_project(&cli.command)` here used to let `--project <main tree>`
    // from a linked worktree resolve the STORE to the main tree's own repo
    // root (`Config::store_dir_for` walks up from the given project path), so
    // a worktree session's `backlog add --project <main>` wrote straight into
    // a tree §8 forbids touching. `None` makes `Config::tasks_path_for` fall
    // through to `std::env::current_dir()` unconditionally, i.e. always the
    // checkout actually running this process. Computed BEFORE the match
    // because `cli.command` is moved into it.
    let tasks_path = cfg.tasks_path_for(None);

    match cli.command {
        Command::Add {
            title,
            project,
            mut tags,
            priority,
            notes,
            weight,
            force,
        } => {
            // priority is a shortcut for adding a priority tag
            if let Some(p) = priority {
                if !tags.contains(&p) {
                    tags.push(p);
                }
            }
            let now = now_unix();
            // Fail-soft GitHub push (Phase 1, one-way): resolve the project's
            // git remote URL for real (`git config --get remote.origin.url`,
            // best-effort empty on any failure) and inject the real `gh`
            // spawn via `gh_probe`, mirroring condukt::pr's real-closure-at-
            // the-call-site / fake-closure-in-tests pattern. The pure
            // decision (`github::decide_issue_create`, driven from
            // `store::add_with_weight_and_github_push`) never runs a process
            // itself, so unit tests exercise it with fake closures only.
            let remote_url = git_remote_origin_url(&project);
            let id = store::add_with_weight_and_github_push(
                &tasks_path,
                &title,
                &project,
                tags,
                &notes,
                weight,
                force,
                now,
                &remote_url,
                gh_probe,
            )?;
            println!("added: {id}");
        }

        Command::List {
            tag,
            project,
            status,
            json: as_json,
            all,
        } => {
            // A typo'd status used to silently match nothing ("no tasks"),
            // indistinguishable from a genuinely empty queue. Warn loudly so an
            // unknown filter value (e.g. the wrong `open`) is obvious. The check
            // lives in `task::status_warning` so it is unit-tested.
            if let Some(w) = task::status_warning(status.as_deref()) {
                eprintln!("{w}");
            }
            // Default scope is the cwd-resolved project
            // (backlog-list-default-scope): an explicit `--project` always
            // wins; `--all` drops the project filter, which now means "every
            // project in THIS store" rather than the old cross-project
            // default — the store itself is per repo, so a repo-resolved one
            // holds a single project key and `--all` shows the same tasks as
            // the default there; it still widens a pinned or legacy store,
            // which are the ones that hold several keys. Otherwise resolve cwd
            // to its CANONICAL project identity via `store::canonical_project_id`
            // — a linked worktree normalizes to the MAIN working tree it
            // belongs to (mirroring `canonicalize_project`'s write-side
            // normalization), so this matches what `add --project <main tree>`
            // would have stored even when this process is running from a
            // worktree, not the main tree itself.
            //
            // A checkout whose project scope cannot actually be determined (a
            // dangling worktree `.git` link, an unreadable gitfile, …) MUST
            // NOT silently degrade to an empty `[]` result: that is
            // indistinguishable from a genuinely empty queue to a downstream
            // reader (e.g. autoflow's `find_open`, which already treats a
            // non-zero `backlog list` exit as `Determination::Undetermined`
            // rather than "no work" — see `crates/autoflow/src/backlog.rs`).
            // So an undetermined scope here is surfaced as a hard error
            // (non-zero exit, diagnostic on stderr, nothing on stdout) instead
            // of a printed empty result.
            //
            // The precedence and the undetermined-scope error both live in
            // `default_project_scope`, shared with `next` so the two commands
            // cannot drift apart on what "this project" means.
            let effective_project = default_project_scope(project, all, "list")?;
            let tasks = store::list(
                &tasks_path,
                tag.as_deref(),
                effective_project.as_deref(),
                status.as_deref(),
            )?;
            // Before anything is printed: an empty listing produced from a
            // store that is not where this checkout's work actually lives must
            // not be rendered as an ordinary empty queue (backlog 5ba13c3e).
            // Placed ahead of BOTH renderers so neither `[]` nor `no tasks`
            // can escape on stdout when the answer is untrustworthy.
            guard_store_divergence(&tasks_path, effective_project.as_deref())?;

            if as_json {
                // Machine-readable array (consumed by autoflow). Each task keeps
                // its real field names (notably `title` and `status`), so callers
                // deserialize a subset and ignore the rest. `hashkey` is computed
                // (not stored) so callers like `/flow` can gate on
                // `condukt state is-claimed --hashkey <h>` without recomputing the
                // normalization themselves.
                let with_hashkey: Vec<serde_json::Value> = tasks
                    .iter()
                    .map(|t| {
                        let mut v = serde_json::to_value(t).unwrap_or_default();
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "hashkey".to_string(),
                                serde_json::Value::String(task::hashkey(&t.title, &t.project)),
                            );
                        }
                        v
                    })
                    .collect();
                println!("{}", serde_json::to_string(&with_hashkey)?);
            } else if tasks.is_empty() {
                println!("no tasks");
            } else {
                let now = now_unix();
                println!("{:<10} {:<10} {:<10} TITLE", "ID", "PRIORITY", "STATUS");
                for t in &tasks {
                    let priority_str = match t.priority() {
                        0 => "p0",
                        1 => "p1",
                        2 => "p2",
                        _ => "-",
                    };
                    let status_str = if t.is_deferred(now) {
                        "deferred".to_string()
                    } else {
                        t.status.clone()
                    };
                    // A task written while its project identity was
                    // UNDETERMINED is listed even when its (guessed) project
                    // label does not match this scope — see `store::list`.
                    // It must not blend in with the tasks that genuinely
                    // scope here, so the row states that its project is a
                    // guess and names the label that was guessed. Rendered
                    // ONLY for marked tasks: a legacy store (no marker field)
                    // therefore looks exactly as it did before.
                    let scope_str = if t.project_unresolved {
                        format!("  [project unresolved: {}]", t.project)
                    } else {
                        String::new()
                    };
                    println!(
                        "{:<10} {:<10} {:<10} {}{}",
                        t.id, priority_str, status_str, t.title, scope_str
                    );
                }
            }
        }

        Command::Next {
            tag,
            project,
            claim,
            all,
        } => {
            // `next` carries the SAME cwd-derived default project scope as
            // `list` (`default_project_scope`), and for the same two reasons.
            //
            // 1. Agreement. `next` used to hand its `Option<String>` straight
            //    to `store::next`, so a bare `next` ranged over EVERY project
            //    key in the resolved store while a bare `list` in the same cwd
            //    scoped to this checkout. A store holds several keys whenever
            //    it is pinned or legacy, or once an `add --project <other>`
            //    lands in it — and then `next` can hand a driver a task
            //    belonging to a different checkout entirely. Ranking makes
            //    this worse than a listing bug: the higher-weighted foreign
            //    task is exactly the one that gets picked.
            // 2. Fail-closed. An undetermined scope must not become "no
            //    pending tasks" on exit 0. Drivers read this output to decide
            //    whether there is work; "I could not determine which project
            //    this is" and "there is nothing to do" have to stay
            //    distinguishable (CLAUDE.md §3). `default_project_scope`
            //    returns `Err`, which `main` reports as a non-zero exit with a
            //    stderr diagnostic — the same shape `list` already had.
            //
            // Both apply to `--claim` too, which is the path real drivers use,
            // so the scope is resolved BEFORE the branch rather than inside
            // the read-only arm.
            let effective_project = default_project_scope(project, all, "next")?;
            // `next` is the question a DRIVER asks ("is there work?"), so
            // `no pending tasks` out of a store that is not this checkout's is
            // the most expensive false answer in the crate. Checked BEFORE the
            // claim so a diverged store neither reports emptiness nor mutates
            // the wrong file (backlog 5ba13c3e).
            guard_store_divergence(&tasks_path, effective_project.as_deref())?;
            let task = if claim {
                // The claim is project-GLOBAL, not store-local: the store
                // follows the checkout by design, so a claim recorded only in
                // this checkout's `tasks.toml` is invisible to every other
                // checkout of the same project and both are handed the same
                // task (backlog 709ff549). `claim_ledger` records it under
                // `~/.backlog/claims/<project-slug>.json` instead.
                let identity = claim_identity(effective_project.as_deref())?;
                let claimed = claim_ledger::claim_next(
                    &tasks_path,
                    tag.as_deref(),
                    effective_project.as_deref(),
                    &identity,
                    None,
                )?;
                match claimed {
                    Determination::Known(t) => t,
                    // REFUSED, not empty. Non-zero exit, reason on stderr,
                    // nothing on stdout — the identical shape
                    // `guard_store_divergence` uses, and the shape
                    // `autoflow::backlog::find_open` reads as
                    // `Determination::Undetermined` rather than "no work".
                    // Rendering this as `no pending tasks` on exit 0 is the
                    // fail-open this whole path exists to close.
                    Determination::Undetermined(why) => {
                        return Err(anyhow::anyhow!(
                            "backlog next --claim REFUSED to claim (this is NOT \
                             \"no pending tasks\" — nothing was claimed and the queue was not \
                             judged empty): {why}"
                        ));
                    }
                }
            } else {
                store::next(&tasks_path, tag.as_deref(), effective_project.as_deref())?
            };
            match task {
                Some(t) => {
                    // Emit the computed (not stored) `hashkey` alongside the
                    // task, exactly as `list --json` does. A driver that picks
                    // with `next --claim` needs it for the cross-session claim
                    // registry (`condukt state claim-task/release-task`) and
                    // for its `overwatch begin --key`; without it, picking this
                    // way would silently lose those keys.
                    let mut v = serde_json::to_value(&t)?;
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "hashkey".to_string(),
                            serde_json::Value::String(task::hashkey(&t.title, &t.project)),
                        );
                    }
                    println!("{}", serde_json::to_string_pretty(&v)?);
                }
                None => {
                    println!("no pending tasks");
                }
            }
        }

        Command::Done { id } => {
            store::mark_done(&tasks_path, &id)?;
            println!("done: {id}");
            // Mirror the completion to GitHub. `mark_done` above has already
            // committed the local truth, so this cannot fail the command — but
            // it also must not fail SILENTLY, which is precisely how 60 done
            // tasks ended up behind 60 open issues. A failure prints and
            // leaves `issue_closed_at` unset, so `backlog sync` retries it.
            mirror_close_for(&tasks_path, &id);
        }

        Command::Sync { apply, limit } => {
            let tasks = store::load(&tasks_path)?;
            let mut plan = store::sync_plan(&tasks);
            if limit > 0 && plan.len() > limit {
                plan.truncate(limit);
            }
            let creates = plan
                .iter()
                .filter(|a| matches!(a, store::SyncAction::Create { .. }))
                .count();
            let closes = plan.len() - creates;
            println!("sync plan: {creates} issue(s) to create, {closes} issue(s) to close");
            if !apply {
                for action in plan.iter().take(20) {
                    match action {
                        store::SyncAction::Create { id, title, .. } => {
                            println!("  create  {id}  {title}");
                        }
                        store::SyncAction::Close { id, number, reason } => {
                            println!("  close   #{number}  {id}  ({})", reason.as_gh_reason());
                        }
                    }
                }
                if plan.len() > 20 {
                    println!("  ... and {} more", plan.len() - 20);
                }
                println!("(dry run; pass --apply to perform)");
                return Ok(());
            }

            // The `gh` calls run OUTSIDE the tasks lock on purpose: at ~1s per
            // round-trip, holding it across a full reconciliation would block
            // every other session's add/next --claim for minutes. Confirmed
            // outcomes are folded back under the lock afterwards, against a
            // freshly-loaded store.
            let remote_url = git_remote_origin_url(&store_repo_root(&tasks_path));
            let mut outcomes = Vec::new();
            let mut failures: Vec<String> = Vec::new();
            for action in &plan {
                match action {
                    store::SyncAction::Create { id, title, notes } => {
                        match github::decide_issue_create(&remote_url, title, notes, gh_probe) {
                            github::IssueOutcome::Created { url } => {
                                match github::parse_issue_number(&url) {
                                    Some(number) => {
                                        println!("created #{number} for {id}");
                                        outcomes.push(store::SyncOutcome::Created {
                                            id: id.clone(),
                                            number,
                                            url,
                                        });
                                    }
                                    None => failures.push(format!(
                                        "{id}: created issue but could not parse number from {url:?}"
                                    )),
                                }
                            }
                            github::IssueOutcome::DegradedLocalOnly { reason } => {
                                failures.push(format!("{id}: create failed: {reason}"));
                            }
                        }
                    }
                    store::SyncAction::Close { id, number, reason } => {
                        match github::decide_issue_close(&remote_url, *number, *reason, gh_probe) {
                            github::CloseOutcome::Closed => {
                                println!("closed #{number} for {id}");
                                outcomes.push(store::SyncOutcome::Closed { id: id.clone() });
                            }
                            github::CloseOutcome::NotClosed { reason } => {
                                failures.push(format!("{id}: close #{number} failed: {reason}"));
                            }
                        }
                    }
                }
            }
            let updated = store::record_sync_outcomes(&tasks_path, &outcomes, now_unix())?;
            println!("sync: {} confirmed, {} recorded", outcomes.len(), updated);
            if !failures.is_empty() {
                eprintln!(
                    "sync: {} action(s) FAILED and were not recorded:",
                    failures.len()
                );
                for f in &failures {
                    eprintln!("  {f}");
                }
                // A partially-applied reconciliation that exits 0 reads as
                // "the mirror is in sync". It is not — say so in the exit code
                // as well as on stderr (CLAUDE.md §3), and re-running picks the
                // failures up again because nothing was recorded for them.
                return Err(anyhow::anyhow!(
                    "{} of {} sync action(s) did not complete; re-run `backlog sync --apply` to retry",
                    failures.len(),
                    plan.len()
                ));
            }
        }

        Command::Fail { id, reason } => {
            store::mark_failed(&tasks_path, &id, reason.as_deref())?;
            // mark_failed は defer_until を now + 172800 (2日後) に設定する。
            // 設定した defer_until を読み取って表示する。
            let tasks = store::load(&tasks_path)?;
            if let Some(task) = tasks.iter().find(|t| t.id == id) {
                if let Some(defer_until) = task.defer_until {
                    // defer_until を人が読める日時文字列に変換する
                    let secs = defer_until as u64;
                    let dt = format_unix_datetime(secs);
                    println!("failed: {id}");
                    println!("deferred until {dt} (2 日後に再実行されます)");
                } else {
                    println!("failed: {id}");
                }
            } else {
                println!("failed: {id}");
            }
        }

        Command::Edit {
            id,
            title,
            tags,
            notes,
            status,
        } => {
            let tags_opt = if tags.is_empty() { None } else { Some(tags) };
            store::edit(
                &tasks_path,
                &id,
                title.as_deref(),
                tags_opt,
                notes.as_deref(),
                status.as_deref(),
            )?;
            println!("updated: {id}");
        }

        Command::SessionStart => {
            run_hook(|| {
                let raw = read_stdin();
                // Don't silently coerce malformed stdin to an empty default —
                // that would run session-start against a blank input and could
                // mis-key the project. Surface it on stderr and skip (the hook
                // still exits 0, never breaking the turn).
                let Some(input) = HookInput::parse(&raw) else {
                    eprintln!("backlog session-start: malformed hook input on stdin; skipping");
                    return;
                };
                if let Some(ctx) = hooks::session_start::run(&input) {
                    println!("{}", json!({ "additionalContext": ctx }));
                }
            });
        }

        Command::Install { dry_run } => {
            install::install(dry_run)?;
        }

        Command::Uninstall { dry_run } => {
            install::uninstall(dry_run)?;
        }

        Command::Lock { action } => match action {
            LockAction::Acquire {
                session_id,
                project,
                force,
            } => {
                let pid = std::process::id();
                if force {
                    lock::acquire_forced(&session_id, pid, &project)?;
                    println!("lock acquired (forced)");
                } else {
                    lock::acquire(&session_id, pid, &project)?;
                    println!("lock acquired");
                }
            }
            LockAction::Release { project } => {
                lock::release(&project)?;
                println!("lock released");
            }
            LockAction::Status { project } => {
                let status = match &project {
                    Some(p) => lock::status(p),
                    None => lock::status_any(),
                };
                let presence = driver::presence_at(project.as_deref(), None);
                // Compute the holder's progress verdict up front, via the SAME
                // machinery as the reap gate, so an active/heartbeat liveness
                // object can never be rendered without a progress verdict. When
                // there is no --project (the cross-project `status_any` scan),
                // the winning lock's own project is used; an absent/unreadable
                // holder is `Undetermined`, not a fabricated "progressing".
                let progress = match (&project, &status) {
                    (Some(p), _) => lock::holder_progress_verdict(p),
                    (None, lock::LockStatus::Active(info))
                    | (None, lock::LockStatus::Stale(info)) => {
                        lock::holder_progress_verdict(&info.project)
                    }
                    (None, _) => harness_core::verdict::Determination::undetermined(
                        "no single holder to probe for progress",
                    ),
                };
                match liveness::status_value(status, presence, progress) {
                    None => println!("none"),
                    Some(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                }
            }
            LockAction::Probe { project, json } => {
                let report = lock::probe(&project);
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    let holder = report.holder_session.as_deref().unwrap_or("(none)");
                    println!(
                        "progress: {} (reap_eligible={}) holder={} heartbeat_stale={:?}",
                        report.verdict, report.reap_eligible, holder, report.heartbeat_stale
                    );
                    if let Some(reason) = &report.reason {
                        println!("  reason: {reason}");
                    }
                    for s in &report.signals {
                        println!(
                            "  signal {}: {} ({})",
                            s.name,
                            if s.readable { "readable" } else { "UNREADABLE" },
                            s.detail
                        );
                    }
                }
            }
            LockAction::Heartbeat {
                session_id,
                project,
            } => {
                lock::heartbeat(&session_id, &project)?;
                println!("lock heartbeat updated");
            }
        },

        Command::Driver { action } => match action {
            DriverAction::Register {
                session_id,
                project,
            } => {
                let info = driver::register_at(&session_id, std::process::id(), &project, None)?;
                println!("{}", serde_json::to_string_pretty(&info)?);
            }
            DriverAction::Heartbeat {
                session_id,
                project,
            } => {
                driver::heartbeat_at(&session_id, std::process::id(), &project, None)?;
                println!("driver heartbeat updated");
            }
            DriverAction::Unregister {
                session_id,
                project,
            } => {
                driver::unregister_at(&session_id, &project, None)?;
                println!("driver unregistered");
            }
            DriverAction::Status { project } => {
                // An unreadable registry is reported as `undetermined` with
                // `active: true` — never as an empty driver list, which would
                // read as "nobody is driving" (see driver.rs module docs).
                let v = match driver::presence_at(project.as_deref(), None) {
                    harness_core::verdict::Determination::Known(p) => json!({
                        "project": project,
                        "active": p.live_count() > 0,
                        "undetermined": false,
                        "count": p.live_count(),
                        "drivers": p.live,
                        "stale": p.stale,
                    }),
                    harness_core::verdict::Determination::Undetermined(why) => json!({
                        "project": project,
                        "active": true,
                        "undetermined": true,
                        "reason": format!("{why:?}"),
                    }),
                };
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        },
    }

    Ok(())
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Bound on how long `git_remote_origin_url` waits for `git` before treating
/// it as absent/failed. `add_with_weight_and_github_push` calls it from
/// inside `Add`'s critical path, so an unbounded spawn (a hung `git` — a
/// corrupt/locked index, a wedged credential helper, an fsmonitor/hook
/// subprocess that never returns) would block the whole `backlog add`
/// indefinitely. Kept short relative to `GH_TIMEOUT`: this is a local
/// `git config` read, not a network round-trip to `gh`, so it does not need
/// 20s to distinguish "slow" from "hung".
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Best-effort git remote URL (`git config --get remote.origin.url`, run in
/// `project`), trimmed. Empty string on any failure (not a git repo, no
/// `origin` remote, `git` absent, non-UTF8 output, or a hung `git` that
/// doesn't finish within `GIT_TIMEOUT`) — the IO boundary for
/// `store::add_with_weight_and_github_push`'s `remote_url` parameter. An
/// empty/non-github.com string makes `github::is_github_remote` false, which
/// fail-softs the push to local-only, exactly like every other failure mode
/// here. Bounded via `boundary::run_with_timeout` (mirrors `gh_probe` below
/// and `propguard::git`'s `GIT_TIMEOUT` pattern) so a hung `git` cannot hold
/// up the caller. Never spawns from within a unit test — this fn is only
/// reachable from the `Add` handler, not from any pure decision logic under
/// test.
fn git_remote_origin_url(project: &str) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", project, "config", "--get", "remote.origin.url"])
        .stdin(std::process::Stdio::null());
    match boundary::run_with_timeout(&mut cmd, GIT_TIMEOUT) {
        Determination::Known(out) => match out.stdout_allowing(&[0]) {
            Determination::Known(s) => s.trim().to_string(),
            Determination::Undetermined(_) => String::new(),
        },
        Determination::Undetermined(_) => String::new(),
    }
}

/// The repo root a resolved store belongs to, as a string for
/// `git_remote_origin_url`. A store always lives at
/// `<root>/.backlog/tasks.toml`, so the root is two parents up.
///
/// Derived from the store PATH rather than from any task's `project` field on
/// purpose: `project` labels are written by whichever checkout made the task
/// and legitimately name other worktrees of the same repo — and, for a
/// cross-machine label such as `C:/Users/.../harness`, a path that does not
/// exist here at all. Asking git about those yields an empty remote, which
/// would fail-soft the whole reconciliation into "not a GitHub remote" and
/// silently do nothing.
fn store_repo_root(tasks_path: &Path) -> String {
    tasks_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Close the GitHub issue mirroring task `id`, if the store says there is one
/// to close. Prints what happened either way; never returns an error, because
/// the caller has already committed the local state change and the mirror is
/// downstream of it. A failure deliberately records NOTHING, so the task keeps
/// its `issue_closed_at: None` and the next `backlog sync` retries.
fn mirror_close_for(tasks_path: &Path, id: &str) {
    let Ok(tasks) = store::load(tasks_path) else {
        eprintln!("warning: could not re-read the store to close {id}'s issue; run `backlog sync`");
        return;
    };
    let plan: Vec<_> = store::sync_plan(&tasks)
        .into_iter()
        .filter(|a| a.task_id() == id)
        .collect();
    let remote_url = git_remote_origin_url(&store_repo_root(tasks_path));
    for action in plan {
        if let store::SyncAction::Close { number, reason, .. } = action {
            match github::decide_issue_close(&remote_url, number, reason, gh_probe) {
                github::CloseOutcome::Closed => {
                    match store::record_sync_outcomes(
                        tasks_path,
                        &[store::SyncOutcome::Closed { id: id.to_string() }],
                        now_unix(),
                    ) {
                        Ok(_) => println!("closed issue #{number}"),
                        Err(e) => eprintln!(
                            "warning: closed issue #{number} but could not record it ({e}); \
                             `backlog sync` will retry the close (a no-op on an already-closed issue)"
                        ),
                    }
                }
                github::CloseOutcome::NotClosed { reason } => {
                    eprintln!(
                        "warning: issue #{number} left OPEN: {reason}; \
                         run `backlog sync --apply` to retry"
                    );
                }
            }
        }
    }
}

/// Bound on how long `gh_probe` waits for `gh` before treating it as absent.
/// `add_with_weight_and_github_push` calls `gh_probe` from inside the
/// tasks-file lock's critical section, so an unbounded spawn (`gh` hanging on
/// a network auth prompt) would hold that lock indefinitely and block every
/// other session's `backlog add`/`next --claim`. Mirrors `propguard::git`'s
/// `GIT_TIMEOUT` use of `boundary::run_with_timeout`.
const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// Spawn `gh <argv>` and map it to the injected-runner shape
/// `github::decide_issue_create` expects: `Some((success, combined_output))`,
/// or `None` when the binary can't be spawned OR doesn't finish within
/// `GH_TIMEOUT` (gh absent / hung — both are the same fail-soft "absent"
/// signal to the caller, since either way there is no answer to act on).
/// Never panics. Mirrors condukt::main's `gh_probe` for the same
/// `gh_status`-style detection pattern, plus the timeout bound from
/// `boundary::run_with_timeout` (kills the whole process group on timeout).
fn gh_probe(argv: &[&str]) -> Option<(bool, String)> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(argv).stdin(std::process::Stdio::null());
    match boundary::run_with_timeout(&mut cmd, GH_TIMEOUT) {
        Determination::Known(out) => {
            let code = out.code();
            let stderr = out.stderr().to_string();
            let stdout = match out.stdout_allowing(&[code]) {
                Determination::Known(s) => s,
                Determination::Undetermined(_) => unreachable!(
                    "stdout_allowing given the command's own exit code always succeeds"
                ),
            };
            Some((code == 0, format!("{stdout}{stderr}")))
        }
        Determination::Undetermined(_) => None,
    }
}

/// Unix タイムスタンプ (秒) を "YYYY-MM-DD HH:MM UTC" 形式の文字列に変換する。
/// 標準ライブラリのみで実装 (外部クレート不使用)。
fn format_unix_datetime(secs: u64) -> String {
    // グレゴリオ暦変換 (ユリウス通日ベース)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;

    // 1970-01-01 からの日数を年月日に変換 (Fliegel-Van Flandern algorithm)
    let jd = days + 2440588; // Julian Day Number for 1970-01-01
    let l = jd + 68569;
    let n = 4 * l / 146097;
    let l = l - (146097 * n).div_ceil(4);
    let i = 4000 * (l + 1) / 1461001;
    let l = l - 1461 * i / 4 + 31;
    let j = 80 * l / 2447;
    let day = l - 2447 * j / 80;
    let l = j / 11;
    let month = j + 2 - 12 * l;
    let year = 100 * (n - 49) + i + l;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hh, mm
    )
}

#[cfg(test)]
#[allow(clippy::panic)]
mod gh_probe_tests {
    use super::*;

    // ── gh-probe-timeout: gh_probe must be timeout-bounded ──────────────────
    //
    // Before this fix, `gh_probe` spawned `gh` with no timeout at all. Since
    // `add_with_weight_and_github_push` calls it from inside the tasks-file
    // lock's critical section, a hung `gh` (e.g. stuck on an interactive
    // network-auth prompt) would hold that lock indefinitely and block every
    // other session's `backlog add`/`next --claim`. Actually reproducing a
    // hung `gh` binary is too heavy for a unit test, so this checks the two
    // things done_criteria calls out instead: the timeout bound is set to a
    // concrete, sane value, and the underlying mechanism `gh_probe` wires
    // through (`boundary::run_with_timeout`) really does bound a hung
    // subprocess rather than blocking forever — mirroring
    // `propguard::git`'s equivalent regression test for the same mechanism.

    #[test]
    fn gh_timeout_is_a_concrete_bounded_value() {
        assert!(
            GH_TIMEOUT > Duration::from_secs(0),
            "GH_TIMEOUT must be a real bound, not zero/disabled"
        );
        assert!(
            GH_TIMEOUT <= Duration::from_secs(60),
            "GH_TIMEOUT must stay well under the tasks-file lock's own stale-reap window, \
             got {GH_TIMEOUT:?}"
        );
    }

    #[test]
    fn hung_subprocess_via_run_with_timeout_returns_promptly_undetermined() {
        // Exercises the exact mechanism `gh_probe` calls
        // (`boundary::run_with_timeout`) against a deliberately hanging
        // command, with a short local timeout override so the test doesn't
        // have to wait out the production `GH_TIMEOUT` (20s) or depend on a
        // real slow `gh` binary being available.
        let marker = format!("backlog-gh-probe-hang-marker-{}", std::process::id());
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", &format!("sh -c 'sleep 30' {marker}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let short_timeout = Duration::from_millis(300);

        let start = std::time::Instant::now();
        let outcome = boundary::run_with_timeout(&mut cmd, short_timeout);
        let elapsed = start.elapsed();

        assert!(
            matches!(outcome, Determination::Undetermined(_)),
            "a timed-out invocation must fall back gracefully (Undetermined), matching the \
             `None` that gh_probe maps it to, not fabricate a Known result: {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must return promptly on timeout, took {elapsed:?}"
        );
    }
}

// ── git-remote-url-timeout: git_remote_origin_url must be timeout-bounded ──
//
// Before this fix, `git_remote_origin_url` spawned `git config --get
// remote.origin.url` via a raw `std::process::Command::output()` with no
// timeout at all, so a hung `git` (e.g. stuck on a corrupt/locked index, an
// interactive credential prompt some misconfigured `git config` triggers, or
// an fsmonitor/hook subprocess that never returns) would block the `Add`
// command's caller indefinitely. This mirrors the exact `gh_probe`-timeout
// fix above and `propguard::git`'s `GIT_TIMEOUT`/`run_git` treatment of the
// same underlying problem.
#[cfg(test)]
#[allow(clippy::panic)]
mod git_remote_url_timeout_tests {
    use super::*;

    #[test]
    fn git_timeout_is_a_concrete_bounded_value() {
        assert!(
            GIT_TIMEOUT > Duration::from_secs(0),
            "GIT_TIMEOUT must be a real bound, not zero/disabled"
        );
        assert!(
            GIT_TIMEOUT <= Duration::from_secs(20),
            "GIT_TIMEOUT must stay short — this is a local git-config read, not a network \
             call like GH_TIMEOUT, got {GIT_TIMEOUT:?}"
        );
    }

    #[test]
    fn hung_subprocess_via_run_with_timeout_returns_promptly_undetermined() {
        // Exercises the exact mechanism `git_remote_origin_url` must call
        // (`boundary::run_with_timeout`) against a deliberately hanging
        // command, with a short local timeout override so the test doesn't
        // have to wait out the production `GIT_TIMEOUT` or depend on a real
        // stuck `git` binary being available.
        let marker = format!("backlog-git-remote-url-hang-marker-{}", std::process::id());
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", &format!("sh -c 'sleep 30' {marker}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let short_timeout = Duration::from_millis(300);

        let start = std::time::Instant::now();
        let outcome = boundary::run_with_timeout(&mut cmd, short_timeout);
        let elapsed = start.elapsed();

        assert!(
            matches!(outcome, Determination::Undetermined(_)),
            "a timed-out invocation must fall back gracefully (Undetermined), matching the \
             empty string that git_remote_origin_url maps it to, not fabricate a Known \
             result: {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must return promptly on timeout, took {elapsed:?}"
        );
    }
}

/// `claim_identity` refusing a GUESSED project label had a kill rate of ZERO:
/// deleting the `canonical.unresolved` refusal left all 225 tests green
/// (measured 2026-08-12 on `2b8abcc6`). That arm is load-bearing — keying the
/// project-wide ledger by a guessed slug gives this checkout a PRIVATE ledger,
/// i.e. no cross-checkout exclusion at all, which is the very defect
/// `709ff549` closes. An arm with no test is an arm that can be deleted by
/// accident, so it gets its own behavioural test here.
#[cfg(test)]
mod claim_identity_tests {
    use super::*;

    /// A path that EXISTS but whose identity cannot be resolved must refuse
    /// the claim, not claim under the substituted fallback label.
    ///
    /// The undetermined-ness is injected physically: the project path lives
    /// inside a mode-000 directory, so `symlink_metadata` returns EACCES —
    /// "could not determine whether it exists", which is neither absent nor
    /// resolved.
    #[test]
    fn an_unresolvable_project_label_refuses_the_claim_rather_than_guessing() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!(
            "backlog-claim-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let outer = base.join("outer");
        let inner = outer.join("proj");
        std::fs::create_dir_all(&inner).expect("fixture dirs");

        // Deny traversal so that stat-ing `inner` fails with EACCES rather
        // than NotFound. Root ignores this, so the assertion below is skipped
        // rather than falsely passing if the probe does not actually degrade.
        std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let degraded = std::fs::symlink_metadata(&inner)
            .err()
            .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);

        let got = claim_identity(Some(&inner.to_string_lossy()));

        // Restore before asserting so a failure still cleans up.
        let _ = std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&base);

        if !degraded {
            eprintln!(
                "skipped: this environment can stat inside a mode-000 dir (running as root?)"
            );
            return;
        }
        let err = got.expect_err(
            "an unresolvable project label must REFUSE the claim: claiming under the substituted \
             fallback label keys the ledger to a private slug, so no other checkout of the \
             project ever sees the claim (backlog 709ff549)",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("refusing to claim"),
            "the refusal must say so verbatim, got: {msg}"
        );
    }
}
