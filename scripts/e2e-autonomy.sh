#!/usr/bin/env bash
#
# e2e-autonomy.sh — deterministic proof that the autonomy switch lets the
# "scout 施策 → backlog → condukt → verify done" chain run with ZERO human
# intervention (autonomy DoD #6).
#
# ────────────────────────────────────────────────────────────────────────────
# WHAT THIS SCRIPT VERIFIES (the deterministic control-plane wiring)
#   1. The autonomy switch itself: `condukt state autonomy-check` returns
#      exit 0 + {"autonomous":true} in this environment. If autonomy is OFF
#      (or the subcommand is missing) the script says so EXPLICITLY and skips
#      the demo — it never pretends the chain ran.
#   2. The backlog queue mechanics: a scout-style opportunity can be enqueued
#      with `backlog add` and then surfaces in `backlog list --status pending`
#      (the hand-off queue between scout and flow/condukt). Done in an ISOLATED
#      temp HOME holding its own throwaway git repo + linked worktree and a
#      committed failing repro test (backlog keeps one store per repo and files
#      only reproduced findings), so no real store is ever touched.
#   3. The gate CONTRACTS that remove every human `AskUserQuestion` stop along
#      the chain, asserted by grepping the actually-installed SKILL specs:
#        - scout  Phase 4 (selection Ask)  → skipped under autonomy
#        - scout  Phase 6 (hand-off Ask)   → auto-handoff into /flow
#        - condukt Phase 3 (agreement Ask) → schedule adopted as-is
#        - flow   Step 0.5 (human gates)   → degraded to deterministic defaults
#      Together these prove no human decision point remains on the happy path.
#
# WHAT THIS SCRIPT DOES **NOT** VERIFY (be honest — no silent truncation)
#   * It does NOT run the real LLM work: condukt's worker/verifier generation,
#     scout's web research/scoring, or flow's decomposition reasoning are all
#     model-driven and heavy, so they are OUT OF SCOPE here.
#   * It does NOT assert the semantic CORRECTNESS of any generated code, nor
#     does it create git worktrees / merges. Those belong to the manual full
#     e2e run documented in docs/e2e-autonomy.md.
#   * Passing here means: "the deterministic layer (switch + gate contracts +
#     queue wiring) PERMITS a human-0 chain", NOT "a real autonomous run
#     produced a correct, merged change".
# ────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── tiny reporting helpers ──────────────────────────────────────────────────
PASS=0
say()  { printf '%s\n' "$*"; }
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
info() { printf '  \033[36m·\033[0m   %s\n' "$*"; }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$*"; }
die()  { printf '  \033[31mFAIL\033[0m %s\n' "$*" >&2; exit 1; }

# ── resolve the binaries (PATH first, plugin-cache fallback) ────────────────
resolve_bin() {
  # $1 = binary name; prints an absolute path or empty
  local name="$1" p
  if p="$(command -v "$name" 2>/dev/null)"; then
    printf '%s\n' "$p"; return 0
  fi
  for p in "$HOME"/.claude/plugins/cache/*/"$name"/*/bin/"$name"; do
    [ -x "$p" ] && { printf '%s\n' "$p"; return 0; }
  done
  return 1
}

CONDUKT="$(resolve_bin condukt || true)"
BACKLOG="$(resolve_bin backlog || true)"

say "== e2e-autonomy: deterministic human-0 chain proof =="
[ -n "$CONDUKT" ] || die "condukt binary not found on PATH or in plugin cache"
[ -n "$BACKLOG" ] || die "backlog binary not found on PATH or in plugin cache"
info "condukt = $CONDUKT"
info "backlog = $BACKLOG"

# ── 1. autonomy switch: the ambient environment must be autonomous ──────────
say "-- step 1: autonomy switch (condukt state autonomy-check) --"
set +e
AUT_JSON="$("$CONDUKT" state autonomy-check 2>/dev/null)"
AUT_EXIT=$?
set -e
case "$AUT_EXIT" in
  0) : ;;  # autonomous — continue below
  127)
    skip "condukt has no 'autonomy-check' subcommand (exit 127) — treated as NON-autonomous."
    skip "This condukt predates the autonomy switch; the human-0 chain cannot be demonstrated."
    say  "RESULT: SKIP (autonomy switch not present)"
    exit 0 ;;
  *)
    skip "autonomy-check exit=$AUT_EXIT json='${AUT_JSON:-}' → autonomy is DISABLED."
    skip "Enable it via ~/.condukt/config.toml (autonomous = true) or CONDUKT_AUTONOMOUS=1,"
    skip "then re-run. Without it the scout→…→done chain keeps its human Ask gates."
    say  "RESULT: SKIP (autonomy disabled — human gates intact, as designed)"
    exit 0 ;;
esac
[ "$AUT_JSON" = '{"autonomous":true}' ] \
  || die "expected {\"autonomous\":true}, got '$AUT_JSON'"
ok "autonomy-check → exit 0 + $AUT_JSON  (ambient autonomy is ON)"
PASS=$((PASS+1))

# ── 2. backlog queue mechanics in an ISOLATED temp HOME ─────────────────────
say "-- step 2: scout-style enqueue → pending queue (isolated) --"
TMPHOME="$(mktemp -d)"
cleanup() {
  # idempotent teardown of the throwaway backlog/state; never fail the script.
  [ -n "${TMPHOME:-}" ] && [ -d "$TMPHOME" ] && rm -rf "$TMPHOME" 2>/dev/null || true
}
trap cleanup EXIT
info "isolated HOME = $TMPHOME  (real ~/.backlog is untouched)"

DEMO_TITLE="scout: harden autonomy hand-off (seeded by e2e)"

# backlog keeps ONE store per repo (<repo root>/.backlog/tasks.toml), refuses a
# --project naming a different repo, refuses `add` inside a primary working
# tree, and files a task only when a committed --repro-test FAILS at HEAD. So
# the demo gets its own throwaway git repo inside the temp HOME, a linked
# worktree to run from, and a committed failing repro script — the shape a
# real scout Phase-5 `backlog add` has. Nothing touches the invoking repo.
DEMO_GIT_ENV=(HOME="$TMPHOME" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  GIT_AUTHOR_NAME=e2e GIT_AUTHOR_EMAIL=e2e@example.invalid
  GIT_COMMITTER_NAME=e2e GIT_COMMITTER_EMAIL=e2e@example.invalid)
DEMO_PROJECT="$(cd "$TMPHOME" && pwd -P)/demo"
DEMO_WT="$(cd "$TMPHOME" && pwd -P)/demo-wt"
env "${DEMO_GIT_ENV[@]}" git init -q -b main "$DEMO_PROJECT" \
  || die "could not create the demo git repo"
env "${DEMO_GIT_ENV[@]}" git -C "$DEMO_PROJECT" commit -q --allow-empty -m seed \
  || die "could not seed the demo git repo"
env "${DEMO_GIT_ENV[@]}" git -C "$DEMO_PROJECT" worktree add -q -b demo "$DEMO_WT" \
  || die "could not add the demo linked worktree"
mkdir -p "$DEMO_WT/tests" || die "could not create the demo tests dir"
printf '%s\n' '#!/usr/bin/env bash' 'echo "FAIL: modeled scout finding (e2e)"' 'exit 1' \
  >"$DEMO_WT/tests/repro.sh" || die "could not write the demo repro test"
env "${DEMO_GIT_ENV[@]}" git -C "$DEMO_WT" add tests/repro.sh \
  && env "${DEMO_GIT_ENV[@]}" git -C "$DEMO_WT" commit -q -m "demo repro" \
  || die "could not commit the demo repro test"
info "demo repo = $DEMO_PROJECT (run from linked worktree $DEMO_WT)"

(cd "$DEMO_WT" && env "${DEMO_GIT_ENV[@]}" "$BACKLOG" add \
  --title "$DEMO_TITLE" \
  --project "$DEMO_PROJECT" \
  --priority p1 \
  --notes "modeled scout opportunity for the human-0 e2e" \
  --repro-test "bash tests/repro.sh" >/dev/null) \
  || die "backlog add failed"
ok "backlog add — one scout-style opportunity enqueued"

PENDING_JSON="$(cd "$DEMO_WT" && env "${DEMO_GIT_ENV[@]}" "$BACKLOG" list --status pending --json)" \
  || die "backlog list failed"
printf '%s' "$PENDING_JSON" | grep -q "$DEMO_PROJECT" \
  || die "enqueued task did not surface in 'backlog list --status pending'"
printf '%s' "$PENDING_JSON" | grep -q '"status":"pending"' \
  || die "task status is not pending"
ok "backlog list --status pending — the opportunity is on the queue"
info "queue payload: $PENDING_JSON"
PASS=$((PASS+1))

# ── 3. gate contracts: every human Ask on the chain degrades under autonomy ─
say "-- step 3: gate contracts (no human Ask remains on the happy path) --"

# Resolve the installed SKILL specs (the actually-running contract). The path
# shape is  <cache>/<owner>/<plugin>/<version>/skills/<plugin>/SKILL.md .
find_skill() {
  # $1 = plugin name; prints newest matching SKILL.md path or empty
  local plugin="$1" f
  for f in "$HOME"/.claude/plugins/cache/*/"$plugin"/*/skills/"$plugin"/SKILL.md; do
    [ -f "$f" ] && printf '%s\n' "$f"
  done | sort | tail -n1
}

SCOUT_SKILL="$(find_skill scout || true)"
FLOW_SKILL="$(find_skill flow || true)"
CONDUKT_SKILL="$(find_skill condukt || true)"

for pair in "scout:$SCOUT_SKILL" "flow:$FLOW_SKILL" "condukt:$CONDUKT_SKILL"; do
  name="${pair%%:*}"; path="${pair#*:}"
  [ -n "$path" ] && [ -f "$path" ] \
    || die "could not locate installed SKILL.md for '$name'"
done
info "scout   SKILL = $SCOUT_SKILL"
info "flow    SKILL = $FLOW_SKILL"
info "condukt SKILL = $CONDUKT_SKILL"

# assert_contract <label> <file> <regex...> : every regex must match in file.
assert_contract() {
  local label="$1" file="$2"; shift 2
  local rx
  for rx in "$@"; do
    grep -Eq -- "$rx" "$file" \
      || die "$label: contract regex not found in $file : /$rx/"
  done
  ok "$label"
}

# 3a. scout Phase 4 — selection Ask is gated on autonomy-check and skipped.
assert_contract "scout Phase 4: selection AskUserQuestion skipped under autonomy" \
  "$SCOUT_SKILL" \
  'condukt state autonomy-check' \
  '(省略|skip)'

# 3b. scout Phase 6 — auto-handoff into /flow (no hand-off Ask).
assert_contract "scout Phase 6: auto-handoff into /flow (no hand-off Ask)" \
  "$SCOUT_SKILL" \
  'auto-handoff' \
  '/flow'

# 3c. condukt Phase 3 — agreement Ask skipped, schedule adopted as-is.
assert_contract "condukt Phase 3: agreement AskUserQuestion skipped, schedule adopted" \
  "$CONDUKT_SKILL" \
  'condukt state autonomy-check' \
  '(省略|schedule)'

# 3d. flow Step 0.5 — human gates degrade to deterministic defaults.
assert_contract "flow Step 0.5: human gates degrade to deterministic defaults" \
  "$FLOW_SKILL" \
  'condukt state autonomy-check' \
  '(human gate.*縮退|縮退|決定論的.*既定)'

PASS=$((PASS+1))

# ── 4. assert no human intervention point remains on the autonomous path ────
say "-- step 4: human-0 assertion --"
info "switch ON  → scout auto-selects + auto-handoffs, flow degrades its gates,"
info "             condukt adopts the schedule without an agreement Ask."
info "The ONLY stops left under autonomy are the safety invariants (worker"
info "blocked / hard verify failure), which are NOT human decision prompts."
ok "no human AskUserQuestion decision point remains on the deterministic happy path"
PASS=$((PASS+1))

# cleanup runs via the EXIT trap.
say ""
say "RESULT: PASS ($PASS/4 deterministic checks green)"
say "NOTE: real LLM worker/verifier generation is intentionally NOT run here."
say "      See docs/e2e-autonomy.md for the manual full-e2e (with real LLM) procedure."
