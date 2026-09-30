#!/usr/bin/env python3
"""Ratchet the number of crates that ADOPT harness-core's `fault-injection` seam
(backlog 8696dd7e, slice 3).

Why: `harness_core::boundary::fault` (FaultPlan / with_fault_plan) and
`harness_core::degrade::assert_fails_closed` let a gate crate's tests force every
IO entrance to `Undetermined` and assert the verdict does not get MORE permissive.
That is the only mechanical check for the class the type seal cannot close (an
`Err` arm that mints `Clean` through a legitimate API). The seam does nothing
unless crates opt in, so this script counts the opt-ins and refuses to let the
count move without the committed baseline moving with it.

Adoption (per crate, `crates/<c>/Cargo.toml`, parsed with tomllib — comments and
strings elsewhere never count):
  a crate ADOPTS iff `harness-core` appears in `[dev-dependencies]` (or a
  `[target.<cfg>.dev-dependencies]` table) with `features` containing
  "fault-injection". A dependency renamed via `package = "harness-core"` counts
  the same way.

Excluded: `crates/harness-core` itself. It PROVIDES the seam (it declares the
`fault-injection` feature and enables it on its own dev self-dependency so its
own tests/fault_plan.rs can run); counting it would make the ratchet start at 1
with zero consumers and let one real adopter's regression hide behind the
provider.

Shipped-feature rejection (exit 1 regardless of count): the feature must never
reach a released binary, so it is rejected when enabled for harness-core in
`[dependencies]` / `[build-dependencies]` (or their `[target.*]` forms), when
forwarded from a crate's own `[features]` table ("harness-core/fault-injection"
or "harness-core?/fault-injection"), or when enabled in the root
`[workspace.dependencies]` entry (which every `workspace = true` user inherits).

Exit codes (same shape as check-raw-io-ratchet.py):
  0  count == baseline and nothing ships the feature
  1  count < baseline (adoption regressed), count > baseline (raise the
     baseline IN THE SAME COMMIT so a later drop is caught), or the feature is
     enabled outside dev-dependencies
  2  UNDETERMINED: no crates/ dir, zero manifests, an unreadable/unparseable
     manifest, a crates/<c>/ without Cargo.toml that the root workspace does not
     explicitly exclude, or a missing/empty/non-integer/negative baseline.
     Not being able to count is never a pass.

Usage:
  python3 scripts/check-fault-injection-adoption.py                  # gate (0/1/2)
  python3 scripts/check-fault-injection-adoption.py --list           # print adopters, then gate as usual
  python3 scripts/check-fault-injection-adoption.py --update-baseline  # re-pin to the current count (refuses on 2 or shipped)
"""
from __future__ import annotations

import sys
from pathlib import Path

try:
    import tomllib
except ImportError:  # python < 3.11: cannot parse manifests -> cannot count
    print(
        "fault-injection-adoption: UNDETERMINED — python3 lacks tomllib (needs >= 3.11)",
        file=sys.stderr,
    )
    sys.exit(2)

REPO = Path(__file__).resolve().parent.parent
BASELINE_FILE = REPO / "scripts" / "check-fault-injection-adoption.baseline"
FEATURE = "fault-injection"
CORE = "harness-core"
# The provider of the seam — see module docstring for why it is not counted.
EXCLUDED_CRATES = frozenset({CORE})

LABEL = "fault-injection-adoption"


class Undetermined(Exception):
    """The count or the baseline could not be established."""


def _load_toml(path: Path) -> dict:
    try:
        with path.open("rb") as fh:
            return tomllib.load(fh)
    except OSError as e:
        raise Undetermined(f"cannot read {path.relative_to(REPO)}: {e}") from e
    except tomllib.TOMLDecodeError as e:
        raise Undetermined(f"cannot parse {path.relative_to(REPO)}: {e}") from e


def _core_features(table: object, where: str) -> list[str] | None:
    """Return the features enabled on harness-core in a dependency table, or None
    if harness-core is not in it. Raise Undetermined on a shape we cannot read."""
    if table is None:
        return None
    if not isinstance(table, dict):
        raise Undetermined(f"{where} is not a table")
    found: list[str] | None = None
    for key, spec in table.items():
        is_core = key == CORE or (isinstance(spec, dict) and spec.get("package") == CORE)
        if not is_core:
            continue
        if isinstance(spec, str):  # `harness-core = "0.2"` — no features
            feats: list[str] = []
        elif isinstance(spec, dict):
            raw = spec.get("features", [])
            if not isinstance(raw, list) or not all(isinstance(f, str) for f in raw):
                raise Undetermined(f"{where}.{key}.features is not a list of strings")
            feats = raw
        else:
            raise Undetermined(f"{where}.{key} has an unreadable shape")
        found = (found or []) + feats
    return found


def _dep_tables(manifest: dict, kind: str) -> list[tuple[str, object]]:
    """`[<kind>]` plus every `[target.<cfg>.<kind>]`."""
    out: list[tuple[str, object]] = [(kind, manifest.get(kind))]
    target = manifest.get("target", {})
    if not isinstance(target, dict):
        raise Undetermined("[target] is not a table")
    for cfg, body in target.items():
        if not isinstance(body, dict):
            raise Undetermined(f"[target.{cfg}] is not a table")
        out.append((f"target.{cfg}.{kind}", body.get(kind)))
    return out


def _workspace_excludes() -> set[str]:
    root = REPO / "Cargo.toml"
    if not root.exists():
        return set()
    data = _load_toml(root)
    ws = data.get("workspace", {})
    excl = ws.get("exclude", []) if isinstance(ws, dict) else []
    if not isinstance(excl, list):
        raise Undetermined("root [workspace].exclude is not a list")
    return {Path(e).name for e in excl if isinstance(e, str) and Path(e).parent.name == "crates"}


def scan() -> tuple[list[str], list[str]]:
    """Return (adopters, shipped_violations). Raise Undetermined if not countable."""
    crates_dir = REPO / "crates"
    if not crates_dir.is_dir():
        raise Undetermined("no crates/ directory — nothing could be counted")
    try:
        dirs = sorted(p for p in crates_dir.iterdir() if p.is_dir())
    except OSError as e:
        raise Undetermined(f"cannot list crates/: {e}") from e

    excludes = _workspace_excludes()
    adopters: list[str] = []
    shipped: list[str] = []

    # Root [workspace.dependencies]: a feature enabled here ships to every
    # `workspace = true` consumer.
    root = REPO / "Cargo.toml"
    if root.exists():
        ws = _load_toml(root).get("workspace", {})
        wdeps = ws.get("dependencies") if isinstance(ws, dict) else None
        feats = _core_features(wdeps, "workspace.dependencies")
        if feats and FEATURE in feats:
            shipped.append("Cargo.toml [workspace.dependencies] harness-core")

    manifests = 0
    for d in dirs:
        name = d.name
        cargo = d / "Cargo.toml"
        if not cargo.exists():
            if name in excludes:
                continue  # skill-only plugin, declared non-crate by the workspace
            raise Undetermined(
                f"crates/{name}/ has no Cargo.toml and is not in the root workspace exclude list"
            )
        manifests += 1
        if name in EXCLUDED_CRATES:
            continue
        m = _load_toml(cargo)
        rel = f"crates/{name}/Cargo.toml"

        for kind in ("dependencies", "build-dependencies"):
            for where, table in _dep_tables(m, kind):
                feats = _core_features(table, where)
                if feats and FEATURE in feats:
                    shipped.append(f"{rel} [{where}] harness-core")

        own_features = m.get("features", {})
        if not isinstance(own_features, dict):
            raise Undetermined(f"{rel} [features] is not a table")
        for fname, enables in own_features.items():
            if not isinstance(enables, list):
                raise Undetermined(f"{rel} [features].{fname} is not a list")
            for e in enables:
                if isinstance(e, str) and e in (f"{CORE}/{FEATURE}", f"{CORE}?/{FEATURE}"):
                    shipped.append(f"{rel} [features].{fname} forwards {e}")

        adopted = False
        for where, table in _dep_tables(m, "dev-dependencies"):
            feats = _core_features(table, where)
            if feats and FEATURE in feats:
                adopted = True
        if adopted:
            adopters.append(name)

    if manifests == 0:
        raise Undetermined("crates/ contains no Cargo.toml — zero manifests is not zero adopters")
    return adopters, shipped


def read_baseline() -> int:
    try:
        text = BASELINE_FILE.read_text()
    except OSError as e:
        raise Undetermined(f"cannot read baseline {BASELINE_FILE.relative_to(REPO)}: {e}") from e
    s = text.strip()
    if not s:
        raise Undetermined(f"baseline {BASELINE_FILE.relative_to(REPO)} is empty")
    try:
        n = int(s)
    except ValueError as e:
        raise Undetermined(f"baseline {BASELINE_FILE.relative_to(REPO)} is not an integer: {s!r}") from e
    if n < 0:
        raise Undetermined(f"baseline {BASELINE_FILE.relative_to(REPO)} is negative: {n}")
    return n


def main(argv: list[str]) -> int:
    args = set(argv[1:])
    unknown = args - {"--list", "--update-baseline"}
    if unknown:
        print(f"{LABEL}: unknown argument(s): {' '.join(sorted(unknown))}", file=sys.stderr)
        return 2

    try:
        adopters, shipped = scan()
    except Undetermined as e:
        print(f"{LABEL}: UNDETERMINED — {e}", file=sys.stderr)
        return 2

    count = len(adopters)
    if "--list" in args:
        for a in adopters:
            print(a)
        print(f"{count} adopter(s)")

    if shipped:
        print(f"{LABEL}: `{FEATURE}` is enabled outside [dev-dependencies] — it would ship", file=sys.stderr)
        print("  in a released binary. Enable it only under [dev-dependencies]:", file=sys.stderr)
        for s in shipped:
            print(f"    {s}", file=sys.stderr)
        return 1

    if "--update-baseline" in args:
        BASELINE_FILE.write_text(f"{count}\n")
        print(f"{LABEL}: baseline re-pinned to {count}")
        return 0

    try:
        baseline = read_baseline()
    except Undetermined as e:
        print(f"{LABEL}: UNDETERMINED — {e}", file=sys.stderr)
        return 2

    if count < baseline:
        print(
            f"{LABEL}: adoption REGRESSED — {count} crate(s) enable harness-core `{FEATURE}` "
            f"in [dev-dependencies], baseline is {baseline}.",
            file=sys.stderr,
        )
        print("  Restore the dropped opt-in (run with --list to see the current adopters).", file=sys.stderr)
        return 1
    if count > baseline:
        print(
            f"{LABEL}: adoption rose to {count} but the baseline is still {baseline}.",
            file=sys.stderr,
        )
        print(
            f"  Raise scripts/check-fault-injection-adoption.baseline to {count} IN THIS SAME COMMIT\n"
            "  (python3 scripts/check-fault-injection-adoption.py --update-baseline). The ratchet\n"
            "  must track the real count exactly, or a later drop back to the old pin passes unseen.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
