#!/usr/bin/env python3
"""Reachability probe: replace one literal anchor in a source file with a panic
(or any --replacement), run the test suite, and report whether it went red.

  reproduced     <=> test cmd exits non-zero after mutation
  not_reproduced <=> test cmd exits 0 after mutation (does NOT prove unreachable)

Undetermined (exit 2, no result file): baseline red, anchor not found exactly
once, build failure after mutation (a compile failure is never a reproduction),
unreadable target, or restore that cannot be verified byte-identical (non-zero
exit, no result file). Verdicts come from subprocess exit codes only; the
output is scanned only for the panic marker, which is informational.
Exit 0 = result written. Result shape is accepted by overwatch parse_probe.
Test/build commands run in the caller's cwd.
"""
import argparse
import json
import signal
import subprocess
import sys
from pathlib import Path

MARKER = "reachability-probe"
DEFAULT_REPL = 'panic!("reachability-probe")'
REPO = Path(__file__).resolve().parent.parent


def sh(cmd):
    try:
        p = subprocess.run(cmd, shell=True, capture_output=True, text=True, errors="replace")
    except OSError as e:
        return None, f"spawn failed: {e}"
    return p.returncode, (p.stdout or "") + (p.stderr or "")


def undetermined(msg):
    print(f"reachability-probe: undetermined: {msg}", file=sys.stderr)
    return 2


def git_rev():
    try:
        p = subprocess.run(["git", "rev-parse", "HEAD"], cwd=str(REPO), capture_output=True, text=True)
        if p.returncode == 0 and p.stdout.strip():
            return p.stdout.strip()
    except OSError:
        pass
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--file", required=True)
    ap.add_argument("--anchor", required=True)
    ap.add_argument("--replacement", default=DEFAULT_REPL)
    ap.add_argument("--out", required=True)
    ap.add_argument("--test-cmd")
    ap.add_argument("--build-cmd")
    ap.add_argument("--crate")
    a = ap.parse_args()

    test_cmd, build_cmd = a.test_cmd, a.build_cmd
    if test_cmd is None or build_cmd is None:
        if not a.crate:
            return undetermined("--crate required when --test-cmd/--build-cmd omitted")
        test_cmd = test_cmd or f"cargo test -p {a.crate}"
        build_cmd = build_cmd or f"cargo test -p {a.crate} --no-run"

    def _on_signal(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")

    # SIGTERM/SIGINT unwind through the finally below (restore), then exit non-zero, no result.
    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGINT, _on_signal)

    target = Path(a.file)
    try:
        orig = target.read_bytes()
        text = orig.decode("utf-8")
    except (OSError, UnicodeDecodeError) as e:
        return undetermined(f"cannot read target: {e}")
    if not a.anchor:
        return undetermined("empty anchor")
    n = text.count(a.anchor)

    code, _ = sh(test_cmd)
    if code != 0:
        return undetermined(f"baseline test cmd exit {code}")
    if n != 1:
        return undetermined(f"anchor occurs {n} times (need exactly 1)")

    mutated = text.replace(a.anchor, a.replacement, 1).encode("utf-8")
    result = None
    err = None
    try:
        target.write_bytes(mutated)
        bcode, _ = sh(build_cmd)
        if bcode != 0:
            err = f"build after mutation exit {bcode}"
        else:
            tcode, out = sh(test_cmd)
            result = (tcode, out, bcode)
    except BaseException as e:  # restore still runs below
        err = f"error during probe: {e}"
    finally:
        restored = False
        try:
            target.write_bytes(orig)
            restored = target.read_bytes() == orig
        except OSError:
            restored = False
    if not restored:
        print(f"reachability-probe: FAILED to verify restore of {target}", file=sys.stderr)
        return 3
    if err or result is None:
        return undetermined(err or "no result")

    tcode, out, bcode = result
    reproduced = tcode != 0
    seen = MARKER in out
    note = "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red."
    if not reproduced and not seen:
        note += " The panic marker was not seen; catch_unwind may have swallowed the panic."
    if reproduced and not seen:
        note += " Test cmd failed but the panic marker was not seen; the failure may be unrelated to the injected panic."
    doc = {
        "result": "reproduced" if reproduced else "not_reproduced",
        "note": note,
        "panic_marker_seen": seen,
        "crate": a.crate,
        "file": str(target),
        "anchor": a.anchor,
        "replacement": a.replacement,
        "rev": git_rev(),
        "test_cmd": test_cmd,
        "build_cmd": build_cmd,
        "baseline_exit_code": 0,
        "build_exit_code": bcode,
        "test_exit_code": tcode,
    }
    try:
        Path(a.out).write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    except OSError as e:
        return undetermined(f"cannot write result: {e}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
