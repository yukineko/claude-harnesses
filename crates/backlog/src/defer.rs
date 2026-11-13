//! `backlog defer ID --ruling QUOTE` is ABOLISHED (user ruling 2026-11-13:
//! 「保留はありえない実証されないならclose,実証されたらopen」). There is no
//! deferral state any more: a row is open only while its stored repro still
//! reproduces, and is closed (auto-cancel) when it does not — by
//! `next --claim` for the candidate it is about to hand out, and by
//! `backlog recheck` for every open row.
//!
//! The subcommand is kept only so that a caller following an old instruction
//! gets a non-zero refusal naming the rule instead of clap's generic
//! "unrecognized subcommand". Nothing is written. Legacy rows already stored
//! as `ruled-deferred` are still parsed; `backlog recheck` resolves them
//! (reproduced -> `pending`, otherwise auto-cancelled).

use anyhow::{anyhow, Result};

use crate::claim_recheck::NO_DEFERRAL_RULING;

/// Arguments of `backlog defer` (accepted only to refuse them).
#[derive(clap::Args, Debug)]
pub struct DeferArgs {
    /// Task ID
    pub id: String,

    /// The user's ruling, quoted (ignored: `defer` is abolished)
    #[arg(long)]
    pub ruling: Option<String>,
}

/// Always an `Err` (non-zero exit); never touches the store.
pub fn run(args: &DeferArgs) -> Result<()> {
    Err(anyhow!(
        "refused: `backlog defer {}` is abolished — there is no deferral state (user ruling \
         2026-11-13 {NO_DEFERRAL_RULING}). Nothing was written. A ticket stays open only while \
         its stored repro still reproduces: run `backlog recheck --id {}` from a linked \
         worktree (reproduced -> stays pending; not demonstrated -> auto-cancelled), or close \
         it yourself with `backlog cancel {} --reason R`.",
        args.id,
        args.id,
        args.id
    ))
}
