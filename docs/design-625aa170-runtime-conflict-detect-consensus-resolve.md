# Design — 625aa170: runtime conflict detection (A) + consensus merge-conflict resolution (B)

Status: DESIGN (awaiting human review before build). Depends on the merged PDO
conflict-hardening batch (schedule.rs twins fc03d26d, worktree repo-lock c701e75f).

## Baseline anchors
- `schedule.rs` clears parallel-safety on *declared* `touched_files` only
  (`files_conflict`/`entries_conflict`, schedule.rs:175, 513-535) — structurally blind to a
  worker editing an *undeclared* file. That is the gap (A) closes.
- The detection hook already exists in prototype: `diffrisk_record::record_post_execution_diff_risk`
  (diffrisk_record.rs:180) fires from `state set --status done` (main.rs:3386-3400) on the
  `Running/Pending → Done` edge (dedup-safe), already computing the real worktree diff vs base
  (`worktree_diff`, diffrisk_record.rs:92-109). This is the wiring point + fail-soft precedent for (A).
- Overwatch = append-only JSONL streams under `~/.overwatch/<project-key>/overwatch/`.
  Lockless atomic append (`append_violation`, store.rs:209); read-modify-rewrite / check-then-append
  take `LeaseLock::acquire(cwd)` (store.rs:400,479,620). condukt already depends on overwatch
  (diffrisk calls `overwatch::store::append_violation`); reverse dep is forbidden (cycle) — overwatch
  reads condukt state by path. This dep direction dictates task ownership.
- `overwatch review-queue` merges streams by `EntryKind` (Systemic/Rollback/AiFinding/Escalation,
  review_queue.rs:26-49) — the consensus kind is added here.
- `condukt policy answer`/`decide`: risk×reversibility×confidence → Auto(0)/Escalate(2)/Block(3)
  (policy.rs) — the policy half of (B). `gatelog::GateDecision` + `escalate.rs` = human half.
- condukt + overwatch are GATE crates → lockstep 3-file bump + `rollout-plugins.sh --canary`.

## (A) Mid-flight runtime conflict detection
- Fire a sibling of the diff-risk hook at main.rs:3386 (`state set --status done`, post-worker-commit
  pre-merge): `runtime_conflict::record_and_check_actual_overlap(...)`. Worktree still on disk →
  `git diff --name-only <base>...HEAD` = ACTUAL changed set (reuse three-dot base + fail-soft cascade).
- New overwatch type `ActualChangeset { task_key, run_id, session_id, branch, base_ref (frozen SHA),
  head_sha, files (normalized repo-relative), ts, merged }`, stored in NEW registry
  `active_changesets.json` (BTreeMap by task_key) + append-only `runtime_conflicts.jsonl`. New module
  `crates/overwatch/src/changeset.rs` mirroring violations_path/append/read trio.
- Cross-check is a shared-registry RMW → runs under `overwatch::lock::LeaseLock::acquire(repo)` (the
  SAME lock as other overwatch registry mutations; NOT condukt's REPO_PRIMARY_LOCK_KEY — the diff is
  in the task's own worktree). For each other in-flight (`!merged` AND lease live per LEASE_TTL_SECS)
  entry, intersect normalized paths (reuse `schedule::normalize_entry` for casefold/`./`-strip parity;
  literal paths → plain set intersection, no glob). Append one `RuntimeConflictEvent
  { run_id, task_key_a, task_key_b, overlapping_files, base_ref, session_id, ts, detail }` per overlap,
  then upsert+save (temp+rename).
- Fully fail-soft (missing worktree / git fail / empty diff / lock timeout / write error → silent
  no-op, exit code + schedule gating untouched). Observational.
- Cleanup: mark `merged=true`/delete on branch land — hook merge-success (main.rs:3024-3033 run_pr Poll;
  main.rs:2695 run_worktree Merge) + reconcile.rs. Lease-liveness filter ages out crashed runs.

## (B) Consensus merge-conflict resolution
- New `EntryKind::MergeConflict` (`[merge-conflict]`, High) in review_queue.rs + `merge_conflicts.jsonl`
  stream + `resolve` mutation in new `crates/overwatch/src/merge_conflict.rs` (mirror
  disposition.rs/append_disposition, check-then-append under LeaseLock).
- Schema: `MergeConflictEntry { conflict_id, run_id, branch, default_branch, base_ref,
  conflicted_files, diff_ours (base...default), diff_theirs (base...branch) [byte-bounded], ts,
  resolution: Option<Resolution> }`; `Resolution { choice: Ours|Theirs|Manual,
  decided_by: Human|Policy, note, ts }`.
- Capture at the existing trial-merge conflict path (worktree.rs:443-464) which today only `bail!`s:
  before aborting, grab `conflicted_files` (`git ls-files --unmerged`) + both diffs, record ONE entry.
  The `bail!` (block) is PRESERVED — conflict still stops merge; entry makes it visible/resolvable
  instead of silent local-only degrade. Diffs truncated to a byte cap.
- Two decision authorities: (1) Human — review-queue surfaces the open row; new CLI
  `overwatch resolve-merge-conflict --id <id> --choose ours|theirs|manual [--note]` writes Resolution
  (RMW under LeaseLock). (2) Policy — condukt frames it via `policy answer`; DEFAULT posture: conflict
  may only Escalate/Block — auto pick-side (Auto) is DISALLOWED unless opted in (auto pick-side IS
  last-writer-wins, the very failure this kills).
- Reconciliation driver: new `condukt worktree resolve-merge --id <id>` reads Resolution, executes
  under `lock::acquire_repo_primary`: Ours→`merge -s ours`/skip; Theirs→`merge -X theirs`/checkout;
  Manual→materialize markers→worker/human→commit; then re-run real `worktree::merge`. Closes
  decision→reconciliation loop.

## Decomposition (GATE crates; overwatch-API-first)
- **T1** overwatch changeset registry + runtime-conflict stream — changeset.rs(new), store.rs, lib.rs.
  RED: append/read round-trip + pure overlap() intersection + concurrent double-write serialized (LeaseLock).
- **T2** condukt detection hook — runtime_conflict.rs(new), main.rs. RED: two worktrees edit an
  UNDECLARED shared file, both →done → exactly one event names that file; missing-worktree/git-fail = no-op. (dep T1)
- **T3** overwatch merge-conflict store + review-queue kind + resolve CLI — merge_conflict.rs(new),
  review_queue.rs, store.rs, main.rs. RED: open entry shows as `[merge-conflict]` High in
  review-queue --json; resolve writes Resolution and leaves open set; concurrent resolves no double-write. (dep T1, shares store.rs → serialized)
- **T4** condukt merge-conflict capture — worktree.rs, main.rs. RED: guaranteed 3-way conflict
  (worktree.rs:555 fixture) records entry with both diffs + conflicted_files; merge still blocks, exit 0. (dep T3)
- **T5** condukt resolution driver + policy wiring — worktree.rs, main.rs, policy.rs. RED: given
  ours/theirs/manual, resolve-merge reconciles + subsequent merge succeeds; policy default = Escalate not Auto. (dep T3,T4)
- **T6** lockstep version bumps + canary rollout — condukt+overwatch Cargo.toml/plugin.json +
  marketplace.json. check-plugin-versions.py + check-version-bumped.py green; rollout --canary. (dep T1-T5, Class::Gated)

Serialization: T1&T3 share overwatch store.rs → serial; T2/T4/T5 share condukt main.rs (+worktree.rs) → serial.
**Keep version files OUT of T1-T5 touched_files** and do ALL bumps in trailing T6 — else the single shared
root marketplace.json demotes every task to serial. Net order: (T1→T3) and (T2 after T1 → T4 after T3 → T5), then T6.

## Open questions for human review (recommendations in brackets)
1. (A) block vs annotate — [annotate-only; observational, do NOT gate the merge]. (B) keeps hard-block.
2. base-ref — [three-dot merge-base, freeze resolved SHA in ActualChangeset.base_ref]. Re-trigger on moving default branch?
3. in-flight set location — [overwatch global active_changesets.json → catches cross-RUN collisions; lease-filtered]. vs condukt run state (intra-run only).
4. actual-vs-actual only, or also vs declared — [actual-vs-actual: precise, seen once both committed, symmetric on late finisher]. Add vs-declared for earlier (noisier) warning?
5. stale-entry retention — remove on merge / prune / TTL / all? Crashed run leaves merged=false → phantom overlaps until TTL. [lease-TTL fallback acceptable?]
6. diff storage bounds for (B) — pick truncation byte cap; inline JSONL vs blob path for full diffs.
7. may policy ever auto-pick a side — [default NO (Escalate/Block only) to avoid re-introducing last-writer-wins]. Opt-in Auto desired at all?

## DECISIONS (human review 2026-07-18)
1. **(A) GATE the merge on overlap** (not annotate-only). A detected actual-overlap between two
   in-flight tasks HOLDS the second task's merge until resolved. Design delta: detection no longer
   pure-observational — on overlap, T2 (a) records the `runtime_conflicts` event AND (b) sets a
   **merge-hold** on the task; the condukt merge path (T4 territory: worktree merge callers,
   main.rs:2695 / 3024) must check the hold and, if set, SKIP the merge and enqueue the overlap into
   the SAME consensus review surface as a real merge conflict (reuse `[merge-conflict]` kind or a
   sibling `[runtime-overlap]` kind in review_queue). T5's `resolve-merge` clears the hold then
   proceeds. This unifies A→B into one gate→review→resolve loop. Consequence: T2 now also depends on
   T3 (needs the review surface to enqueue), and the merge-hold check lands in the condukt merge path.
   Keep the gate fail-soft on the DETECTION side (compute-overlap errors never hold), but the HOLD
   itself is a real block (conservative: if overlap is detected, do not silently merge).
   NOTE: mitigate false positives — a detected same-file overlap that git would merge cleanly should
   still be gated per this decision, but the review entry should carry both diffs so the resolver can
   fast-approve a clean merge; do not hard-fail the run (block == hold-for-review, not error).
2. **(B) Escalate/Block only** — policy MUST NOT auto-pick a side. `policy answer` for a conflict/overlap
   returns only Escalate or Block; there is NO opt-in Auto pick-side. Remove that option from T5.
3. **In-flight set = overwatch global** `active_changesets.json` (cross-run, lease-filtered) — confirmed.
   Reject the condukt-run-state-only alternative (misses cross-run/multi-session drift).
- Defaults taken as recommended for the rest: three-dot merge-base with frozen base SHA (Q2);
  actual-vs-actual comparison (Q4); lease-TTL stale cleanup + on-merge/on-prune removal (Q5);
  byte-capped inline diffs in JSONL (Q6).
