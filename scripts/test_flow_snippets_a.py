"""Claim-reproduction tests for crates/flow (skills-only plugin), part A.

Every test pins what is OBSERVED today about a prose claim in
crates/flow/{README.md,README.ja.md,skills/flow/SKILL.md}. Commands are
extracted from SKILL.md fenced blocks (so prose drift breaks the test) and run
against real binaries (condukt, fugu-router, autoflow, hypothesis) with a
throw-away HOME; the real ~/.claude and $HOME are never touched.

Redirect the prose for RED checks with FLOW_SKILL_MD / FLOW_README_MD /
FLOW_README_JA_MD (and CONDUKT_SKILL_MD for the condukt skill).
Run: python3 -m pytest -q scripts/test_flow_snippets_a.py
"""
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE = Path(os.environ.get("FLOW_CRATE_DIR", ROOT / "crates" / "flow"))
SKILL = Path(os.environ.get("FLOW_SKILL_MD", CRATE / "skills" / "flow" / "SKILL.md"))
README = Path(os.environ.get("FLOW_README_MD", CRATE / "README.md"))
README_JA = Path(os.environ.get("FLOW_README_JA_MD", CRATE / "README.ja.md"))
CONDUKT_SKILL = Path(os.environ.get("CONDUKT_SKILL_MD", ROOT / "crates" / "condukt" / "skills" / "condukt" / "SKILL.md"))
CONDUKT_AGENTS = Path(os.environ.get("CONDUKT_AGENTS_DIR", ROOT / "crates" / "condukt" / "agents"))
MARKETPLACE = Path(os.environ.get("FLOW_MARKETPLACE_JSON", ROOT / ".claude-plugin" / "marketplace.json"))
ROOT_CARGO = Path(os.environ.get("FLOW_ROOT_CARGO_TOML", ROOT / "Cargo.toml"))
CRATES_ROOT = Path(os.environ.get("FLOW_CRATES_ROOT", ROOT / "crates"))

DEBUG = ROOT / "target" / "debug"
_BUILT = set()


def _cargo():
    return shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")


def build(*pkgs):
    """cargo build the packages once; return nothing (binaries live in target/debug)."""
    need = [p for p in pkgs if p not in _BUILT]
    if need:
        args = [_cargo(), "build", "-q"]
        for p in need:
            args += ["-p", p]
        r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        _BUILT.update(need)


def binpath(name, pkg=None):
    build(pkg or name)
    p = DEBUG / name
    assert p.exists(), p
    return p


_BINDIR = None


def bindir():
    """A dir holding symlinks `condukt`, `fugu-router`, `hypothesis` -> target/debug."""
    global _BINDIR
    if _BINDIR is None:
        _BINDIR = Path(tempfile.mkdtemp(prefix="flowa-bin-"))
        for n in ("condukt", "fugu-router", "hypothesis"):
            (_BINDIR / n).symlink_to(binpath(n))
    return _BINDIR


def prose(path):
    """Read a prose file with the lead's `[test: ...]` citation markers stripped."""
    return re.sub(r" ?\[test: [^\]]+\]", "", Path(path).read_text(encoding="utf-8"))


def skill_text():
    return prose(SKILL)


def frontmatter_raw(path=None):
    m = re.match(r"---\n(.*?)\n---\n", prose(path or SKILL), re.S)
    assert m, "no YAML front matter"
    return m.group(1)


def frontmatter(path=None):
    fm = {}
    for line in frontmatter_raw(path).splitlines():
        k, _, v = line.partition(":")
        fm[k.strip()] = v.strip()
    return fm


def allowed_tools(path=None):
    return [t.strip() for t in frontmatter(path)["allowed-tools"].split(",")]


def fences(text):
    """[(lang, body, start_offset)] for every ``` fenced block (indent tolerated)."""
    out = []
    for m in re.finditer(r"^[ \t]*```([a-zA-Z]*)[ \t]*\n(.*?)^[ \t]*```[ \t]*$", text, re.S | re.M):
        out.append((m.group(1), m.group(2), m.start()))
    return out


def fence_with(needle, text=None, lang=None):
    text = skill_text() if text is None else text
    for l, body, _ in fences(text):
        if needle in body and (lang is None or l == lang):
            return body
    raise AssertionError(f"no fenced block containing {needle!r}")


def strip_test_tags(s):
    return re.sub(r"\s*\[test: [^\]]*\]", "", s)


def base_env(home, **extra):
    e = {"PATH": f"{bindir()}:{os.environ['PATH']}", "HOME": str(home),
         "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"}
    e.update(extra)
    return e


def sh(cmd, cwd, env):
    return subprocess.run(["sh", "-c", cmd], cwd=cwd, env=env, capture_output=True, text=True)


class Sandbox:
    """temp HOME + a git repo as cwd."""

    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="flowa-"))
        self.home = self.tmp / "home"
        self.repo = self.tmp / "repo"
        self.home.mkdir()
        self.repo.mkdir()
        g = lambda *a: subprocess.run(["git", *a], cwd=self.repo, env=base_env(self.home),
                                      capture_output=True, text=True, check=True)
        g("init", "-q", "-b", "main")
        g("config", "user.email", "t@t.t")
        g("config", "user.name", "t")
        (self.repo / "base.txt").write_text("b\n")
        g("add", ".")
        g("commit", "-q", "-m", "init")

    def env(self, **extra):
        return base_env(self.home, **extra)

    def run(self, argv, env=None, input=None):
        return subprocess.run([str(a) for a in argv], cwd=self.repo, env=env or self.env(),
                              capture_output=True, text=True, input=input)

    def sh(self, cmd, **extra):
        return sh(cmd, self.repo, self.env(**extra))

    def cleanup(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


def decomp(n=1, **top):
    d = {"goal": "g", "tasks": [
        {"id": f"t{i}", "title": "t", "touched_files": [f"f{i}.rs"], "deps": [],
         "done_criteria": "c"} for i in range(n)]}
    d.update(top)
    return d


def policy(sb, *args, autonomous=True):
    extra = {"HARNESS_AUTONOMOUS": "1"} if autonomous else {}
    argv = ["condukt", "policy", "answer", *args]
    return sb.run([bindir() / "condukt", *argv[1:]], env=sb.env(**extra))


class T(unittest.TestCase):
    def setUp(self):
        self.sb = Sandbox()
        self.addCleanup(self.sb.cleanup)

    # ------------------------------------------------------------------ layout
    def test_claim_layout_crate_with_cargo_bin_hooks(self):
        # re-anchored 2026-11-21 (tC): skill-only plugin (no Cargo.toml/src/bin/hooks) ->
        # flow is a Rust crate shipping the `flow stop-gate` Stop hook (bin/flow + hooks/hooks.json)
        # that keeps a live /flow driver running while work remains (ruling 2026-11-21).
        names = {p.name for p in CRATE.iterdir()}
        for required in ("Cargo.toml", "src", "bin", "hooks", ".claude-plugin", "skills"):
            self.assertIn(required, names, f"crates/flow must contain {required}")
        for forbidden in ("Cargo.lock", "target"):  # workspace member: lock/target live at the root
            self.assertNotIn(forbidden, names, f"crates/flow must not contain {forbidden}")
        tracked = set(subprocess.run(["git", "ls-files", "crates/flow"], cwd=ROOT, capture_output=True,
                                     text=True, check=True).stdout.split())
        required_tracked = {
            "crates/flow/.claude-plugin/plugin.json", "crates/flow/Cargo.toml",
            "crates/flow/README.ja.md", "crates/flow/README.md", "crates/flow/skills/flow/SKILL.md",
            "crates/flow/hooks/hooks.json", "crates/flow/bin/flow", "crates/flow/src/main.rs"}
        self.assertEqual(required_tracked - tracked, set(), f"untracked layout files; tracked={sorted(tracked)}")
        self.assertFalse([t for t in tracked if t.endswith("Cargo.lock") or "/target/" in t], tracked)

    def test_claim_layout_plugin_json_valid_name_flow_and_stop_gate_hook(self):
        # re-anchored 2026-11-21 (tC): plugin ships no hooks -> plugin ships exactly the
        # `${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate` Stop hook (and no SessionStart: still no proposing).
        pj = json.loads((CRATE / ".claude-plugin" / "plugin.json").read_text())
        self.assertEqual(pj["name"], "flow")
        hj = json.loads((CRATE / "hooks" / "hooks.json").read_text())
        self.assertNotIn("SessionStart", hj.get("hooks", {}))
        stop_cmds = [h.get("command", "") for e in hj["hooks"].get("Stop", []) for h in e.get("hooks", [])]
        self.assertIn("${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate", stop_cmds)
        launcher = CRATE / "bin" / "flow"
        self.assertTrue(launcher.is_file())
        mode = subprocess.run(["git", "ls-files", "-s", "crates/flow/bin/flow"], cwd=ROOT,
                              capture_output=True, text=True, check=True).stdout.split()[0]
        self.assertEqual(mode, "100755", "bin/flow launcher must be executable in the git index")

    def test_claim_layout_only_skill_is_flow(self):
        skills = sorted(p.name for p in (CRATE / "skills").iterdir())
        self.assertEqual(skills, ["flow"])
        self.assertEqual(sorted(p.name for p in (CRATE / "skills" / "flow").iterdir()), ["SKILL.md"])
        self.assertEqual(frontmatter(CRATE / "skills" / "flow" / "SKILL.md")["name"], "flow")
        # no agents/commands bundled either: "the /flow skill and nothing else"
        self.assertFalse((CRATE / "agents").exists())
        self.assertFalse((CRATE / "commands").exists())

    def test_claim_layout_is_a_workspace_member(self):
        # re-anchored 2026-11-21 (tC): crates/flow excluded from the workspace (not a crate) ->
        # crates/flow is a workspace member package named "flow" (it builds the stop-gate binary).
        root_toml = ROOT_CARGO.read_text()
        m = re.search(r"^exclude\s*=\s*\[(.*?)\]", root_toml, re.S | re.M)
        if m:
            self.assertNotIn('"crates/flow"', m.group(1))
        r = subprocess.run([_cargo(), "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        pkgs = {p["name"]: p["manifest_path"] for p in json.loads(r.stdout)["packages"]}
        self.assertIn("flow", pkgs)
        self.assertTrue(pkgs["flow"].endswith("/crates/flow/Cargo.toml"), pkgs["flow"])

    # ----------------------------------------------------------------- license
    def test_claim_license_mit(self):
        m = json.loads(MARKETPLACE.read_text())
        entry = [p for p in m["plugins"] if p["name"] == "flow"]
        self.assertEqual(len(entry), 1)
        self.assertEqual(entry[0].get("license"), "MIT")
        pj = json.loads((CRATE / ".claude-plugin" / "plugin.json").read_text())
        # plugin.json declares no license of its own; if it ever does it must agree
        self.assertIn(pj.get("license", "MIT"), ("MIT",))
        for path in (README, README_JA):
            last = [l for l in prose(path).splitlines() if l.strip()][-1]
            self.assertEqual(last.strip(), "MIT")

    # -------------------------------------------------------------- no_propose
    def test_claim_no_propose_no_hook_runs_flow(self):
        # re-anchored 2026-11-21 (tC): no hook runs a `flow` binary at all -> the only hook that
        # runs `flow` is the flow crate's own Stop hook `bin/flow stop-gate` (tB); no other event
        # (notably SessionStart, the retired proposal path) and no other crate runs it.
        hits = []
        for hj in sorted(CRATES_ROOT.glob("*/hooks/hooks.json")):
            d = json.loads(hj.read_text())
            for ev, entries in d.get("hooks", {}).items():
                for e in entries:
                    for h in e.get("hooks", []):
                        cmd = h.get("command", "")
                        if re.search(r"(^|/|\s)flow(\s|$)", cmd):
                            hits.append((str(hj.relative_to(CRATES_ROOT)), ev, cmd))
        self.assertEqual(hits, [("flow/hooks/hooks.json", "Stop", "${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate")],
                         "only flow's own Stop hook may run the `flow` binary")

    def test_claim_no_propose_autoflow_has_no_sessionstart_and_no_proposal_strings(self):
        hj = json.loads((ROOT / "crates" / "autoflow" / "hooks" / "hooks.json").read_text())
        self.assertNotIn("SessionStart", hj["hooks"])
        r = subprocess.run([str(binpath("autoflow")), "session-start"], input="{}", capture_output=True,
                           text=True, env={"PATH": os.environ["PATH"], "HOME": str(self.sb.home)})
        self.assertNotEqual(r.returncode, 0, "autoflow must have no session-start subcommand")
        # no NON-comment source line in any crate carries the retired proposal text
        bad = []
        for f in (ROOT / "crates").glob("*/src/**/*.rs"):
            for i, line in enumerate(f.read_text(errors="replace").splitlines(), 1):
                s = line.strip()
                if s.startswith("//"):
                    continue
                if "で開始しますか" in s or "/backlog を実行" in s:
                    bad.append(f"{f.relative_to(ROOT)}:{i}")
        self.assertEqual(bad, [])

    def test_claim_no_propose_autoflow_stop_output_never_proposes_flow(self):
        sb = self.sb
        bindir_stub = sb.tmp / "stub"
        bindir_stub.mkdir()
        stub = bindir_stub / "backlog"
        stub.write_text('#!/bin/sh\nif [ "$1" = lock ]; then echo none; exit 0; fi\n'
                        'echo \'[{"id":"x","title":"pending","status":"pending"}]\'\nexit 0\n')
        stub.chmod(0o755)
        env = {"PATH": f"{bindir_stub}:{bindir()}:/usr/bin:/bin", "HOME": str(sb.home),
               "GIT_CONFIG_GLOBAL": "/dev/null"}
        # enough work for the /record arm, plus a pending condukt run for the /condukt arm
        sess = "sess-noprop"
        st = sb.home / ".session-insights" / "state"
        st.mkdir(parents=True)
        (st / f"{sess}.json").write_text(json.dumps({"turns": 5, "tool_events": 9}))
        p = sb.tmp / "d.json"
        p.write_text(json.dumps(decomp(2)))
        r = sb.run([binpath("condukt"), "state", "init", "--file", p], env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        payload = {"hook_event_name": "Stop", "session_id": sess, "cwd": str(sb.repo), "transcript_path": ""}
        outs = []
        for _ in range(3):
            r = sb.run([binpath("autoflow"), "stop"], env=env, input=json.dumps(payload))
            outs.append(r.stdout + r.stderr)
        self.assertTrue(any("/session-insights:record" in o for o in outs), outs)
        self.assertTrue(any("/condukt" in o for o in outs), outs)
        for o in outs:
            for token in ("/flow", "で開始しますか", "/backlog を実行", "AskUserQuestion"):
                self.assertNotIn(token, o)
        r = sb.run([binpath("autoflow"), "prompt-submit"], env=env,
                   input=json.dumps({"hook_event_name": "UserPromptSubmit", "session_id": sess}))
        self.assertNotIn("/flow", r.stdout + r.stderr)

    # ------------------------------------------------------------- frontmatter
    def test_claim_frontmatter_declares_args_and_tools(self):
        fm = frontmatter()
        self.assertEqual(fm["name"], "flow")
        self.assertIn("argument-hint", fm)
        self.assertIn("$ARGUMENTS", skill_text())
        at = allowed_tools()
        for t in ("Task", "AskUserQuestion", "Bash(backlog:*)", "Bash(compass:*)", "Bash(condukt:*)",
                  "Bash(fugu-router:*)", "Bash(hypothesis:*)", "Bash(overwatch:*)"):
            self.assertIn(t, at)
        cfm = frontmatter(CONDUKT_SKILL)
        self.assertEqual(cfm["name"], "condukt")
        self.assertTrue(CONDUKT_SKILL.exists())
        # the skill's own Step-0 / carve / Task prose is present
        t = skill_text()
        self.assertIn("`$ARGUMENTS` に課題文があれば Step 3（その課題文で condukt 実行）へ直行せよ", t)
        self.assertIn("`/condukt` は `Task` ツールで非同期起動すること", t)
        # re-anchored 2026-11-21 (tC): the circuit-trip "続行 / 中止" fallback Ask and the mandatory
        # carve-time AskUserQuestion were human-judgment waits -> ruling 2026-11-21 removes human
        # waits (every gate goes through `condukt policy answer --resolve`; only the circuit stops).
        # The old phrases are no longer required; the carve must not be pinned as a forced Ask.
        self.assertNotIn("この carve 中の `AskUserQuestion` を省いてはならない", t)

    @staticmethod
    def _invoked_binaries():
        """Binaries invoked as commands in the skill's fenced bash blocks."""
        names = ["backlog", "compass", "condukt", "fugu-router", "hypothesis", "overwatch",
                 "specguard", "specforge", "jq", "printf", "git"]
        found = set()
        for lang, body, _ in fences(skill_text()):
            if lang != "bash":
                continue
            for raw in body.splitlines():
                line = raw.split("  #")[0]
                if line.lstrip().startswith("#"):
                    continue
                for n in names:
                    if re.search(rf"(^|[\s(|;&`]|\$\()({re.escape(n)})\s", line + " "):
                        found.add(n)
        return found

    def test_claim_frontmatter_every_invoked_binary_allowed(self):
        """Every binary the fenced bash blocks invoke must be granted by allowed-tools."""
        at = allowed_tools()
        granted = {re.match(r"Bash\(([^:)]+)", t).group(1) for t in at if t.startswith("Bash(")}
        invoked = self._invoked_binaries()
        for n in ("backlog", "compass", "condukt", "fugu-router", "hypothesis", "overwatch"):
            self.assertIn(n, invoked)
        missing = invoked - granted
        self.assertEqual(missing, set(), f"invoked but not granted: {sorted(missing)}")

    # ---------------------------------------------- autonomy_unknown_subcommand
    def test_claim_autonomy_unknown_subcommand_never_exits_zero(self):
        sb = self.sb
        r = sb.run([binpath("condukt"), "state", os.environ.get("FLOW_A_BOGUS_SUB", "autonomy-check-from-the-future")])
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.returncode, 2)
        self.assertIn("unrecognized subcommand", r.stderr)
        self.assertEqual(r.stdout.strip(), "")
        # the real command, extracted from the fenced block: default is non-autonomous exit 1,
        # exit 0 only when autonomy is switched on
        cmd = fence_with("condukt state autonomy-check", lang="bash").splitlines()[0]
        cmd = cmd.split("#")[0].strip()
        self.assertEqual(cmd, "condukt state autonomy-check")
        off = sb.sh(cmd)
        self.assertEqual(off.returncode, 1)
        self.assertEqual(json.loads(off.stdout), {"autonomous": False})
        on = sb.sh(cmd, HARNESS_AUTONOMOUS="1")
        self.assertEqual(on.returncode, 0)
        self.assertEqual(json.loads(on.stdout), {"autonomous": True})
        t = skill_text()
        # the prose names the exact observed behaviour: exit 2 + "unrecognized subcommand"
        self.assertIn("存在しない版は unrecognized subcommand で exit 2 になる", t)
        self.assertIn("非自律とみなす", t)
        self.assertIn("`autonomy-check` が exit 0 以外なら非自律とみなすこと", t)
        claimed = int(re.search(r"unrecognized subcommand で exit (\d+)", t).group(1))
        self.assertEqual(claimed, r.returncode)

    # ------------------------------------------------------------ policy_table
    # re-anchored 2026-11-21 (tC): the Step 0.5 verdict table (per-row auto/escalate, `--approval`
    # column, judgment rows left to a human) -> ruling 2026-11-21 「破壊はdeny,未測定はreject,
    # autoで自答はいいが解決できないときは別の問いかcancel, 基本はすべてAllow」: every gate is a
    # fenced `condukt policy answer ... --resolve` whose autonomous outcome is one of
    # auto(0)/deny(3)/reject(4)/cancel(5), never escalate(2). The tests below no longer parse the
    # table (its layout is not the contract); they run the fenced commands themselves.
    RESOLVE_EXIT = {0: "auto", 3: "deny", 4: "reject", 5: "cancel"}
    LEVELS = ("low", "medium", "high")

    def _answer(self, risk, rev, conf, *flags, recommend="0", opts=("A", "B"), autonomous=True):
        args = ["--risk", risk, "--reversible", rev, "--confidence", conf, "--question", "q"]
        for o in opts:
            args += ["--option", o]
        args += ["--recommend", recommend, *flags]
        return policy(self.sb, *args, autonomous=autonomous)

    @staticmethod
    def _fenced_policy_invocations():
        """argv (after `condukt policy answer`) of every fenced bash invocation, with `\\`
        continuations joined, a trailing `  # comment` and shell plumbing (`2>/dev/null`, `)`)
        dropped. Placeholder levels like `<low|medium|high>` are kept as-is for the caller."""
        import shlex
        out = []
        for lang, body, _ in fences(skill_text()):
            joined = re.sub(r"\\\n\s*", " ", body)
            for line in joined.splitlines():
                line = re.sub(r"\s{2,}#.*$", "", line)
                i = line.find("condukt policy answer")
                if i < 0 or line.lstrip().startswith("#"):
                    continue
                rest = line[i + len("condukt policy answer"):]
                rest = re.sub(r"\s*2>\s*/dev/null", "", rest)
                rest = re.sub(r"\)\s*;?\s*$", "", rest.strip())
                # `<既定 index>`-style placeholders contain spaces: keep each as ONE token
                rest = re.sub(r"<[^<>\"]*>", lambda m: m.group(0).replace(" ", "_"), rest)
                out.append(shlex.split(rest))
        return out

    def _concretize(self, argv):
        """Expand placeholder levels to every concrete level; fill a placeholder --recommend with 0."""
        argv = list(argv)
        for k, tok in enumerate(argv):
            if k > 0 and argv[k - 1] == "--recommend" and tok.startswith("<"):
                argv[k] = "0"
        slots = [k for k, tok in enumerate(argv)
                 if k > 0 and argv[k - 1] in ("--risk", "--reversible", "--confidence") and tok.startswith("<")]
        combos = [[]]
        for _ in slots:
            combos = [c + [lv] for c in combos for lv in self.LEVELS]
        for c in combos:
            a = list(argv)
            for k, lv in zip(slots, c):
                a[k] = lv
            yield a

    def test_claim_policy_every_fenced_gate_resolves_and_never_escalates_when_autonomous(self):
        invs = self._fenced_policy_invocations()
        self.assertTrue(invs, "SKILL.md has no fenced `condukt policy answer`")
        bad = []
        for argv in invs:
            self.assertIn("--resolve", argv, argv)
            for a in self._concretize(argv):
                r = policy(self.sb, *a)
                want = self.RESOLVE_EXIT.get(r.returncode)
                try:
                    got = json.loads(r.stdout).get("policy")
                except ValueError:
                    got = None
                if want is None or got != want:
                    bad.append((a, r.returncode, r.stdout, r.stderr))
        self.assertEqual(bad, [], "a fenced gate escalated (exit 2) or mis-reported its outcome")

    def test_claim_policy_resolve_reversible_high_always_self_answers(self):
        # was: phase3_every_combination_auto_with_approval (premise: `--approval` clamps to auto).
        # Under --resolve no `--approval` is needed: nothing with reversible=high is destructive.
        bad = []
        for risk in self.LEVELS:
            for conf in self.LEVELS:
                for flags in ([], ["--approval"]):
                    r = self._answer(risk, "high", conf, "--resolve", *flags)
                    if r.returncode != 0 or json.loads(r.stdout)["policy"] != "auto":
                        bad.append((risk, conf, flags, r.returncode, r.stdout))
        self.assertEqual(bad, [])

    def test_claim_policy_resolve_conflict_cancels_untestable_rejects_even_with_approval(self):
        # was: conflict_and_untestable_escalate_even_with_approval -> under --resolve (autonomous)
        # conflict = cannot be resolved -> cancel(5); untestable = unmeasured -> reject(4); neither
        # ever self-answers, with or without --approval.
        for flag, code, name in (("--conflict", 5, "cancel"), ("--untestable", 4, "reject"),
                                 ("--unmeasured", 4, "reject")):
            for extra in ([], ["--approval"]):
                r = self._answer("low", "high", "high", "--resolve", flag, *extra)
                self.assertEqual(r.returncode, code, f"{flag} {extra}: {r.stdout}")
                self.assertEqual(json.loads(r.stdout), {"answered": False, "policy": name, "resolve": True})
        ctl = self._answer("low", "high", "high", "--resolve")
        self.assertEqual(ctl.returncode, 0)  # control: without the flag it self-answers
        # --resolve is inert when not autonomous: the legacy escalate (2) is kept for a human
        r = self._answer("low", "high", "high", "--resolve", "--conflict", autonomous=False)
        self.assertEqual(r.returncode, 2, r.stdout)

    def test_claim_policy_resolve_destructive_is_deny(self):
        r = self._answer("high", "low", "high", "--resolve")
        self.assertEqual(r.returncode, 3, r.stdout)
        self.assertEqual(json.loads(r.stdout)["policy"], "deny")

    def test_claim_policy_table_block_is_never_relaxed_by_approval(self):
        for flag in ([], ["--approval"]):
            r = self._answer("high", "low", "high", *flag)
            self.assertEqual(r.returncode, 3, r.stdout)

    def test_claim_policy_table_auto_picks_the_recommended_option(self):
        r = self._answer("low", "high", "high", "--approval", recommend="0", opts=("stand down", "force"))
        self.assertEqual(json.loads(r.stdout)["chosen"], "stand down")
        r = self._answer("low", "high", "high", "--approval", recommend="1", opts=("stand down", "force"))
        self.assertEqual(json.loads(r.stdout)["chosen"], "force")

    def test_claim_policy_charter_recarve_self_answers_under_resolve(self):
        # re-anchored 2026-11-21 (tC): charter re-carve questions are never self-answered (a human
        # must answer; "人に聞く（自答しない）") -> ruling 2026-11-21 「autoで自答はいい」: the carve
        # question goes through --resolve like every gate and self-answers with the recommended
        # option (it is reversible, not destructive), so /flow does not wait on a human here.
        extra = os.environ.get("FLOW_A_CHARTER_EXTRA", "").split()  # RED hook only
        for risk in ("medium", "high"):
            r = self._answer(risk, "high", "low", "--resolve", *extra, recommend="1", opts=("A", "B"))
            self.assertEqual(r.returncode, 0, f"risk={risk}: {r.stdout}")
            self.assertEqual(json.loads(r.stdout)["chosen"], "B")
        # control: the same question WITHOUT --resolve is the old escalate (exit 2) — --resolve is
        # exactly what removes the human wait
        ctl = self._answer("medium", "high", "low")
        self.assertEqual(ctl.returncode, 2, ctl.stdout)
        t = skill_text()
        self.assertNotIn("charter 彫り直しの問いを自答してはならない", t)

    def test_claim_policy_table_circuit_trip_exits_nonzero(self):
        sb = self.sb
        p = sb.tmp / "d.json"
        p.write_text(json.dumps(decomp(3)))
        rid = "flow-SCIRC"
        self.assertEqual(sb.run([binpath("condukt"), "state", "init", "--run", rid, "--file", p]).returncode, 0)
        for i in range(3):
            self.assertEqual(sb.run([binpath("condukt"), "state", "set", "--run", rid, "--task", f"t{i}",
                                     "--status", "failed"]).returncode, 0)
        cmd = [l for l in fence_with("condukt circuit check", lang="bash").splitlines()
               if l.startswith("condukt circuit check")][0].split("#")[0]
        r = sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SCIRC")
        self.assertNotEqual(r.returncode, 0)
        v = json.loads(r.stdout)
        self.assertEqual((v["verdict"], v["reason"]), ("trip", "failure_streak"))

    # ------------------------------------------------------ judgment_no_approval
    def test_claim_judgment_approval_flag_is_exactly_what_erases_the_question(self):
        for risk, rev, conf in (("medium", "high", "low"), ("medium", "medium", "low")):
            without = self._answer(risk, rev, conf)
            self.assertEqual(without.returncode, 2, without.stdout)
            self.assertEqual(json.loads(without.stdout), {"answered": False, "policy": "escalate"})
            with_flag = self._answer(risk, rev, conf, "--approval")
            self.assertEqual(with_flag.returncode, 0, with_flag.stdout)
            self.assertTrue(json.loads(with_flag.stdout)["answered"])

    def test_claim_judgment_divert_command_as_written_resolves_and_is_journaled(self):
        # re-anchored 2026-11-21 (tC): the spec-gap divert gate escalates to a human (exit 2) ->
        # ruling 2026-11-21: it is a fenced `condukt policy answer --resolve` that, autonomous,
        # self-answers with its recommended option (exit 0) and journals the row with resolve:true.
        block = next((b for l, b, _ in fences(skill_text())
                      if l == "bash" and "condukt policy answer" in b and "specforge" in b), None)
        self.assertIsNotNone(block, "no fenced bash policy-answer block for the specforge divert gate")
        self.assertIn("--resolve", block)
        rec_m = re.search(r"--recommend (\d+)", block)
        self.assertIsNotNone(rec_m, block)
        opts = re.findall(r'--option "([^"]+)"', block)
        want = opts[int(rec_m.group(1))]
        sb = self.sb
        r = sb.sh(block, HARNESS_AUTONOMOUS="1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual((out["policy"], out["chosen"]), ("auto", want))
        journals = list(sb.home.glob(".condukt/state/gate-decisions.jsonl"))
        self.assertEqual(len(journals), 1, list(sb.home.rglob("*.jsonl")))
        row = json.loads(journals[0].read_text().strip().splitlines()[-1])
        self.assertEqual((row["policy"], row.get("resolve"), row["chosen"]), ("auto", True, want))
        self.assertIn("正典", row["question"])
        # control: the same block with --resolve removed is the old human wait (escalate, exit 2)
        ctl = sb.sh(block.replace(" --resolve", ""), HARNESS_AUTONOMOUS="1")
        self.assertEqual(ctl.returncode, 2, ctl.stdout)

    @staticmethod
    def _pivot_command():
        # re-anchored 2026-11-21 (tC): exact prose 「既定 verdict は escalate」 + inline command ->
        # locate the pivot gate by its `--question "pivot…"` wherever it is (fenced or inline),
        # with the nearest preceding risk triple; the old "既定 verdict" capture is gone.
        t = strip_test_tags(skill_text())
        sep = r"[\s`、\\]*"
        m = re.search(r'--question "([^"]*pivot[^"]*)"' + sep + r'--option "([^"]+)"' + sep +
                      r'--option "([^"]+)"' + sep + r"--recommend (\d)", t)
        assert m, "pivot gate command not found in SKILL.md"
        lo = max(0, m.start() - 600)
        triples = list(re.finditer(r"--risk (\w+) --reversible (\w+) --confidence (\w+)", t[lo:m.start()]))
        assert triples, "pivot gate has no --risk/--reversible/--confidence before its --question"
        tri = triples[-1]
        window = t[lo + tri.start():m.end()]
        return tri.groups() + m.groups() + (window,)

    def test_claim_judgment_pivot_gate_resolves_and_recommends_continue(self):
        # re-anchored 2026-11-21 (tC): pivot escalates to a human (judgment, no --approval) ->
        # ruling 2026-11-21: the pivot gate goes through --resolve, self-answers, and its
        # recommended option is 継続 (the loop does not stop on a pivot signal).
        risk, rev, conf, q, o0, o1, rec, window = self._pivot_command()
        self.assertIn("--resolve", window, f"pivot gate lacks --resolve: {window!r}")
        self.assertEqual([o0, o1][int(rec)], "継続")
        args = ["--risk", risk, "--reversible", rev, "--confidence", conf, "--question", q,
                "--option", o0, "--option", o1, "--recommend", rec]
        r = policy(self.sb, *args, "--resolve")
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertEqual(json.loads(r.stdout)["chosen"], "継続")
        ctl = policy(self.sb, *args)  # control: without --resolve it is the old human wait
        self.assertEqual(ctl.returncode, 2, ctl.stdout)

    # ------------------------------------------------------ max_parallel_default
    def test_claim_max_parallel_default_is_four_when_unset(self):
        sb = self.sb
        r = sb.run([binpath("condukt"), "init"])
        self.assertEqual(r.returncode, 0, r.stderr)
        cfg = (sb.home / ".condukt" / "config.toml").read_text()
        self.assertRegex(cfg, r"(?m)^max_parallel = 4$")
        # control: the written value is the effective config value, not a literal
        sb2 = Sandbox()
        self.addCleanup(sb2.cleanup)
        r = sb2.run([binpath("condukt"), "init"], env=sb2.env(CONDUKT_MAX_PARALLEL="7"))
        self.assertRegex((sb2.home / ".condukt" / "config.toml").read_text(), r"(?m)^max_parallel = 7$")
        t = skill_text()
        self.assertIn("無指定なら 4", t)

    # ------------------------------------------------------------ decomp_fields
    def test_claim_decomp_fields_engine_accepts_open_questions_and_confidence_low(self):
        sb = self.sb
        d = decomp(2, open_questions=["which spec?"])
        d["tasks"][0]["confidence"] = "low"
        p = sb.tmp / "d.json"
        p.write_text(json.dumps(d))
        for sub in ("validate", "schedule"):
            r = sb.run([binpath("condukt"), sub, "--file", p])
            self.assertEqual(r.returncode, 0, f"{sub}: {r.stderr}")
        r = sb.run([binpath("condukt"), "state", "init", "--file", p])
        self.assertEqual(r.returncode, 0, r.stderr)
        # the interpreter agent defines per-task confidence (high|medium|low)
        interp = (CONDUKT_AGENTS / "condukt-interpreter.md").read_text(encoding="utf-8")
        self.assertIn('"confidence": "high | medium | low"', interp)

    # ------------------------------------------------------- condukt_fugu_route
    def _route(self, good_model, suggested, runs=20):
        """Seed 8 verified episodes of `good_model`; route a task whose suggested_model is
        `suggested` `runs` times; return a Counter of the rewritten models.
        OBSERVED: routing is stochastic (Thompson sampling), so only a majority is pinned."""
        import collections
        sb = Sandbox()
        self.addCleanup(sb.cleanup)
        for _ in range(8):
            r = sb.run([binpath("fugu-router"), "record", "--title", "refactor parser module",
                        "--class", "parallel", "--model", good_model, "--status", "verified",
                        "--cost", "0.01", "--duration", "5"])
            self.assertEqual(r.returncode, 0, r.stderr)
        d = decomp(1)
        d["tasks"][0].update(title="refactor parser module", **{"class": "parallel", "suggested_model": suggested})
        p = sb.tmp / "d.json"
        p.write_text(json.dumps(d))
        seen = collections.Counter()
        for _ in range(runs):
            r = sb.run([binpath("fugu-router"), "route", "--file", p])
            self.assertEqual(r.returncode, 0, r.stderr)
            out = json.loads(r.stdout)
            self.assertEqual(out["goal"], "g")
            self.assertEqual(out["tasks"][0]["id"], "t0")
            seen[out["tasks"][0]["suggested_model"]] += 1
        return seen

    def test_claim_condukt_fugu_route_rewrites_suggested_model_from_episodes(self):
        haiku_store = self._route("haiku", "opus")
        self.assertGreaterEqual(haiku_store["haiku"], 15, haiku_store)  # opus rewritten to haiku
        opus_store = self._route("opus", "opus")
        # control: an opus-only store pulls toward haiku clearly less often (routing is stochastic)
        self.assertGreaterEqual(haiku_store["haiku"] - opus_store["haiku"], 3, (haiku_store, opus_store))
        t = skill_text()
        self.assertIn("`fugu-router route` が各タスクの `suggested_model` を実績から（Thompson sampling による確率的な選択で）上書きする", t)
        self.assertIn("backlog バッチは 1 回の `/condukt` 呼び出しに束ねること", t)

    # ---------------------------------------------------------------- fugu_audit
    def _fugu_cmds(self, model, status, delegation):
        block = fence_with("fugu-router record", lang="bash")
        block = (block.replace("<task title>", "flow audit").replace("<suggested_model>", model)
                 .replace("<verified|failed>", status).replace("<observed_cost_usd>", "0.25")
                 .replace("<observed_duration_secs>", "42").replace("<fork|inline>", delegation))
        rec, _, audit = block.partition("\n\n")
        self.assertIn("fugu-router record", rec)
        self.assertIn("fugu-router audit-recent", audit)
        return rec.strip(), audit.strip()

    def test_claim_fugu_audit_record_then_audit_found(self):
        for delegation, status in (("fork", "verified"), ("inline", "failed")):
            sb = Sandbox()
            self.addCleanup(sb.cleanup)
            rec, audit = self._fugu_cmds("sonnet", status, delegation)
            miss = sb.sh(audit)
            self.assertEqual(miss.returncode, 1, miss.stdout)
            self.assertEqual(json.loads(miss.stdout), {"found": False})
            r = sb.sh(rec)
            self.assertEqual(r.returncode, 0, r.stderr)
            hit = sb.sh(audit)
            self.assertEqual(hit.returncode, 0, hit.stdout + hit.stderr)
            self.assertEqual(json.loads(hit.stdout), {"found": True})
            ep = (sb.home / ".fugu-router" / "episodes.jsonl").read_text().strip().splitlines()
            self.assertEqual(len(ep), 1)
            e = json.loads(ep[0])
            self.assertEqual(e.get("class"), "flow-delegation")
            self.assertEqual(e.get("delegation"), delegation)
            self.assertEqual(e.get("duration_secs"), 42)
            self.assertAlmostEqual(e.get("cost_usd"), 0.25)
        t = skill_text()
        self.assertIn("`audit-recent` が exit 0（found:true）", t)
        self.assertIn("`audit-recent` が exit 1（found:false）", t)

    def test_claim_fugu_audit_other_class_does_not_satisfy_the_audit(self):
        sb = self.sb
        r = sb.run([binpath("fugu-router"), "record", "--title", "x", "--class", "something-else",
                    "--model", "sonnet", "--status", "verified", "--cost", "0.1", "--duration", "1"])
        self.assertEqual(r.returncode, 0, r.stderr)
        _, audit = self._fugu_cmds("sonnet", "verified", "fork")
        miss = sb.sh(audit)
        self.assertEqual(miss.returncode, 1)
        self.assertEqual(json.loads(miss.stdout), {"found": False})

    # --------------------------------------------------------- linked_hypotheses
    def _await_block(self):
        return fence_with("hypothesis await-measurement", text=prose(CONDUKT_SKILL),
                          lang="bash")

    def _hyp_world(self):
        sb = Sandbox()
        self.addCleanup(sb.cleanup)
        r = sb.run([binpath("hypothesis"), "add", "ship it"])
        self.assertEqual(r.returncode, 0, r.stderr)
        hid = r.stdout.split()[0]
        d = decomp(1, linked_hypotheses=[hid])
        p = sb.tmp / "d.json"
        p.write_text(json.dumps(d))
        r = sb.run([binpath("condukt"), "state", "init", "--file", p])
        self.assertEqual(r.returncode, 0, r.stderr)
        return sb, hid, p, r.stdout.strip()

    def _status_lists(self, sb):
        out = {}
        for st in ("open", "awaiting-measurement"):
            r = sb.run([binpath("hypothesis"), "list", "--status", st])
            self.assertEqual(r.returncode, 0, r.stderr)
            out[st] = r.stdout
        return out

    def test_claim_linked_hypotheses_gate_pass_alone_does_not_transition_the_binary(self):
        """FALSE finding (wording): flow's SKILL.md says "condukt は gate PASS 時に
        linked_hypotheses を awaiting-measurement へ遷移済みである". The condukt BINARY's
        `state gate` passing leaves the hypothesis `open`; the transition is a shell
        block in condukt's SKILL.md that the LLM must run afterwards."""
        sb, hid, p, rid = self._hyp_world()
        sb.run([binpath("condukt"), "state", "set", "--run", rid, "--task", "t0", "--status", "verified"])
        g = sb.run([binpath("condukt"), "state", "gate", "--run", rid])
        self.assertEqual(g.returncode, 0, g.stdout + g.stderr)
        if os.environ.get("FLOW_A_PRETRANSITION"):  # RED hook: simulate a binary that transitions
            sb.run([binpath("hypothesis"), "await-measurement", hid, "--run", rid])
        lists = self._status_lists(sb)
        self.assertIn(hid, lists["open"])
        self.assertNotIn(hid, lists["awaiting-measurement"])

    def test_claim_linked_hypotheses_condukt_skill_block_moves_them_to_awaiting_measurement(self):
        sb, hid, p, rid = self._hyp_world()
        sb.run([binpath("condukt"), "state", "set", "--run", rid, "--task", "t0", "--status", "verified"])
        self.assertEqual(sb.run([binpath("condukt"), "state", "gate", "--run", rid]).returncode, 0)
        block = self._await_block().replace("<json.routed>", str(p))
        r = sb.sh(f'RID={rid}\n{block}')
        self.assertEqual(r.returncode, 0, r.stderr)
        lists = self._status_lists(sb)
        self.assertIn(hid, lists["awaiting-measurement"])
        self.assertNotIn(hid, lists["open"])
        self.assertIn("awaiting-measurement", r.stdout)
        # control: an unrelated hypothesis is untouched
        self.assertIn("計測待ち", r.stdout)

    # ------------------------------------------------------------- release_task
    def test_claim_release_task_releases_only_that_hashkey(self):
        sb = self.sb
        env_extra = {"CLAUDE_CODE_SESSION_ID": "SREL"}
        claim = fence_with("condukt state claim-task", lang="bash")
        release = fence_with("condukt state release-task", lang="bash")
        self.assertIn("--stateless", claim)
        for hk in ("H1", "H2"):
            r = sb.sh(claim.replace("<title>", "t").replace("<hashkey>", hk), **env_extra)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        isc = lambda hk: sb.run([binpath("condukt"), "state", "is-claimed", "--hashkey", hk])
        for hk in ("H1", "H2"):
            r = isc(hk)
            self.assertEqual(r.returncode, 0)
            self.assertEqual(json.loads(r.stdout)["holder_run"], "flow-SREL")
        r = sb.sh(release.replace("<hashkey>", "H1"), **env_extra)
        self.assertEqual(r.returncode, 0, r.stderr)
        gone = isc("H1")
        self.assertEqual(gone.returncode, 1)
        self.assertEqual(json.loads(gone.stdout), {"claimed": False, "hashkey": "H1", "holder_run": None})
        still = isc("H2")
        self.assertEqual(still.returncode, 0)
        self.assertTrue(json.loads(still.stdout)["claimed"])

    # ----------------------------------------------------------- circuit_defaults
    def _circuit_cmd(self):
        return [l for l in fence_with("condukt circuit check", lang="bash").splitlines()
                if l.startswith("condukt circuit check")][0].split("#")[0].strip()

    def test_claim_circuit_flow_invocation_streak_and_budget_axes_are_dead(self):
        """Regression (backlog cccc9c43, was an expected failure).
        /flow runs `condukt circuit check --run flow-$SID --session $SID`: it never runs
        `state init` for that run and passes no --budget-cap-usd. The streak used to be
        structurally 0 because `backlog fail` is invisible to the circuit. Now
        `state release-task` records each item's outcome (no `--outcome done` = not done)
        and the circuit counts that log for a run without run state, so after three
        failed items the exact SKILL command trips on failure_streak. The budget axis
        stays disabled for /flow (asserted below)."""
        cmd = self._circuit_cmd()
        self._fresh_session()
        self.assertNotIn("--budget-cap-usd", cmd)
        sb = self.sb
        for i in range(3):  # what /flow does per item: claim it, fail it in backlog, release it
            hk = f"HK{i}"
            sb.sh(f"condukt state claim-task --run flow-SIDLE --session SIDLE --stateless --title t --hashkey {hk}")
            sb.sh(f"condukt state release-task --run flow-SIDLE --hashkey {hk}")
        r = sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
        v = json.loads(r.stdout)
        self.assertEqual((r.returncode, v.get("reason")), (1, "failure_streak"),
                         f"observed: exit={r.returncode} json={v}")

    def _fresh_session(self):
        sb = self.sb
        d = sb.home / ".claude" / "projects" / "-fixture"
        d.mkdir(parents=True)
        tr = d / "SIDLE.jsonl"
        tr.write_text("{}\n")
        return tr

    def test_claim_circuit_skill_command_never_trips_on_compaction(self):
        # re-anchored 2026-11-21 (tC): circuit trips `session_compacted` after --max-compactions
        # (the compaction handoff stop) -> ruling 2026-11-21 removes the compaction stop: the SKILL's
        # own command carries --no-compaction-axis, so a heavily compacted fresh transcript continues.
        cmd = self._circuit_cmd()
        self.assertIn("--no-compaction-axis", cmd)
        tr = self._fresh_session()
        tr.write_text("{}\n" + '{"type":"system","subtype":"compact_boundary"}\n' * 5)
        r = self.sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
        v = json.loads(r.stdout)
        self.assertEqual((r.returncode, v["verdict"]), (0, "continue"), v)
        # control: the same command with the axis ON trips session_compacted on this transcript
        on = self.sb.sh(cmd.replace("--no-compaction-axis", ""), CLAUDE_CODE_SESSION_ID="SIDLE")
        self.assertEqual((on.returncode, json.loads(on.stdout).get("reason")), (1, "session_compacted"),
                         on.stdout)

    def test_claim_circuit_defaults_idle_ttl_1800_boundary(self):
        cmd = self._circuit_cmd()
        tr = self._fresh_session()
        out = {}
        for ago in (1700, 1900):
            t = time.time() - ago
            os.utime(tr, (t, t))
            r = self.sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
            v = json.loads(r.stdout)
            out[ago] = (r.returncode, v["verdict"], v["reason"])
        self.assertEqual(out[1700], (0, "continue", None))
        self.assertEqual(out[1900], (1, "trip", "stall"))

    # -------------------------------------------------------------- circuit_idle
    def test_claim_circuit_idle_source_is_session_flag_with_fresh_transcript(self):
        cmd = self._circuit_cmd()
        self._fresh_session()
        r = self.sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
        self.assertEqual(r.returncode, 0, r.stdout)
        v = json.loads(r.stdout)
        self.assertEqual(v["idle_source"], "session_flag")
        self.assertEqual(v["verdict"], "continue")
        self.assertLess(v["idle_secs"], 600)
        # --run not exactly flow-<SESSION> : the session flag is not used (fail-closed)
        r2 = self.sb.sh(cmd.replace('--run "flow-$CLAUDE_CODE_SESSION_ID"', '--run "other-run"'),
                        CLAUDE_CODE_SESSION_ID="SIDLE")
        self.assertEqual(r2.returncode, 1)
        self.assertEqual(json.loads(r2.stdout)["reason"], "idle_unmeasured")
        self.assertIn("`--run` がちょうど `flow-<SESSION>` のときだけ有効", skill_text())
        self.assertIn("JSON の `idle_source` が `session_flag` になる", skill_text())

    def test_claim_circuit_idle_home_unset_trips_unmeasured(self):
        cmd = self._circuit_cmd()
        self._fresh_session()
        env = {"PATH": f"{bindir()}:{os.environ['PATH']}", "CLAUDE_CODE_SESSION_ID": "SIDLE"}
        r = sh(cmd, self.sb.repo, env)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        v = json.loads(r.stdout)
        self.assertEqual((v["verdict"], v["reason"]), ("trip", "idle_unmeasured"))
        self.assertIn("HOME unset", v["idle_unknown_reason"])

    def test_claim_circuit_idle_unreadable_claim_registry_trips_unmeasured(self):
        cmd = self._circuit_cmd()
        self._fresh_session()
        sb = self.sb
        r = sb.run([binpath("condukt"), "state", "claim-task", "--run", "flow-OTHER", "--session", "OTHER",
                    "--stateless", "--title", "t", "--hashkey", "hk"])
        self.assertEqual(r.returncode, 0, r.stderr)
        regs = list(sb.home.glob(".condukt/state/*/claims.json"))
        self.assertEqual(len(regs), 1)
        ok = sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
        self.assertEqual(ok.returncode, 0, ok.stdout)  # control: readable registry + fresh transcript continues
        regs[0].write_text("{not json")
        bad = sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SIDLE")
        self.assertEqual(bad.returncode, 1, bad.stdout)
        v = json.loads(bad.stdout)
        self.assertEqual((v["verdict"], v["reason"]), ("trip", "idle_unmeasured"))
        self.assertIn("unreadable", v["idle_unknown_reason"])

    def test_claim_circuit_idle_missing_transcript_trips_unmeasured(self):
        cmd = self._circuit_cmd()
        r = self.sb.sh(cmd, CLAUDE_CODE_SESSION_ID="SNOTX")
        self.assertEqual(r.returncode, 1)
        v = json.loads(r.stdout)
        self.assertEqual((v["verdict"], v["reason"]), ("trip", "idle_unmeasured"))


if __name__ == "__main__":
    unittest.main()
