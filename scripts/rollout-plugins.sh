#!/usr/bin/env bash
# Fully automate the `/plugin update` step for the `yukineko` LOCAL DIRECTORY
# marketplace, so no manual UI action is needed to roll repo changes out to
# the running harness.
#
# WHY THIS EXISTS
#   `~/.claude/plugins/known_marketplaces.json` registers `yukineko` as a
#   directory marketplace pointing straight at this repo. `/plugin update`
#   (a UI-only action) does exactly two file operations for such a
#   marketplace, and both are entirely scriptable:
#     1. plain-copies the repo's `crates/<name>/` into
#        `~/.claude/plugins/cache/yukineko/<name>/<version>/` (a fresh dir
#        per version — old version dirs are never touched/deleted).
#     2. repoints `~/.claude/plugins/installed_plugins.json` so
#        `"<name>@yukineko"` names the new version dir.
#   This script does both, then (unless disabled) runs `rebuild-plugins.sh`
#   to swap in freshly built binaries and each plugin's
#   `scripts/sync-plugin-assets.sh` to refresh skills/hooks/agents — the same
#   three steps a human normally does by hand after `/plugin update`.
#
#   Order matters: the version dir + registry pointer must exist BEFORE
#   rebuild-plugins.sh runs, because rebuild-plugins.sh only *swaps binaries
#   into existing* cache bin files — it does not create version dirs.
#
# USAGE
#   scripts/rollout-plugins.sh                  # roll out every plugin
#   scripts/rollout-plugins.sh --dry-run         # show actions, write nothing
#   scripts/rollout-plugins.sh --force           # recopy + re-point even if unchanged
#   scripts/rollout-plugins.sh --plugin condukt  # limit to one plugin (repeatable)
#   scripts/rollout-plugins.sh --no-rebuild      # copy + registry only, skip rebuild
#   scripts/rollout-plugins.sh --no-sync         # copy + registry only, skip asset sync
#
# CANARY (opt-in for non-gate crates — default behavior is UNCHANGED without
#         --canary; REQUIRED when the target set includes a GATE crate)
#   scripts/rollout-plugins.sh --canary                     # staged rollout
#   scripts/rollout-plugins.sh --canary --canary-stage-size 2
#   scripts/rollout-plugins.sh --canary --canary-threshold 3
#   scripts/rollout-plugins.sh --plugin specguard --canary  # gate crate: --canary required
#   scripts/rollout-plugins.sh --plugin specguard --no-canary  # explicit override (escape hatch)
#   With --canary, the plugin set is split into ordered STAGES (via
#   `overwatch canary-plan`). Each stage is copied+repointed, then BETWEEN
#   stages the item-B violation registry is checked via `overwatch
#   canary-gate`. The gate emits a COMBINED verdict carrying BOTH a raw-spike
#   AND a systemic (fleet-recurrence) signal and rolls back if EITHER fires
#   (Problem-2.1); the count is anchored to each stage's deploy time via
#   --since so pre-deploy violations are not misattributed (Problem-2.2). On a
#   rollback the just-applied stage is AUTO-ROLLED-BACK (prior version dir
#   re-pointed) and the rollout halts. Without --canary none of this runs and
#   the script behaves exactly as it always has for NON-gate crates. Combine
#   with --dry-run to preview the staged plan + rollback plan and mutate
#   nothing.
#
# GATE CRATES require a canary (Problem-2.3)
#   The prompt-injection / spec / mutation DEFENSE gates (per docs/GLOSSARY.md:
#   blastguard, propguard, specguard, stuckguard; also the non-plugin
#   mutategate) guard the fleet, so rolling one out WITHOUT a canary is an
#   ERROR. Pass --canary to stage it, or --no-canary to explicitly override.
#   A rollout with no --plugin filter targets EVERY plugin (which includes gate
#   crates), so it too requires --canary / --no-canary. Non-gate crates are
#   unaffected — canary stays optional for them.
#
# ENV
#   CLAUDE_PLUGIN_CACHE     owner-scoped plugin cache root
#                           (default: ~/.claude/plugins/cache/yukineko)
#   CLAUDE_PLUGIN_REGISTRY  path to installed_plugins.json
#                           (default: ~/.claude/plugins/installed_plugins.json)
#   OVERWATCH_BIN           path to the overwatch binary used for canary
#                           planning/gating (default: auto-detect on PATH,
#                           then target/{release,debug}/overwatch)
#
# SAFETY
#   - Idempotent: re-running with no version change is a no-op.
#   - Never deletes old version dirs, never touches other plugins' registry
#     entries, never touches the registry's top-level "version" field.
#   - Excludes target/, .git/, and .in_use/ (a runtime lock dir the Claude
#     Code plugin loader creates *inside* a live cache version dir — not part
#     of repo source, must survive a --force recopy of an in-use version).
#   - Registry write is atomic (temp file + os.replace) and backed up first
#     to installed_plugins.json.bak-<epoch>; the written file is re-parsed
#     after writing and, if invalid, the backup is restored and the script
#     exits non-zero.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

OWNER=yukineko
CACHE="${CLAUDE_PLUGIN_CACHE:-$HOME/.claude/plugins/cache/yukineko}"
REGISTRY="${CLAUDE_PLUGIN_REGISTRY:-$HOME/.claude/plugins/installed_plugins.json}"
MARKETPLACE="$REPO/.claude-plugin/marketplace.json"

usage() { sed -n '2,80p' "$0"; }

# --- GATE CRATES (Problem-2.3) ----------------------------------------------
# The prompt-injection / spec / mutation DEFENSE gates, per docs/GLOSSARY.md
# (the canonical source: crates classified/described as "gate" — blastguard,
# propguard, specguard, stuckguard — plus the mutation-testing kill-rate gate
# `mutategate`, which is a non-plugin here but listed for completeness). These
# guard the fleet itself, so they MUST NOT roll out without a canary: when the
# target set includes any of them, --canary becomes REQUIRED (omitting it is an
# ERROR). `--no-canary` is the explicit escape hatch. Non-gate crates are
# unaffected (canary stays optional).
GATE_CRATES="blastguard propguard specguard stuckguard mutategate"

is_gate_crate() {
  local want="$1" g
  for g in $GATE_CRATES; do
    [ "$g" = "$want" ] && return 0
  done
  return 1
}

dry=0 force=0 no_rebuild=0 no_sync=0
canary=0 canary_stage_size=1 canary_threshold=2 canary_systemic_threshold=0 no_canary=0
declare -a only_plugins=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)    dry=1; shift ;;
    --force)      force=1; shift ;;
    --plugin)     [ $# -ge 2 ] || { echo "--plugin requires a name" >&2; exit 2; }
                  only_plugins+=("$2"); shift 2 ;;
    --no-rebuild) no_rebuild=1; shift ;;
    --no-sync)    no_sync=1; shift ;;
    --canary)     canary=1; shift ;;
    --no-canary)  no_canary=1; shift ;;
    --canary-stage-size)
                  [ $# -ge 2 ] || { echo "--canary-stage-size requires N" >&2; exit 2; }
                  canary_stage_size="$2"; shift 2 ;;
    --canary-threshold)
                  [ $# -ge 2 ] || { echo "--canary-threshold requires N" >&2; exit 2; }
                  canary_threshold="$2"; shift 2 ;;
    --canary-systemic-threshold)
                  [ $# -ge 2 ] || { echo "--canary-systemic-threshold requires N" >&2; exit 2; }
                  canary_systemic_threshold="$2"; shift 2 ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

# --- locate the overwatch binary (canary planning/gating) --------------------
# Only needed for --canary. Prefer an explicit override, then PATH, then a
# freshly built target binary. Deterministic core lives in overwatch.
resolve_overwatch_bin() {
  if [ -n "${OVERWATCH_BIN:-}" ] && [ -x "${OVERWATCH_BIN}" ]; then
    echo "$OVERWATCH_BIN"; return 0
  fi
  if command -v overwatch >/dev/null 2>&1; then
    command -v overwatch; return 0
  fi
  for cand in "$REPO/target/release/overwatch" "$REPO/target/debug/overwatch"; do
    [ -x "$cand" ] && { echo "$cand"; return 0; }
  done
  return 1
}

if [ ! -f "$MARKETPLACE" ]; then
  echo "marketplace file not found: $MARKETPLACE" >&2
  exit 1
fi
if [ ! -f "$REGISTRY" ]; then
  echo "installed_plugins.json not found: $REGISTRY" >&2
  echo "(set CLAUDE_PLUGIN_REGISTRY to override, or install at least one plugin first)" >&2
  exit 1
fi

GIT_SHA="$(git -C "$REPO" rev-parse HEAD)"

echo "repo:        $REPO"
echo "cache:       $CACHE"
echo "registry:    $REGISTRY"
echo "dry-run:     $([ $dry = 1 ] && echo yes || echo no)   force: $([ $force = 1 ] && echo yes || echo no)   no-rebuild: $([ $no_rebuild = 1 ] && echo yes || echo no)   no-sync: $([ $no_sync = 1 ] && echo yes || echo no)"
[ "$canary" = 1 ] && echo "canary:      yes   stage-size: $canary_stage_size   threshold: $canary_threshold"
[ "$no_canary" = 1 ] && echo "canary:      disabled (--no-canary override)"
[ "${#only_plugins[@]}" -gt 0 ] && echo "plugins:     ${only_plugins[*]}"
echo

# --- fail closed before touching anything if the registry is unparseable ----
# (don't half-apply: a copy must never happen if we can't safely repoint the
# registry afterwards)
if ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$REGISTRY" 2>/dev/null; then
  echo "installed_plugins.json is not valid JSON: $REGISTRY" >&2
  echo "refusing to proceed (fix it, or restore from a .bak-* backup, then retry)" >&2
  exit 1
fi

# --- validate --plugin filter against the marketplace -----------------------
all_names="$(python3 - "$MARKETPLACE" <<'PY'
import json, sys
mp = json.load(open(sys.argv[1]))
for p in mp.get("plugins", []):
    n = p.get("name")
    if n:
        print(n)
PY
)"
if [ "${#only_plugins[@]}" -gt 0 ]; then
  for want in "${only_plugins[@]}"; do
    if ! grep -qxF "$want" <<<"$all_names"; then
      echo "unknown plugin (not in marketplace.json): $want" >&2
      exit 2
    fi
  done
fi

# --- Problem-2.3: mandatory --canary when the target set includes a gate crate
# Determine the effective target set: the explicit --plugin list, or (when none
# given) every marketplace plugin. If that set contains any GATE crate, refuse
# to roll it out without a canary — unless --no-canary explicitly overrides.
# This runs BEFORE any mutation (fail-closed), and --dry-run keeps working (the
# check only gates the DECISION to require canary; it never itself mutates).
declare -a target_names=()
if [ "${#only_plugins[@]}" -gt 0 ]; then
  target_names=("${only_plugins[@]}")
else
  while IFS= read -r _n; do
    [ -n "$_n" ] && target_names+=("$_n")
  done <<<"$all_names"
fi
declare -a targeted_gates=()
for _t in ${target_names[@]+"${target_names[@]}"}; do
  if is_gate_crate "$_t"; then
    targeted_gates+=("$_t")
  fi
done
if [ "${#targeted_gates[@]}" -gt 0 ] && [ "$canary" != 1 ] && [ "$no_canary" != 1 ]; then
  echo "ERROR: refusing to roll out gate crate(s) without a canary: ${targeted_gates[*]}" >&2
  echo "       Gate crates (prompt-injection / spec / mutation defenses) guard the fleet and" >&2
  echo "       must be staged behind a canary health gate. Re-run with --canary to stage the" >&2
  echo "       rollout, or --no-canary to explicitly override (escape hatch)." >&2
  exit 2
fi

# --- plan: one TSV row per plugin --------------------------------------------
# name  version  src  target  needs_copy  needs_registry  mismatch  mpver  pjver  cur_version  cur_path
plan() {
  python3 - "$REPO" "$CACHE" "$OWNER" "$REGISTRY" "$force" ${only_plugins[@]+"${only_plugins[@]}"} <<'PY'
import json, os, sys

repo, cache, owner, registry_path, force = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5] == "1"
only = set(sys.argv[6:])

mp = json.load(open(os.path.join(repo, ".claude-plugin", "marketplace.json")))
reg_plugins = {}
if os.path.isfile(registry_path):
    try:
        reg_plugins = json.load(open(registry_path)).get("plugins", {}) or {}
    except Exception:
        reg_plugins = {}

for p in mp.get("plugins", []):
    name = p.get("name")
    src = (p.get("source") or {}).get("path")
    mpver = p.get("version")
    if not name or not src:
        continue
    if only and name not in only:
        continue

    pj_path = os.path.join(repo, src, ".claude-plugin", "plugin.json")
    pjver = None
    if os.path.isfile(pj_path):
        try:
            pjver = json.load(open(pj_path)).get("version")
        except Exception:
            pjver = None

    version = pjver or mpver or ""
    mismatch = "1" if (pjver and mpver and pjver != mpver) else "0"
    target = os.path.join(cache, name, version)

    needs_copy = "1" if (force or not os.path.isdir(target)) else "0"

    key = f"{name}@{owner}"
    entries = reg_plugins.get(key) or []
    cur = entries[0] if entries else None
    cur_version = (cur or {}).get("version", "")
    cur_path = (cur or {}).get("installPath", "")
    needs_registry = "1" if (force or cur is None or cur_version != version or cur_path != target) else "0"

    print("\t".join([name, version, src, target, needs_copy, needs_registry, mismatch,
                      mpver or "", pjver or "", cur_version, cur_path]))
PY
}

# --- copy a repo plugin dir into a cache version dir -------------------------
# Excludes target/, .git/, .in_use/ (see header). rsync preferred; cp -a fallback.
copy_plugin_dir() {
  local src="$1" dst="$2"
  mkdir -p "$dst"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete --exclude '/target/' --exclude '/.git/' --exclude '/.in_use/' "$src/" "$dst/"
  else
    find "$dst" -mindepth 1 -maxdepth 1 ! -name '.in_use' -exec rm -rf {} +
    cp -a "$src/." "$dst/"
    rm -rf "${dst:?}/target" "${dst:?}/.git"
  fi
}

# --- registry patch (atomic, backed up, validated) ---------------------------
# Args: registry_path owner git_sha [--dry-run] name1 version1 target1 name2 version2 target2 ...
registry_patch() {
  python3 - "$@" <<'PY'
import json, os, shutil, sys, tempfile, time

args = sys.argv[1:]
dry = "--dry-run" in args
args = [a for a in args if a != "--dry-run"]
registry_path, owner, git_sha = args[0], args[1], args[2]
rest = args[3:]
if len(rest) % 3 != 0:
    print("registry_patch: malformed update args", file=sys.stderr)
    sys.exit(2)
updates = [{"name": rest[i], "version": rest[i + 1], "target": rest[i + 2]}
           for i in range(0, len(rest), 3)]
if not updates:
    print("registry: no changes needed")
    sys.exit(0)

if not os.path.isfile(registry_path):
    print(f"registry not found: {registry_path}", file=sys.stderr)
    sys.exit(1)

with open(registry_path) as f:
    reg = json.load(f)
plugins = reg.setdefault("plugins", {})


def now_iso():
    t = time.time()
    return time.strftime("%Y-%m-%dT%H:%M:%S.", time.gmtime(t)) + f"{int((t % 1) * 1000):03d}Z"


ts = now_iso()
changes = []
for u in updates:
    key = f"{u['name']}@{owner}"
    entries = plugins.get(key)
    if entries and isinstance(entries, list) and len(entries) > 0:
        entry = entries[0]
        old = dict(entry)
        entry["version"] = u["version"]
        entry["installPath"] = u["target"]
        entry["lastUpdated"] = ts
        entry["gitCommitSha"] = git_sha
        entry.setdefault("scope", "user")
    else:
        old = None
        entry = {
            "scope": "user",
            "installPath": u["target"],
            "version": u["version"],
            "installedAt": ts,
            "lastUpdated": ts,
            "gitCommitSha": git_sha,
        }
        plugins[key] = [entry]
    changes.append((key, old, entry))

for key, old, entry in changes:
    if old is None:
        verb = "would create" if dry else "created"
        print(f"registry {verb} {key}: version={entry['version']} installPath={entry['installPath']}")
    else:
        verb = "would update" if dry else "updated"
        print(f"registry {verb} {key}: version {old.get('version')!r} -> {entry['version']!r} | "
              f"installPath {old.get('installPath')!r} -> {entry['installPath']!r}")

if dry:
    sys.exit(0)

backup = f"{registry_path}.bak-{int(time.time())}"
backed_up = False
tmp = None
try:
    shutil.copy2(registry_path, backup)
    backed_up = True
    print(f"registry backup: {backup}")

    d = os.path.dirname(registry_path) or "."
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".installed_plugins.", suffix=".json.tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(reg, f, indent=2)
        f.write("\n")
    with open(tmp) as f:
        json.load(f)  # validate before swap
    os.replace(tmp, registry_path)
    tmp = None
    with open(registry_path) as f:
        json.load(f)  # validate the file that is now live
except Exception as e:
    if tmp and os.path.exists(tmp):
        os.unlink(tmp)
    if backed_up:
        shutil.copy2(backup, registry_path)
        print(f"registry write failed, restored from backup: {e}", file=sys.stderr)
    else:
        print(f"registry write failed before backup completed (original left untouched): {e}", file=sys.stderr)
    sys.exit(1)

print(f"registry patched: {registry_path}")
PY
}

# --- rollback target lookup (argv-safe, no string interpolation into Python) -
# Given a `canary-rollback-plan` JSON blob and a plugin name, print the prior
# version on line 1 and the restore install path on line 2 (both empty if the
# plugin is not found or is_new). The plugin name is passed as sys.argv[1] —
# NEVER interpolated into the Python source — so a name containing a quote,
# backslash, or other special character cannot break out of (or inject code
# into) the literal. This is the fix for the quote-injection bug where the
# name used to be spliced directly into a `t["name"]=="..."` string literal.
rollback_target_lookup() {
  local rbplan_json="$1" name="$2"
  RBPLAN_JSON_ENV="$rbplan_json" python3 - "$name" <<'PY'
import json, os, sys
name = sys.argv[1]
p = json.loads(os.environ["RBPLAN_JSON_ENV"])
for t in p.get("targets", []):
    if t.get("name") == name and not t.get("is_new"):
        print(t.get("prior_version") or "")
        print(t.get("restore_install_path") or "")
        break
else:
    print("")
    print("")
PY
}

# --- build prior/canary state JSON from plan rows (proper escaping, one place) -
# Takes plan TSV rows as ARGV (one arg per row — NOT stdin, because the `<<'PY'`
# heredoc already occupies python3's stdin, matching how plan()/registry_patch()
# pass their inputs) and emits TWO lines, both built via json.dumps (so a
# Windows-style path with backslashes, or any name/path containing a quote, is
# correctly escaped and can never corrupt the JSON — the finding-3 fix). Line 1
# = the `prior` array (state live NOW, before the canary moves it); line 2 =
# the `canary` array (target version/path). Both the initial canary plan and
# the rollback path use this single helper (the finding-14 dedup) so there is
# exactly one JSON-construction site for each shape.
build_state_json() {
  python3 - "$@" <<'PY'
import json, sys
prior, canary = [], []
for line in sys.argv[1:]:
    if not line:
        continue
    f = line.split("\t")
    # plan() row order: name version src target needs_copy needs_registry
    #                   mismatch mpver pjver cur_version cur_path
    name, version, target = f[0], f[1], f[3]
    cur_version, cur_path = f[9], f[10]
    prior.append({"name": name,
                  "prior_version": cur_version or None,
                  "prior_install_path": cur_path or None})
    canary.append({"name": name,
                   "canary_version": version,
                   "canary_install_path": target})
print(json.dumps(prior))
print(json.dumps(canary))
PY
}

# --- rebuild + asset sync (shared by the normal AND canary success paths) -----
# Extracted so a successful --canary rollout also swaps in freshly built
# binaries and refreshes skills/hooks/agents — the finding-4 fix (the canary
# path used to copy + repoint the registry and then exit 0 without ever
# rebuilding or syncing, silently leaving the running harness on stale
# binaries). Honors --no-rebuild / --no-sync / --dry-run exactly like the
# normal path. Args: zero or more "name:srcdir" entries for plugins that ship
# scripts/sync-plugin-assets.sh.
run_rebuild_and_sync() {
  echo
  if [ "$no_rebuild" = 1 ]; then
    echo "rebuild: skipped (--no-rebuild)"
  elif [ "$dry" = 1 ]; then
    echo "[dry-run] would run: scripts/rebuild-plugins.sh --no-clean (CLAUDE_PLUGIN_CACHE=$CACHE)"
  else
    echo ">>> scripts/rebuild-plugins.sh --no-clean"
    CLAUDE_PLUGIN_CACHE="$CACHE" bash "$REPO/scripts/rebuild-plugins.sh" --no-clean
  fi
  echo

  # sync-plugin-assets.sh's own CLAUDE_PLUGIN_CACHE default is the *owner-less*
  # cache root (it globs */<name>/<version>); pass the parent of our
  # owner-scoped $CACHE so it resolves the same dir whether or not the caller
  # overrode CLAUDE_PLUGIN_CACHE.
  local sync_cache_root; sync_cache_root="$(dirname "$CACHE")"
  if [ "$no_sync" = 1 ]; then
    echo "sync: skipped (--no-sync)"
  elif [ "$#" -eq 0 ]; then
    echo "sync: no plugin ships scripts/sync-plugin-assets.sh"
  else
    local entry pname pdir
    for entry in "$@"; do
      pname="${entry%%:*}"
      pdir="${entry#*:}"
      if [ "$dry" = 1 ]; then
        echo "[dry-run] would run: $pdir/scripts/sync-plugin-assets.sh (for $pname)"
      else
        echo ">>> $pname: scripts/sync-plugin-assets.sh"
        CLAUDE_PLUGIN_CACHE="$sync_cache_root" bash "$pdir/scripts/sync-plugin-assets.sh"
      fi
    done
  fi
  echo
}

# --- capture the plan once (used by both the normal and canary paths) --------
# One TSV row per plugin. Read into an array so the canary path can slice the
# ordered plugin set into stages without re-running `plan`.
declare -a PLAN_ROWS=()
while IFS= read -r _row; do
  [ -z "$_row" ] && continue
  PLAN_ROWS+=("$_row")
done < <(plan)

# =============================================================================
# CANARY STAGED ROLLOUT (opt-in) — only runs with --canary. This block is a
# SELF-CONTAINED alternative to the normal single-pass path below; it never
# runs unless --canary was given, so default behavior is byte-for-byte
# unchanged. Under --dry-run it prints the staged plan + rollback plan and
# mutates NOTHING. The real (non-dry-run) staged path exists so it's usable
# later under manual approval, but is reachable ONLY via this explicit flag.
# =============================================================================

# Copy ONE plugin row into its cache version dir (no registry write — the
# registry update for a whole stage is batched into a single registry_patch
# call by the caller, the finding-17 fix, so we don't spawn one python3
# subprocess per plugin). Honors --dry-run. Echoes what it did. Args: the TSV
# row fields as a single tab-delimited string.
canary_copy_row() {
  local row="$1"
  IFS=$'\t' read -r name version src target needs_copy needs_registry mismatch mpver pjver cur_version cur_path <<<"$row"
  local srcdir="$REPO/$src"
  [ -d "$srcdir" ] || { echo "WARN: source dir missing for $name: $srcdir — skipping" >&2; return 0; }

  if [ "$needs_copy" = "1" ] || [ "$force" = 1 ]; then
    if [ "$dry" = 1 ]; then
      echo "  [dry-run] would copy $srcdir/ -> $target/"
    else
      copy_plugin_dir "$srcdir" "$target"
      echo "  copied $name -> $target"
    fi
  fi
}

run_canary() {
  local ow
  if ! ow="$(resolve_overwatch_bin)"; then
    echo "canary: overwatch binary not found (set OVERWATCH_BIN, or build it: cargo build -p overwatch)" >&2
    echo "        refusing to run a staged rollout without the deterministic canary core" >&2
    exit 1
  fi
  echo "canary: using overwatch binary: $ow"

  # Ordered plugin list + a name->row index for stage slicing.
  local -a ordered_names=()
  declare -A row_by_name=()
  local r name version src target needs_copy needs_registry mismatch mpver pjver cur_version cur_path
  for r in "${PLAN_ROWS[@]}"; do
    IFS=$'\t' read -r name version src target needs_copy needs_registry mismatch mpver pjver cur_version cur_path <<<"$r"
    [ -z "$name" ] && continue
    ordered_names+=("$name")
    row_by_name["$name"]="$r"
  done

  if [ "${#ordered_names[@]}" -eq 0 ]; then
    echo "canary: no plugins to roll out"
    return 0
  fi

  # prior-state / canary-target JSON, built once via json.dumps (finding-3
  # escaping + finding-14 single construction site). Rows go via argv.
  local state_json prior_json canary_json
  state_json="$(build_state_json "${PLAN_ROWS[@]}")"
  prior_json="$(sed -n '1p' <<<"$state_json")"
  canary_json="$(sed -n '2p' <<<"$state_json")"

  # 1. Ask overwatch to split the ordered set into stages (deterministic).
  local plan_json
  plan_json="$("$ow" canary-plan --plugins "$(IFS=,; echo "${ordered_names[*]}")" --stage-size "$canary_stage_size")"
  echo "=== canary stage plan ==="
  echo "$plan_json"

  # 2. Ask overwatch what a rollback of the whole set would restore (as data).
  echo "=== canary rollback plan (data only — not executed) ==="
  "$ow" canary-rollback-plan --stage-index 0 --prior "$prior_json" --canary-targets "$canary_json"

  # Number of stages from the plan JSON.
  local nstages
  nstages="$(python3 -c 'import json,sys; print(len(json.load(sys.stdin)["stages"]))' <<<"$plan_json")"
  echo
  echo "canary: $nstages stage(s), stage-size=$canary_stage_size, threshold=$canary_threshold"

  # 3. Walk stages: apply → health-gate → (proceed | rollback + halt).
  local s
  for (( s=0; s<nstages; s++ )); do
    local stage_names
    stage_names="$(python3 -c 'import json,sys; print(" ".join(json.load(sys.stdin)["stages"]['"$s"']["plugins"]))' <<<"$plan_json")"
    echo
    echo "--- stage $s: $stage_names ---"
    # Problem-2.2: capture the pre-stage DEPLOY timestamp (epoch seconds) BEFORE
    # this stage is applied, and pass it to the health gate as --since so the
    # gate only counts violations at/after the deploy. Violations that predate
    # the stage are no longer misattributed to the canary. Fail-soft: if `date`
    # is somehow unavailable, leave the anchor empty (gate falls back to no
    # lower bound — exactly the pre-fix behavior), never crashing the rollout.
    #
    # OVERWATCH_CANARY_SINCE (advanced/testing hook): when set, PINS the deploy
    # anchor to that epoch value instead of the wall clock, so a deterministic
    # test can control which seeded violations fall at/after the anchor. Unset
    # in normal operation (the auto-captured wall-clock deploy time is used).
    local stage_deploy_ts=""
    if [ -n "${OVERWATCH_CANARY_SINCE:-}" ]; then
      stage_deploy_ts="$OVERWATCH_CANARY_SINCE"
    else
      stage_deploy_ts="$(date +%s 2>/dev/null || true)"
    fi
    local pn
    # Copy each plugin in the stage, then batch the whole stage's registry
    # update into ONE registry_patch call (finding-17: no per-plugin python3
    # subprocess spawn).
    local -a stage_reg_args=()
    for pn in $stage_names; do
      canary_copy_row "${row_by_name[$pn]}"
      IFS=$'\t' read -r name version src target needs_copy needs_registry mismatch mpver pjver cur_version cur_path <<<"${row_by_name[$pn]}"
      if [ "$needs_registry" = "1" ] || [ "$force" = 1 ]; then
        stage_reg_args+=("$name" "$version" "$target")
      fi
    done
    if [ "${#stage_reg_args[@]}" -gt 0 ]; then
      if [ "$dry" = 1 ]; then
        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" --dry-run "${stage_reg_args[@]}" | sed 's/^/  /'
      else
        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" "${stage_reg_args[@]}" | sed 's/^/  /'
      fi
    fi

    # Health gate BETWEEN stages (skip the check after the final stage).
    if [ "$s" -lt "$((nstages - 1))" ]; then
      echo "  health-gate: checking violation rate (threshold=$canary_threshold)..."
      if [ "$dry" = 1 ]; then
        # Dry-run: observe 0 violations (nothing was really applied) so the
        # gate deterministically PROCEEDs and we exercise the full plan.
        "$ow" canary-gate --observed-violations 0 --threshold "$canary_threshold" | sed 's/^/  /' || true
        echo "  [dry-run] gate would PROCEED (no live violations observed)"
      else
        # Real path: consult the item-B violation registry for the cwd project.
        # The gate (default registry mode) emits a COMBINED verdict carrying
        # BOTH a raw-spike AND a systemic (fleet-recurrence) sub-verdict and
        # exits non-zero if EITHER fires (Problem-2.1) — so this single check
        # already honors both signals; we do NOT pass --systemic (which would
        # restrict to the single systemic-only path). The systemic arm uses its
        # OWN, lower threshold (Problem-2.1b: default 0 = any fleet-recurring
        # signature trips) so fleet recurrence can advise rollback independently
        # of the raw-spike count. --since anchors the count to this stage's
        # deploy time (Problem-2.2). A gate-eval error must not crash the
        # rollout: canary is observational, so on any non-rollback failure we
        # treat it as "no spike observed" and PROCEED (fail-soft).
        local -a gate_args=(canary-gate --threshold "$canary_threshold" \
          --systemic-threshold "$canary_systemic_threshold")
        [ -n "$stage_deploy_ts" ] && gate_args+=(--since "$stage_deploy_ts")
        local gate_out gate_rc=0
        gate_out="$("$ow" "${gate_args[@]}")" || gate_rc=$?
        echo "$gate_out" | sed 's/^/  /'
        # Exit 3 = rollback advised (raw OR systemic). Any OTHER non-zero code
        # is a gate-eval error (bad args, unreadable store, etc.) — fail-soft:
        # log and PROCEED rather than aborting the rollout on an observational
        # check.
        if [ "$gate_rc" -ne 0 ] && [ "$gate_rc" -ne 3 ]; then
          echo "  health-gate: eval error (rc=$gate_rc) — treating as no-spike and PROCEEDING (fail-soft)" >&2
          gate_rc=0
        fi
        if [ "$gate_rc" -ne 0 ]; then
          echo "  health-gate: ROLLBACK — raw-spike or systemic recurrence detected; rolling back stage $s and halting" >&2
          # Re-point the just-applied stage back to its prior version dir.
          # Build the prior/canary JSON for this stage through the SAME
          # json.dumps helper as the main path (finding-3 escaping +
          # finding-14 dedup). Rows go via argv.
          local pn
          local -a stage_rows=()
          for pn in $stage_names; do
            stage_rows+=("${row_by_name[$pn]}")
          done
          local stage_state prior_line cj
          stage_state="$(build_state_json "${stage_rows[@]}")"
          prior_line="$(sed -n '1p' <<<"$stage_state")"
          cj="$(sed -n '2p' <<<"$stage_state")"
          local rbplan
          rbplan="$("$ow" canary-rollback-plan --stage-index "$s" --prior "$prior_line" --canary-targets "$cj")"
          echo "  rollback plan for stage $s:"
          echo "$rbplan" | sed 's/^/    /'
          # Execute the rollback: re-point registry to each prior install path.
          # NOTE: the plugin name is passed via argv (sys.argv[1] inside
          # rollback_target_lookup), never string-interpolated into the
          # Python source, so a name containing a quote or other special
          # char cannot break out of (or inject into) the literal. The
          # restores for the whole stage are batched into ONE registry_patch
          # call (finding-17).
          local -a rb_reg_args=()
          for pn in $stage_names; do
            local rv rp lookup_out
            lookup_out="$(rollback_target_lookup "$rbplan" "$pn")"
            rv="$(sed -n '1p' <<<"$lookup_out")"
            rp="$(sed -n '2p' <<<"$lookup_out")"
            if [ -n "$rv" ] && [ -n "$rp" ]; then
              rb_reg_args+=("$pn" "$rv" "$rp")
            else
              echo "    $pn: newly introduced by canary — nothing to restore (left as-is)" >&2
            fi
            # Fail-soft: record an observational rollback event so
            # `overwatch review-queue` can surface it later. This never gates
            # or alters the rollback itself — a record failure is swallowed
            # (|| true) so the audit log can NEVER break a rollout. The canary
            # (from) version this stage moved the plugin to is field 2 of the
            # plugin's row; `rv` (may be empty for a newly-introduced plugin)
            # is the prior version we restored to. The gate here counts raw
            # violations (no --systemic on the canary-gate call), so reason=raw.
            local canary_ver=""
            IFS=$'\t' read -r _n canary_ver _rest <<<"${row_by_name[$pn]}"
            local -a rb_ev_args=(record-rollback --plugin "$pn"
              --to-version "$canary_ver" --stage "$s" --reason raw)
            if [ -n "$rv" ]; then
              rb_ev_args+=(--from-version "$rv")
            fi
            "$ow" "${rb_ev_args[@]}" >/dev/null 2>&1 || true
          done
          if [ "${#rb_reg_args[@]}" -gt 0 ]; then
            registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" "${rb_reg_args[@]}" | sed 's/^/    /'
          fi
          echo "canary: HALTED at stage $s after auto-rollback." >&2
          exit 4
        fi
        echo "  health-gate: PROCEED"
      fi
    fi
  done

  echo
  echo "canary: all $nstages stage(s) completed."

  # All stages completed without a rollback halt (the rollback branch exits 4
  # before reaching here), so finish the rollout exactly like the normal path:
  # swap in freshly built binaries and refresh skills/hooks/agents. Without
  # this the canary path would leave the running harness on stale binaries
  # (finding 4). Build the sync list from the rolled-out plugins that ship a
  # sync script (mirrors the normal path).
  local -a canary_synced=()
  local pn2 sname sver ssrc rest srcdir2
  for pn2 in "${ordered_names[@]}"; do
    IFS=$'\t' read -r sname sver ssrc rest <<<"${row_by_name[$pn2]}"
    srcdir2="$REPO/$ssrc"
    [ -f "$srcdir2/scripts/sync-plugin-assets.sh" ] && canary_synced+=("$sname:$srcdir2")
  done
  run_rebuild_and_sync ${canary_synced[@]+"${canary_synced[@]}"}
}

if [ "$canary" = 1 ]; then
  run_canary
  echo
  echo "done (canary)."
  exit 0
fi

# --- run the plan (default, non-canary path — UNCHANGED) ---------------------
declare -a reg_args=()
declare -a synced_plugins=()
any_reg_change=0

while IFS=$'\t' read -r name version src target needs_copy needs_registry mismatch mpver pjver cur_version cur_path; do
  [ -z "$name" ] && continue

  if [ "$mismatch" = "1" ]; then
    echo "WARN: version lockstep drift for $name — marketplace.json=$mpver plugin.json=$pjver (using plugin.json as truth)" >&2
  fi

  srcdir="$REPO/$src"
  if [ ! -d "$srcdir" ]; then
    echo "WARN: source dir missing for $name: $srcdir — skipping" >&2
    continue
  fi

  skill_only=1
  [ -f "$srcdir/Cargo.toml" ] && skill_only=0

  changed=0
  if [ "$needs_copy" = "1" ]; then
    changed=1
    if [ "$dry" = 1 ]; then
      echo "[dry-run] would copy $srcdir/ -> $target/ (rsync -a --delete, exclude target/ .git/ .in_use/)"
    else
      copy_plugin_dir "$srcdir" "$target"
      echo "copied $name -> $target"
    fi
  fi

  if [ "$needs_registry" = "1" ]; then
    changed=1
    any_reg_change=1
    reg_args+=("$name" "$version" "$target")
  fi

  if [ "$changed" = 1 ]; then
    echo "$name: created $version"
  elif [ "$skill_only" = 1 ]; then
    echo "$name: skip (skill-only, no change) $version"
  else
    echo "$name: already-current $version"
  fi

  if [ -f "$srcdir/scripts/sync-plugin-assets.sh" ]; then
    synced_plugins+=("$name:$srcdir")
  fi
done < <(plan)

echo
if [ "$any_reg_change" = 1 ]; then
  if [ "$dry" = 1 ]; then
    registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" --dry-run "${reg_args[@]}"
  else
    registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" "${reg_args[@]}"
  fi
else
  echo "registry: no changes needed"
fi

# --- rebuild + asset sync (shared with the canary success path) --------------
run_rebuild_and_sync ${synced_plugins[@]+"${synced_plugins[@]}"}

echo "done."
