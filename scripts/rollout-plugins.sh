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
# ENV
#   CLAUDE_PLUGIN_CACHE     owner-scoped plugin cache root
#                           (default: ~/.claude/plugins/cache/yukineko)
#   CLAUDE_PLUGIN_REGISTRY  path to installed_plugins.json
#                           (default: ~/.claude/plugins/installed_plugins.json)
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

usage() { sed -n '2,45p' "$0"; }

dry=0 force=0 no_rebuild=0 no_sync=0
declare -a only_plugins=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)    dry=1; shift ;;
    --force)      force=1; shift ;;
    --plugin)     [ $# -ge 2 ] || { echo "--plugin requires a name" >&2; exit 2; }
                  only_plugins+=("$2"); shift 2 ;;
    --no-rebuild) no_rebuild=1; shift ;;
    --no-sync)    no_sync=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

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

# --- run the plan --------------------------------------------------------------
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
echo

# --- rebuild: swap freshly built binaries into the (now-existing) cache dirs --
if [ "$no_rebuild" = 1 ]; then
  echo "rebuild: skipped (--no-rebuild)"
elif [ "$dry" = 1 ]; then
  echo "[dry-run] would run: scripts/rebuild-plugins.sh --no-clean (CLAUDE_PLUGIN_CACHE=$CACHE)"
else
  echo ">>> scripts/rebuild-plugins.sh --no-clean"
  CLAUDE_PLUGIN_CACHE="$CACHE" bash "$REPO/scripts/rebuild-plugins.sh" --no-clean
fi
echo

# --- sync: refresh skills/hooks/agents for plugins that ship a sync script ---
# sync-plugin-assets.sh's own CLAUDE_PLUGIN_CACHE default is the *owner-less*
# cache root (it globs */<name>/<version>); pass the parent of our
# owner-scoped $CACHE so it resolves the same dir whether or not the caller
# overrode CLAUDE_PLUGIN_CACHE.
SYNC_CACHE_ROOT="$(dirname "$CACHE")"
if [ "$no_sync" = 1 ]; then
  echo "sync: skipped (--no-sync)"
elif [ "${#synced_plugins[@]}" -eq 0 ]; then
  echo "sync: no plugin ships scripts/sync-plugin-assets.sh"
else
  for entry in "${synced_plugins[@]}"; do
    pname="${entry%%:*}"
    pdir="${entry#*:}"
    if [ "$dry" = 1 ]; then
      echo "[dry-run] would run: $pdir/scripts/sync-plugin-assets.sh (for $pname)"
    else
      echo ">>> $pname: scripts/sync-plugin-assets.sh"
      CLAUDE_PLUGIN_CACHE="$SYNC_CACHE_ROOT" bash "$pdir/scripts/sync-plugin-assets.sh"
    fi
  done
fi

echo
echo "done."
