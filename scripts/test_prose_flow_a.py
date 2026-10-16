"""Claim-reproduction tests for crates/flow (skills-only plugin), part A.

Every test pins what is OBSERVED today about a prose claim in
crates/flow/{README.md,README.ja.md,skills/flow/SKILL.md}. Commands are
extracted from SKILL.md fenced blocks (so prose drift breaks the test) and run
against real binaries (condukt, fugu-router, autoflow, hypothesis) with a
throw-away HOME; the real ~/.claude and $HOME are never touched.

Redirect the prose for RED checks with FLOW_SKILL_MD / FLOW_README_MD /
FLOW_README_JA_MD (and CONDUKT_SKILL_MD for the condukt skill).
Run: python3 -m pytest -q scripts/test_prose_flow_a.py
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
    def test_claim_layout_no_cargo_no_bin_no_hooks(self):
        names = {p.name for p in CRATE.iterdir()}
        for forbidden in ("Cargo.toml", "src", "bin", "hooks", "Cargo.lock", "target"):
            self.assertNotIn(forbidden, names, f"crates/flow must not contain {forbidden}")
        self.assertIn(".claude-plugin", names)
        self.assertIn("skills", names)
        # tracked files: exactly the manifest, the skill, and the READMEs
        tracked = subprocess.run(["git", "ls-files", "crates/flow"], cwd=ROOT, capture_output=True,
                                 text=True, check=True).stdout.split()
        self.assertEqual(sorted(tracked), sorted([
            "crates/flow/.claude-plugin/plugin.json", "crates/flow/README.ja.md",
            "crates/flow/README.md", "crates/flow/skills/flow/SKILL.md"]))

    def test_claim_layout_plugin_json_valid_name_flow_no_hooks(self):
        pj = json.loads((CRATE / ".claude-plugin" / "plugin.json").read_text())
        self.assertEqual(pj["name"], "flow")
        self.assertNotIn("hooks", pj)
        self.assertFalse((CRATE / "hooks").exists())

    def test_claim_layout_only_skill_is_flow(self):
        skills = sorted(p.name for p in (CRATE / "skills").iterdir())
        self.assertEqual(skills, ["flow"])
        self.assertEqual(sorted(p.name for p in (CRATE / "skills" / "flow").iterdir()), ["SKILL.md"])
        self.assertEqual(frontmatter(CRATE / "skills" / "flow" / "SKILL.md")["name"], "flow")
        # no agents/commands bundled either: "the /flow skill and nothing else"
        self.assertFalse((CRATE / "agents").exists())
        self.assertFalse((CRATE / "commands").exists())

    def test_claim_layout_not_a_workspace_member(self):
        root_toml = ROOT_CARGO.read_text()
        m = re.search(r"^exclude\s*=\s*\[(.*?)\]", root_toml, re.S | re.M)
        self.assertTrue(m)
        self.assertIn('"crates/flow"', m.group(1))
        r = subprocess.run([_cargo(), "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT,
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        manifests = [p["manifest_path"] for p in json.loads(r.stdout)["packages"]]
        self.assertFalse([m for m in manifests if "/crates/flow/" in m], manifests)
        pkgs = [p["name"] for p in json.loads(r.stdout)["packages"]]
        self.assertNotIn("flow", pkgs)

    def test_claim_layout_readme_prose_and_fence_comments(self):
        for path in (README, README_JA):
            t = prose(path)
            fence = fence_with(".claude-plugin/plugin.json", t)
            self.assertRegex(fence, r"\.claude-plugin/plugin\.json\s+#")
            self.assertRegex(fence, r"skills/flow/SKILL\.md\s+#")
            # the layout fence lists exactly those two paths
            self.assertEqual(len([l for l in fence.splitlines() if l.strip()]), 2, fence)
        en = prose(README)
        self.assertIn("plugin manifest", fence_with("plugin.json", en))
        self.assertIn("the /flow skill (drives the source", fence_with("plugin.json", en))
        self.assertIn("flow has no hook.", en)
        self.assertIn("no binary, no launcher, no hook, not a Cargo crate", en)
        self.assertIn("The plugin bundles the `/flow` skill and nothing else.", en)
        self.assertIn("so `cargo` is not involved in shipping it", en)
        ja = prose(README_JA)
        self.assertIn("flow は hook を持たない", ja)
        self.assertIn("バイナリ・ランチャ・hook はいずれも無く、Cargo クレートでもない", ja)
        self.assertIn("プラグインは `/flow` skill のみを同梱する", ja)
        self.assertIn("`cargo` は関わらない", ja)

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
        hits = []
        for hj in sorted(CRATES_ROOT.glob("*/hooks/hooks.json")):
            d = json.loads(hj.read_text())
            for ev, entries in d.get("hooks", {}).items():
                for e in entries:
                    for h in e.get("hooks", []):
                        cmd = h.get("command", "")
                        if re.search(r"(^|/|\s)flow(\s|$)", cmd):
                            hits.append((str(hj), ev, cmd))
        self.assertEqual(hits, [], "a hook runs the retired `flow` binary")
        self.assertFalse((CRATE / "hooks").exists())

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

    def test_claim_no_propose_docs_say_explicit_invocation_only(self):
        self.assertIn("自分から `/flow` を提案してはならない", skill_text())
        self.assertIn("明示的に起動したときだけ走る", frontmatter()["description"])
        self.assertIn("提案することはしない", frontmatter()["description"])
        self.assertIn("runs when, and only when, you invoke it", prose(README))
        self.assertIn("明示的に起動したときだけ走る", prose(README_JA))

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
        self.assertIn("`AskUserQuestion`「続行 / 中止」", t)
        self.assertIn("この carve 中の `AskUserQuestion` を省いてはならない", t)

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
    TABLE_ROWS = {
        "lock": "排他ロック競合", "resume": "resume 選択", "deploy": "deploy/push",
        "phase3": "condukt Phase 3", "pivot": "pivot-check", "worker": "worker が blocked",
        "divert": "spec-gap divert", "conflict": "merge conflict", "untestable": "測れない決定",
        "charter": "charter 彫り直し", "circuit": "循環ブレーカー",
    }

    @classmethod
    def table(cls):
        lines = skill_text().splitlines()
        i = next(k for k, l in enumerate(lines) if l.startswith("| human gate |"))
        header = [c.strip() for c in lines[i].strip().strip("|").split("|")]
        self_cols = {h: n for n, h in enumerate(header)}
        rows = {}
        for l in lines[i + 2:]:
            if not l.startswith("|"):
                break
            cells = [c.strip().replace("*", "").replace("`", "") for c in l.strip().strip("|").split("|")]
            for key, needle in cls.TABLE_ROWS.items():
                if needle in cells[0]:
                    rows[key] = cells
        return header, rows

    def test_claim_policy_table_header_and_rows_present(self):
        header, rows = self.table()
        self.assertEqual(header[0], "human gate")
        self.assertEqual(header[2:5], ["risk", "reversible", "confidence"])
        self.assertEqual(sorted(rows), sorted(self.TABLE_ROWS))

    def _answer(self, risk, rev, conf, *flags, recommend="0", opts=("A", "B")):
        args = ["--risk", risk, "--reversible", rev, "--confidence", conf, "--question", "q"]
        for o in opts:
            args += ["--option", o]
        args += ["--recommend", recommend, *flags]
        return policy(self.sb, *args)

    def test_claim_policy_table_rows_verdicts(self):
        header, rows = self.table()
        for key in ("lock", "resume", "deploy", "pivot", "worker", "divert"):
            cells = rows[key]
            risk, rev, conf = cells[2], cells[3], cells[4]
            approval = "--approval" if "付ける" in cells[5] and "付けない" not in cells[5] else None
            expect_auto = "auto" in cells[6] and "escalate" not in cells[6]
            r = self._answer(risk, rev, conf, *( [approval] if approval else []))
            want = 0 if expect_auto else 2
            self.assertEqual(r.returncode, want, f"{key} {cells}: {r.stdout}")
            self.assertEqual(json.loads(r.stdout)["policy"], "auto" if expect_auto else "escalate", key)
        # table says lock/resume/deploy take the flag and the judgment rows do not
        for key in ("lock", "resume", "deploy"):
            self.assertIn("付ける", rows[key][5])
        for key in ("pivot", "worker", "divert"):
            self.assertIn("付けない", rows[key][5])

    def test_claim_policy_table_phase3_every_combination_auto_with_approval(self):
        header, rows = self.table()
        cells = rows["phase3"]
        self.assertEqual(cells[3], "high")  # reversible column
        self.assertIn("付ける", cells[5])
        bad = []
        for risk in ("low", "medium", "high"):
            for conf in ("low", "medium", "high"):
                r = self._answer(risk, "high", conf, "--approval")
                if r.returncode != 0:
                    bad.append((risk, conf, r.returncode, r.stdout))
        self.assertEqual(bad, [])

    def test_claim_policy_table_conflict_and_untestable_escalate_even_with_approval(self):
        header, rows = self.table()
        self.assertEqual(rows["conflict"][5], "--conflict")
        self.assertEqual(rows["untestable"][5], "--untestable")
        for flag in ("--conflict", "--untestable"):
            ok = self._answer("low", "high", "high", "--approval", flag)
            self.assertEqual(ok.returncode, 2, f"{flag}: {ok.stdout}")
            self.assertEqual(json.loads(ok.stdout), {"answered": False, "policy": "escalate"})
            ctl = self._answer("low", "high", "high", "--approval")
            self.assertEqual(ctl.returncode, 0)  # control: without the clamp flag it would be auto

    def test_claim_policy_table_block_is_never_relaxed_by_approval(self):
        for flag in ([], ["--approval"]):
            r = self._answer("high", "low", "high", *flag)
            self.assertEqual(r.returncode, 3, r.stdout)

    def test_claim_policy_table_auto_picks_the_recommended_option(self):
        r = self._answer("low", "high", "high", "--approval", recommend="0", opts=("stand down", "force"))
        self.assertEqual(json.loads(r.stdout)["chosen"], "stand down")
        r = self._answer("low", "high", "high", "--approval", recommend="1", opts=("stand down", "force"))
        self.assertEqual(json.loads(r.stdout)["chosen"], "force")

    def test_claim_policy_table_charter_recarve_is_never_self_answered(self):
        header, rows = self.table()
        cells = rows["charter"]
        self.assertIn("判断要求", cells[1])
        self.assertIn("付けない", cells[5])
        self.assertNotIn("付ける", cells[5])
        self.assertIn("人に聞く（自答しない）", cells[6])
        extra = os.environ.get("FLOW_A_CHARTER_EXTRA", "").split()  # RED hook only
        # the carve question is a judgment gate: lowest confidence at medium/high risk, reversible
        # (observed: low-risk/high-reversible/low-confidence would auto, so it is not used here)
        for risk in ("medium", "high"):
            r = self._answer(risk, "high", "low", *extra)
            self.assertEqual(r.returncode, 2, f"risk={risk}: {r.stdout}")
            self.assertEqual(json.loads(r.stdout), {"answered": False, "policy": "escalate"})
        # control: --approval is exactly what would self-answer it
        ctl = self._answer("medium", "high", "low", "--approval")
        self.assertEqual(ctl.returncode, 0)
        # the prose forbids both the flag and self-answering for the carve
        t = skill_text()
        self.assertIn("charter 彫り直しの問いを自答してはならない", t)
        self.assertIn("charter を彫る問いは「どちらを選ぶべきか」を", t)

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

    def test_claim_judgment_divert_command_as_written_escalates_and_is_journaled(self):
        block = fence_with("specforge へ divert して spec を起こす", lang="bash")
        self.assertNotIn("--approval", block)
        self.assertRegex(block, r"--risk medium --reversible high --confidence low")
        sb = self.sb
        r = sb.sh(block, HARNESS_AUTONOMOUS="1")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertEqual(json.loads(r.stdout), {"answered": False, "policy": "escalate"})
        journals = list(sb.home.glob(".condukt/state/gate-decisions.jsonl"))
        self.assertEqual(len(journals), 1, list(sb.home.rglob("*.jsonl")))
        rec = json.loads(journals[0].read_text().strip().splitlines()[-1])
        self.assertEqual(rec["policy"], "escalate")
        self.assertIn("正典が無い", rec["question"])
        self.assertEqual(rec["recommend_index"], 0)
        self.assertEqual(rec["chosen"], "")
        # control: the same text with --approval spliced in would self-answer (erase the question)
        ctl = sb.sh(block.replace("condukt policy answer", "condukt policy answer --approval", 1),
                    HARNESS_AUTONOMOUS="1")
        self.assertEqual(ctl.returncode, 0, ctl.stdout)

    @staticmethod
    def _pivot_command():
        t = strip_test_tags(skill_text())
        m = re.search(r"pivot` なら Step 0\.5.*?（`--risk (\w+) --reversible (\w+) --confidence (\w+)`、既定 verdict は (\w+)、\s*"
                      r"`--question \"([^\"]+)\" --option \"([^\"]+)\" --option \"([^\"]+)\" --recommend (\d)`）",
                      t, re.S)
        assert m, "pivot gate command not found in SKILL.md"
        return m.groups()

    def test_claim_judgment_pivot_gate_no_approval_escalates_and_recommends_continue(self):
        risk, rev, conf, verdict, q, o0, o1, rec = self._pivot_command()
        self.assertEqual((risk, rev, conf, verdict, rec), ("medium", "high", "low", "escalate", "1"))
        self.assertEqual(o1, "継続")
        args = ["--risk", risk, "--reversible", rev, "--confidence", conf, "--question", q,
                "--option", o0, "--option", o1, "--recommend", rec]
        r = policy(self.sb, *args)
        self.assertEqual(r.returncode, 2, r.stdout)
        ctl = policy(self.sb, *args, "--approval")
        self.assertEqual(ctl.returncode, 0)
        self.assertEqual(json.loads(ctl.stdout)["chosen"], "継続")  # recommend 1 == 継続

    def test_claim_judgment_no_approval_on_judgment_gates_in_skill_text(self):
        t = skill_text()
        # every policy-answer command that carries --approval is a permission-approval gate:
        # the only fenced one is the generic approval template, never a judgment question.
        for lang, body, _ in fences(t):
            if "--approval" in body and "condukt policy answer" in body:
                self.assertIn("進んでよいか", body.replace("進めてよいか", "進んでよいか"))
                for judgment in ("divert", "pivot", "charter", "blocked"):
                    self.assertNotIn(judgment, body)
        # every other mention of --approval is in a prohibition sentence, the permission gates
        # (lock/resume/deploy/Phase 3) or the flag description lines
        allowed_ctx = re.compile(r"付けてはならない|付けない|付けること|付ける|付けて|付けても|付けた|"
                                 r"\|.*--approval|`--approval`|--approval` は|--approval` と|--approval 付き|--approval を付け|"
                                 r"--approval\s+\\|condukt policy answer --approval")
        for i, line in enumerate(strip_test_tags(t).splitlines(), 1):
            if "--approval" in line:
                self.assertRegex(line, allowed_ctx, f"SKILL.md:{i}: {line}")
        self.assertIn("判断要求（人間に選択・設計判断を求める Ask）のゲートに `--approval` を付けてはならない", t)
        self.assertIn("charter 彫り直しの問いを自答してはならない", t)
        self.assertIn("勝手に `/compass` を起動してはならない", t)
        stop_list = re.search(r"自律でも escalate のまま残るのは (\(a\).*?)であり、policy が block を返したゲートも止まる", t, re.S).group(1)
        for tag in ("(a) worker blocked", "(b) pivot", "(c) conflict/untestable", "(d) spec-gap divert",
                    "(e) charter 彫り直し中の問い"):
            self.assertIn(tag, stop_list)

    # ------------------------------------------------------------- step1_order
    def test_claim_step1_order_backlog_before_compass(self):
        t = skill_text()
        blocks = fences(t)
        first_backlog = next(off for l, b, off in blocks if "backlog next --claim" in b)
        first_compass = next(off for l, b, off in blocks if re.search(r"\bcompass gap\b", b))
        self.assertLess(first_backlog, first_compass)
        s1 = t.index("### Step 1")
        s2 = t.index("### Step 2")
        self.assertLess(s1, s2)
        in_step1 = [b for l, b, off in blocks if s1 < off < s2]
        for b in in_step1:
            self.assertNotRegex(b, r"\bcompass\b")
        self.assertNotRegex(t[s1:s2].split("\n", 3)[3], r"`compass gap`\s*$")
        # compass gap in a fenced block only ever appears after step 2
        for l, b, off in blocks:
            if re.search(r"\bcompass (gap|charter)\b", b):
                self.assertGreater(off, s2)

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

    def test_claim_decomp_fields_open_questions_is_the_researcher_field_not_the_interpreters(self):
        """FALSE finding (trigger 1 wording): SKILL.md says "condukt interpreter が
        `open_questions` を出したとき". The interpreter's schema has no open_questions;
        the field is defined by the RESEARCHER agent (research_brief). The engine's
        Decomposition type has no such field either (it is silently ignored)."""
        interp = (CONDUKT_AGENTS / "condukt-interpreter.md").read_text(encoding="utf-8")
        research = (CONDUKT_AGENTS / "condukt-researcher.md").read_text(encoding="utf-8")
        self.assertNotIn("open_questions", interp)
        self.assertIn('"open_questions"', research)
        model = (ROOT / "crates" / "condukt" / "src" / "model.rs").read_text()
        decomp_struct = re.search(r"pub struct Decomposition \{.*?\n\}", model, re.S).group(0)
        self.assertNotIn("open_questions", decomp_struct)
        self.assertIn("trigger 1: condukt-researcher の research brief が `open_questions` を出したとき", skill_text())

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

    @unittest.expectedFailure
    def test_claim_circuit_flow_invocation_streak_and_budget_axes_are_dead(self):
        """FINDING (expected failure = the fail-closed property does NOT hold today).
        /flow runs `condukt circuit check --run flow-$SID --session $SID`: it never runs
        `state init` for that run and passes no --budget-cap-usd. Item failures are recorded
        with `backlog fail`, which the circuit cannot see, so the failure-streak axis is
        structurally 0 and the budget axis is disabled. Property wanted: after three failed
        items the exact SKILL command can trip on failure_streak. Observed: streak 0,
        budget_over_cap false, exit 0 (see the assertion message)."""
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
