"""Repros for open scripts/rollout-plugins.sh defects (audit batch nt_E).

    490d30cc  usage() prints header lines 2-80 only; the header runs past line 120.
    c9373b92  a --plugin (filtered) run skips verify_rollout_complete entirely, so the
              commonest invocation has no post-hoc check that the binary landed.
    01dc2dca  verify_rollout_complete treats a MISSING checker (python exit 2) like the
              checker's own non-rollout rc=2 and returns success.
    92e1a0e1  a prune failure never reaches the script's exit code.
    761cf58d  --canary with an empty plan aborts on bash 3.2 with
              "PLAN_ROWS[@]: unbound variable" instead of an explicit message.
    06ec3503  default (non-canary) path: a failed rebuild exits with the registry
              already repointed at a fresh version dir that holds no host binary.
    ef6b8cdf  the canary health gate runs before run_rebuild_and_sync swaps the new
              binaries in, so it can only ever observe the OLD code.

How this tests a script it must not run: rollout-plugins.sh itself is NEVER executed.
Each test extracts the script's own function definitions (and, where the defect lives
in the top-level flow, the script's own top-level tail) from the CURRENT file text and
runs them under /bin/bash with every external effect pointed into a temp dir and every
heavy collaborator (checker, pruner, rebuild, overwatch) replaced by a stub. The code
under test is therefore the real code; only its environment is fake. Nothing here
touches ~/.claude or the real plugin cache/registry.

Each defect test asserts the CORRECT behaviour and is marked expectedFailure while the
item is open; remove the marker in the commit that fixes it. Stdlib only.
"""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "rollout-plugins.sh"
BASH = "/bin/bash" if os.path.exists("/bin/bash") else shutil.which("bash")
FUNC_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\(\)\s*\{(.*)$")


def script_text():
    return SCRIPT.read_text(encoding="utf-8")


def function_defs(text):
    """Every top-level `name() {` ... `}` block of the script, verbatim, in order."""
    out, lines, i = [], text.splitlines(), 0
    while i < len(lines):
        m = FUNC_RE.match(lines[i])
        if not m:
            i += 1
            continue
        if m.group(2).rstrip().endswith("}"):  # one-liner, e.g. usage()
            out.append(lines[i])
            i += 1
            continue
        j = i + 1
        while j < len(lines) and lines[j] != "}":
            j += 1
        if j >= len(lines):
            raise AssertionError(f"unterminated function {m.group(1)}")
        out.append("\n".join(lines[i:j + 1]))
        i = j + 1
    return out


def one_function(text, name):
    for d in function_defs(text):
        if d.startswith(name + "()"):
            return d
    raise AssertionError(f"function {name}() not found in rollout-plugins.sh")


def tail_from(text, marker_regex):
    """The script's top-level code from the LAST line matching marker_regex to EOF."""
    lines = text.splitlines()
    idx = [n for n, l in enumerate(lines) if re.match(marker_regex, l)]
    if not idx:
        raise AssertionError(f"marker {marker_regex!r} not found in rollout-plugins.sh")
    return "\n".join(lines[idx[-1]:])


def sh_q(s):
    return "'" + str(s).replace("'", "'\"'\"'") + "'"


class Sandbox:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="rollout-ntE-")).resolve()
        self.repo = self.root / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        self.cache = self.root / "cache" / "yukineko"
        self.cache.mkdir(parents=True)
        self.registry = self.root / "installed_plugins.json"
        self.registry.write_text(json.dumps({"version": 2, "plugins": {}}))
        self.home = self.root / "home"
        self.home.mkdir()

    def stub(self, rel, body, mode=0o755):
        p = self.repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body)
        p.chmod(mode)
        return p

    def prelude(self, **over):
        v = {
            "REPO": self.repo, "CACHE": self.cache, "REGISTRY": self.registry,
            "OWNER": "yukineko", "GIT_SHA": "0" * 40, "LOCK_DIR": self.cache / ".rollout.lock",
        }
        v.update({k: over.pop(k) for k in list(over) if k in v})
        lines = [f"{k}={sh_q(val)}" for k, val in v.items()]
        lines += [
            "dry=0 force=0 no_rebuild=0 no_sync=0",
            "canary=0 canary_stage_size=1 canary_threshold=2 canary_systemic_threshold=0 no_canary=0",
            "lock_held=0",
            "canary_inflight=0 canary_applied='' canary_ow='' canary_stage=0",
            over.pop("only_plugins", "declare -a only_plugins=()"),
            over.pop("target_names", "declare -a target_names=(p)"),
            over.pop("plan_rows", "declare -a PLAN_ROWS=()"),
        ]
        assert not over, over
        return "\n".join(lines)

    def run(self, body, argv0=None):
        env = dict(os.environ, HOME=str(self.home), CLAUDE_PLUGIN_CACHE=str(self.cache),
                   CLAUDE_PLUGIN_REGISTRY=str(self.registry))
        args = [BASH, "-c", "set -euo pipefail\n" + body]
        if argv0:
            args.append(str(argv0))
        return subprocess.run(args, capture_output=True, text=True, env=env, cwd=str(self.repo), timeout=120)

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


class Base(unittest.TestCase):
    def setUp(self):
        self.text = script_text()
        self.sb = Sandbox()
        self.addCleanup(self.sb.cleanup)


class ExtractionControl(Base):
    def test_control_extraction_is_well_formed(self):
        defs = function_defs(self.text)
        names = [d.split("(", 1)[0] for d in defs]
        for want in ("usage", "verify_rollout_complete", "prune_stale_versions", "run_canary",
                     "registry_patch", "acquire_rollout_lock", "on_rollout_exit"):
            self.assertIn(want, names)
        r = subprocess.run([BASH, "-n"], input="\n".join(defs), capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)


# --- 490d30cc ---------------------------------------------------------------
class Backlog490d30cc(Base):
    def header_lines(self):
        lines = self.text.splitlines()
        end = 1
        while end < len(lines) and lines[end].startswith("#"):
            end += 1
        return lines[1:end]  # line 2 .. last header comment line

    def test_control_header_is_longer_than_80_lines(self):
        self.assertGreater(len(self.header_lines()) + 1, 80)

    @unittest.expectedFailure  # backlog 490d30cc: open defect, remove when fixed
    def test_usage_prints_the_whole_header(self):
        r = self.sb.run(one_function(self.text, "usage") + "\nusage", argv0=SCRIPT)
        self.assertEqual(r.returncode, 0, r.stderr)
        printed = r.stdout.splitlines()
        missing = [l for l in self.header_lines() if l not in printed]
        self.assertEqual(missing[:3], [], f"{len(missing)} header line(s) never printed by usage()")


# --- c9373b92 / 01dc2dca ----------------------------------------------------
class VerifyRolloutComplete(Base):
    def verify(self, only_plugins):
        body = "\n".join([
            self.sb.prelude(only_plugins=only_plugins),
            one_function(self.text, "verify_rollout_complete"),
            "verify_rollout_complete && echo VERIFY_RC=0 || echo VERIFY_RC=$?",
        ])
        return self.sb.run(body)

    def checker_stub(self, rc):
        marker = self.sb.root / "checker-ran"
        self.sb.stub("scripts/check-plugin-rollout.py",
                     f"import sys\nopen({str(marker)!r}, 'w').write(' '.join(sys.argv[1:]))\n"
                     f"print('ROLLOUT: stub drift')\nsys.exit({rc})\n")
        return marker

    def test_control_unfiltered_run_with_drift_fails(self):
        marker = self.checker_stub(1)
        r = self.verify("declare -a only_plugins=()")
        self.assertTrue(marker.exists())
        self.assertIn("VERIFY_RC=1", r.stdout, r.stdout + r.stderr)

    @unittest.expectedFailure  # backlog c9373b92: open defect, remove when fixed
    def test_filtered_run_still_verifies_the_targeted_plugins(self):
        marker = self.checker_stub(1)
        r = self.verify("declare -a only_plugins=(p)")
        self.assertTrue(marker.exists(), "a --plugin run never ran the rollout checker:\n" + r.stdout)
        self.assertNotIn("VERIFY_RC=0", r.stdout, r.stdout)

    @unittest.expectedFailure  # backlog 01dc2dca: open defect, remove when fixed
    def test_missing_checker_is_not_success(self):
        self.assertFalse((self.sb.repo / "scripts" / "check-plugin-rollout.py").exists())
        r = self.verify("declare -a only_plugins=()")
        self.assertNotIn("VERIFY_RC=0", r.stdout, r.stdout + r.stderr)


# --- 92e1a0e1 ---------------------------------------------------------------
class Backlog92e1a0e1(Base):
    def run_tail(self, prune_rc):
        self.sb.stub("scripts/prune-plugin-cache.py",
                     f"import sys\nprint('prune: could not remove a dir')\nsys.exit({prune_rc})\n")
        self.sb.stub("scripts/check-plugin-rollout.py", "import sys\nsys.exit(0)\n")
        body = "\n".join([
            self.sb.prelude(),
            one_function(self.text, "prune_stale_versions"),
            one_function(self.text, "verify_rollout_complete"),
            # the script's own top-level tail: prune -> verify -> done.
            tail_from(self.text, r"^prune_stale_versions$"),
        ])
        return self.sb.run(body)

    def test_control_clean_prune_finishes_done(self):
        r = self.run_tail(0)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("done.", r.stdout)

    @unittest.expectedFailure  # backlog 92e1a0e1: open defect, remove when fixed
    def test_prune_failure_reaches_the_exit_code(self):
        r = self.run_tail(1)
        self.assertNotEqual(r.returncode, 0, "prune failed, yet the rollout ended: " + r.stdout[-300:])


# --- 761cf58d ---------------------------------------------------------------
class Backlog761cf58d(Base):
    @unittest.expectedFailure  # backlog 761cf58d: open defect, remove when fixed
    def test_empty_plan_canary_gives_an_explicit_message_not_unbound_variable(self):
        body = "\n".join([
            self.sb.prelude(plan_rows="declare -a PLAN_ROWS=()"),
            *function_defs(self.text),
            "resolve_overwatch_bin() { echo /usr/bin/true; }",
            "run_canary",
        ])
        r = self.sb.run(body)
        out = r.stdout + r.stderr
        self.assertNotIn("unbound variable", out, out[-400:])
        self.assertRegex(out, r"(?i)no plugins|empty")


# --- 06ec3503 ---------------------------------------------------------------
class Backlog06ec3503(Base):
    def setUp(self):
        super().setUp()
        sb = self.sb
        (sb.repo / "crates" / "p").mkdir(parents=True)
        self.old = sb.cache / "p" / "1.0.0"
        self.new = sb.cache / "p" / "2.0.0"
        for d in (self.old, self.new):
            (d / "bin").mkdir(parents=True)
            (d / "bin" / "p").write_text("#!/bin/sh\n")  # launcher only
        (self.old / "bin" / "p-host").write_text("binary")  # only the OLD dir is live
        sb.registry.write_text(json.dumps({"version": 2, "plugins": {"p@yukineko": [
            {"scope": "user", "installPath": str(self.old), "version": "1.0.0"}]}}))
        sb.stub("scripts/prune-plugin-cache.py", "import sys\nsys.exit(0)\n")
        sb.stub("scripts/check-plugin-rollout.py", "import sys\nsys.exit(0)\n")

    def run_default_path(self, rebuild_ok):
        row = "\t".join(["p", "2.0.0", "crates/p", str(self.new), "0", "1", "0",
                         "2.0.0", "2.0.0", "1.0.0", str(self.old)])
        body = "\n".join([
            self.sb.prelude(),
            *function_defs(self.text),
            # stubs for the heavy collaborators; everything else is the script's own code
            f"plan_or_die() {{ PLAN_TXT={sh_q(row)}; }}",
            "copy_plugin_dir() { :; }",
            ("run_rebuild_and_sync() { echo 'rebuild ok'; }" if rebuild_ok else
             "run_rebuild_and_sync() { echo 'rebuild-plugins.sh: cargo build failed' >&2; return 1; }"),
            "acquire_rollout_lock",
            tail_from(self.text, r"^# --- run the plan \(default, non-canary path"),
        ])
        r = self.sb.run(body)
        entry = json.loads(self.sb.registry.read_text())["plugins"]["p@yukineko"][0]
        return r, entry

    def test_control_successful_run_repoints_registry(self):
        r, entry = self.run_default_path(rebuild_ok=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(entry["installPath"], str(self.new))
        self.assertFalse((self.sb.cache / ".rollout.lock").exists(), "lock not released")

    @unittest.expectedFailure  # backlog 06ec3503: open defect, remove when fixed
    def test_failed_rebuild_does_not_leave_registry_on_a_binaryless_dir(self):
        r, entry = self.run_default_path(rebuild_ok=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(entry["installPath"], str(self.old),
                         "rebuild failed but the registry still points at the launcher-only dir")


# --- ef6b8cdf ---------------------------------------------------------------
class BacklogEf6b8cdf(Base):
    @unittest.expectedFailure  # backlog ef6b8cdf: open defect, remove when fixed
    def test_health_gate_observes_the_swapped_in_binaries(self):
        sb = self.sb
        swapped = sb.root / "new-binaries-in-place"
        gate_log = sb.root / "gate.log"
        ow = sb.stub("fake-overwatch", f"""#!/bin/sh
case "$1" in
  canary-plan) echo '{{"stages":[{{"plugins":["p"]}}]}}' ;;
  canary-rollback-plan) echo '{{}}' ;;
  canary-gate)
    if [ -e {sh_q(swapped)} ]; then echo swapped >> {sh_q(gate_log)}; else echo stale >> {sh_q(gate_log)}; fi
    echo PROCEED ;;
esac
exit 0
""")
        row = "\t".join(["p", "2.0.0", "crates/p", str(sb.cache / "p" / "2.0.0"), "1", "1", "0",
                         "2.0.0", "2.0.0", "1.0.0", str(sb.cache / "p" / "1.0.0")])
        body = "\n".join([
            sb.prelude(plan_rows=f"declare -a PLAN_ROWS=({sh_q(row)})"),
            *function_defs(self.text),
            f"resolve_overwatch_bin() {{ echo {sh_q(ow)}; }}",
            "build_state_json() { echo '{}'; echo '{}'; }",
            "canary_copy_row() { :; }",
            "registry_patch() { echo 'registry patched (stub)'; }",
            f"run_rebuild_and_sync() {{ touch {sh_q(swapped)}; }}",
            "run_canary",
        ])
        r = sb.run(body)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue(swapped.exists(), "the rebuild stub never ran")
        log = gate_log.read_text().split() if gate_log.exists() else []
        self.assertTrue(log, "the health gate never ran")
        self.assertEqual(set(log), {"swapped"}, f"gate evaluations saw: {log}")


if __name__ == "__main__":
    unittest.main()
