"""Claim-reproduction tests for crates/flow (skill-only plugin), part B.

Every test pins what is OBSERVED today about a prose claim in
crates/flow/skills/flow/SKILL.md. Commands are extracted from the SKILL text
(fenced blocks / inline code) so a prose change breaks the test. The SKILL path
can be redirected with FLOW_SKILL_MD (used for RED checks).

Real binaries (backlog, compass, hypothesis, overwatch, specforge) are built
once from this checkout and run with a temp HOME, temp git repos and, where the
store boundary matters, a linked worktree. The real ~/.claude, ~/.backlog and
$HOME are never touched.

Run: python3 -m pytest -q scripts/test_flow_snippets_b.py
"""
import json
import os
import re
import shlex
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "crates" / "flow"
SKILL = Path(os.environ.get("FLOW_SKILL_MD", CRATE / "skills" / "flow" / "SKILL.md"))
TAXONOMY = ROOT / "docs" / "backlog-tag-taxonomy.md"

CHARTER_JSON = json.dumps({
    "north_star": "ship it",
    "definition_of_done": ["tests pass"],
    "measuring_stick": "tests passing",
    "current_gap": "gap",
    "next_action": "act",
    "parked": [],
})

_BUILT = False


def target_dir():
    return Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug"


def bin_path(name):
    global _BUILT
    if not _BUILT:
        cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
        r = subprocess.run(
            [cargo, "build", "-q", "-p", "backlog", "-p", "compass", "-p", "hypothesis",
             "-p", "overwatch", "-p", "specguard"],
            cwd=ROOT, capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        _BUILT = True
    p = target_dir() / name
    assert p.exists(), p
    return p


def skill_text():
    # citation markers ` [test: ...]` are bookkeeping, not prose
    return re.sub(r" ?\[test: [^\]]+\]", "", SKILL.read_text(encoding="utf-8"))


def skill_cmd(prefix, subs=None):
    """Extract the command line starting with `prefix` from SKILL.md (fenced
    block or inline code), joining `\\` continuations and dropping a trailing
    `  # comment`. Returns the shlex-split argv with `subs` (token -> value)
    applied. Raises AssertionError when the SKILL has no such command."""
    lines = skill_text().splitlines()
    # fenced/standalone lines win over inline-code mentions in prose
    hits = [(i, raw.strip()) for i, raw in enumerate(lines) if raw.strip().startswith(prefix)]
    for i, raw in enumerate(lines):
        im = re.search(r"`(" + re.escape(prefix) + r"[^`]*)`", raw)
        if im and not raw.strip().startswith(prefix):
            hits.append((i, im.group(1)))
    for i, m in hits:
        j = i
        while m.endswith("\\"):
            j += 1
            m = m[:-1] + " " + lines[j].strip()
        m = re.sub(r"\s{2,}#.*$", "", m)
        for k, v in (subs or {}).items():
            m = m.replace(k, v)
        return shlex.split(m)
    raise AssertionError(f"SKILL.md has no command starting {prefix!r}")


def skill_has(phrase):
    assert phrase in skill_text(), f"SKILL.md no longer contains {phrase!r}"


def flag_value(argv, flag, value):
    argv = list(argv)
    i = argv.index(flag)
    argv[i + 1] = value
    return argv


class Sandbox:
    """Temp base dir with a HOME, a shim dir (fake condukt answering
    'not claimed') and helpers to run the real binaries."""

    def __init__(self, tc):
        self.tmp = tempfile.TemporaryDirectory(prefix="flowb.")
        tc.addCleanup(self.tmp.cleanup)
        self.base = Path(os.path.realpath(self.tmp.name))
        self.home = self.base / "home"
        self.shim = self.base / "shim"
        self.extra_bin = self.base / "extrabin"
        for d in (self.home, self.shim, self.extra_bin):
            d.mkdir()
        c = self.shim / "condukt"
        c.write_text("#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n")
        c.chmod(c.stat().st_mode | stat.S_IXUSR)

    def env(self, **extra):
        e = {
            "HOME": str(self.home),
            "PATH": f"{self.extra_bin}:{self.shim}:/usr/bin:/bin:/usr/sbin:/sbin",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_SYSTEM": "/dev/null",
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t",
        }
        e.update(extra)
        return e

    def run(self, argv, cwd, stdin=None, **extra_env):
        argv = list(argv)
        exe = argv[0]
        if exe in ("backlog", "compass", "hypothesis", "overwatch", "specforge"):
            argv[0] = str(bin_path(exe))
        return subprocess.run(argv, cwd=cwd, env=self.env(**extra_env), input=stdin,
                              capture_output=True, text=True)

    def git(self, cwd, *args):
        r = subprocess.run(["git", *args], cwd=cwd, env=self.env(),
                           capture_output=True, text=True)
        assert r.returncode == 0, f"git {args}: {r.stderr}"
        return r.stdout

    def init_repo(self, name="primary", files=None):
        p = self.base / name
        p.mkdir()
        self.git(p, "init", "-q", "-b", "main")
        (p / "README").write_text("x\n")
        for rel, body in (files or {}).items():
            f = p / rel
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(body)
        self.git(p, "add", "-A")
        self.git(p, "commit", "-q", "-m", "init")
        return p

    def worktree(self, primary, name):
        w = self.base / name
        self.git(primary, "worktree", "add", "-q", "-b", name, str(w))
        return w


FAIL_SH = "#!/bin/sh\necho boom >&2\nexit 1\n"


def backlog_fixture(tc, titles=("T one", "T two"), tags=None):
    """Primary checkout with .backlog/tasks.toml committed (tasks filed by the
    real `backlog add` from a throwaway linked worktree, each reproduced by a
    failing tracked script), plus a fresh linked worktree `w`.
    Returns (sandbox, primary, linked, ids)."""
    sb = Sandbox(tc)
    primary = sb.init_repo(files={"tests/fail.sh": FAIL_SH})
    os.chmod(primary / "tests" / "fail.sh", 0o755)
    seeder = sb.worktree(primary, "seeder")
    ids = []
    for i, t in enumerate(titles):
        args = ["backlog", "add", "--title", t, "--project", str(seeder),
                "--repro-test", "bash tests/fail.sh"]
        if i == 0:
            args += ["--priority", "p1"]
        for tag in (tags or {}).get(t, []):
            args += ["--tag", tag]
        r = sb.run(args, seeder)
        assert r.returncode == 0, r.stderr
        ids.append(re.search(r"added: (\w+)", r.stdout).group(1))
    (primary / ".backlog").mkdir()
    shutil.copy(seeder / ".backlog" / "tasks.toml", primary / ".backlog" / "tasks.toml")
    sb.git(primary, "add", "-A")
    sb.git(primary, "commit", "-q", "-m", "seed backlog")
    sb.git(primary, "worktree", "remove", "--force", str(seeder))
    linked = sb.worktree(primary, "w")
    return sb, primary, linked, ids


def compass_fixture(tc, charter=True):
    sb = Sandbox(tc)
    primary = sb.init_repo()
    if charter:
        scratch = sb.base / "scratch"
        scratch.mkdir()
        r = sb.run(["compass", "charter", "--write", CHARTER_JSON], scratch)
        assert r.returncode == 0, r.stderr
        (primary / ".compass").mkdir()
        shutil.copy(scratch / ".compass" / "charter.md", primary / ".compass" / "charter.md")
        sb.git(primary, "add", "-A")
        sb.git(primary, "commit", "-q", "-m", "charter")
    linked = sb.worktree(primary, "w")
    return sb, primary, linked


def tree_snapshot(root):
    snap = {}
    for p in sorted(Path(root).rglob("*")):
        if ".git" in p.relative_to(root).parts[:1]:
            continue
        if p.is_file():
            snap[str(p.relative_to(root))] = p.read_bytes()
    return snap


def first_json(text):
    obj, _ = json.JSONDecoder().raw_decode(text.lstrip())
    return obj


class TestMainReads(unittest.TestCase):
    def test_claim_main_reads_backlog_next(self):
        skill_has("`next`（--claim なし）と `compass gap`（--write なし）も main からでも動く。")
        sb, primary, _linked, ids = backlog_fixture(self)
        before = tree_snapshot(primary)
        claims_before = tree_snapshot(sb.home)
        argv = ["backlog", "next"]
        r = sb.run(argv, primary)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(first_json(r.stdout)["id"], ids[0])
        self.assertEqual(tree_snapshot(primary), before, "next wrote into main's tree")
        self.assertEqual(sb.git(primary, "status", "--porcelain"), "")
        self.assertEqual(tree_snapshot(sb.home), claims_before, "next (no --claim) must not lease")
        # control: a store WRITE from the same primary checkout IS refused
        w = sb.run(["backlog", "add", "--title", "z", "--project", str(primary),
                    "--repro-test", "bash tests/fail.sh"], primary)
        self.assertNotEqual(w.returncode, 0)
        self.assertEqual(tree_snapshot(primary), before)

    def test_claim_main_reads_compass_gap(self):
        sb, primary, _linked = compass_fixture(self)
        cmd = skill_cmd("compass gap")
        self.assertEqual(cmd, ["compass", "gap"], "SKILL's fenced `compass gap` must be write-less")
        before = tree_snapshot(primary)
        r = sb.run(cmd, primary)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("measuring_stick", json.loads(r.stdout))
        self.assertEqual(tree_snapshot(primary), before, "gap (no --write) wrote into main's tree")
        self.assertEqual(sb.git(primary, "status", "--porcelain"), "")
        # control: gap --write from primary is refused and leaves the charter alone
        w = sb.run(["compass", "gap", "--write", "x"], primary)
        self.assertNotEqual(w.returncode, 0)
        self.assertEqual(tree_snapshot(primary), before)


class TestWeightDefault(unittest.TestCase):
    def test_claim_weight_default(self):
        skill_has("weight 無指定は既定 0.0 である。")
        sb, _primary, linked, _ids = backlog_fixture(self, titles=("seed",))
        r = sb.run(["backlog", "add", "--title", "no weight given", "--project", str(linked),
                    "--repro-test", "bash tests/fail.sh"], linked)
        self.assertEqual(r.returncode, 0, r.stderr)
        tasks = json.loads(sb.run(["backlog", "list", "--json"], linked).stdout)
        t = next(t for t in tasks if t["title"] == "no weight given")
        self.assertEqual(t["weight"], 0.0)
        self.assertIn("weight = 0.0", (linked / ".backlog" / "tasks.toml").read_text())
        # control: an explicit weight is stored as given
        r2 = sb.run(["backlog", "add", "--title", "weighted", "--project", str(linked),
                     "--weight", "2.5", "--repro-test", "bash tests/fail.sh"], linked)
        self.assertEqual(r2.returncode, 0, r2.stderr)
        tasks = json.loads(sb.run(["backlog", "list", "--json"], linked).stdout)
        self.assertEqual(next(t for t in tasks if t["title"] == "weighted")["weight"], 2.5)


class TestTagTaxonomy(unittest.TestCase):
    def test_claim_tag_taxonomy(self):
        text = skill_text()
        self.assertIn("docs/backlog-tag-taxonomy.md", text)
        tags = ["worktree-hygiene", "deploy-verify", "network-infra"]
        for t in tags:
            self.assertIn(f"`{t}`", text, f"SKILL no longer names {t}")
        self.assertTrue(TAXONOMY.is_file(), "docs/backlog-tag-taxonomy.md missing")
        doc = TAXONOMY.read_text(encoding="utf-8")
        for t in tags:
            self.assertRegex(doc, r"(?m)^- \*\*" + re.escape(t) + r"\*\*", f"{t} not defined")
        sb, _primary, linked, _ = backlog_fixture(self, titles=("seed",))
        for t in tags:
            r = sb.run(["backlog", "add", "--title", f"tagged {t}", "--project", str(linked),
                        "--tag", t, "--repro-test", "bash tests/fail.sh"], linked)
            self.assertEqual(r.returncode, 0, r.stderr)
        tasks = json.loads(sb.run(["backlog", "list", "--json"], linked).stdout)
        for t in tags:
            row = next(x for x in tasks if x["title"] == f"tagged {t}")
            self.assertIn(t, row["tags"])
        by_tag = json.loads(sb.run(["backlog", "list", "--json", "--tag", tags[0]], linked).stdout)
        self.assertEqual([x["title"] for x in by_tag], [f"tagged {tags[0]}"])


class TestGapRoute(unittest.TestCase):
    """Pins what `compass gap` and `compass route` ACTUALLY output. (The earlier
    prose told the reader to take `to_condukt` from `compass gap`; that defect,
    backlog acf4f7ba, is fixed and scripts/test_backlog_acf4f7ba.py now guards
    the prose against naming gap fields that gap does not output.)"""

    GAP_KEYS = {"dod", "recent_activity", "progress_excerpt", "measuring_stick",
                "last_outcome", "opportunities"}

    def test_claim_gap_route_gap_keys(self):
        sb, _primary, linked = compass_fixture(self)
        r = sb.run(skill_cmd("compass gap"), linked)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(set(json.loads(r.stdout)), self.GAP_KEYS)
        for absent in ("to_condukt", "north_star", "current_gap"):
            self.assertNotIn(absent, json.loads(r.stdout))

    def test_claim_gap_route_route_emits_to_condukt_and_context(self):
        sb, primary, linked = compass_fixture(self)
        dec = {"goal": "g", "tasks": [
            {"id": "t1", "title": "small", "size": "s", "done_criteria": "d"},
            {"id": "t2", "title": "huge", "size": "xl"}]}
        r = sb.run(["compass", "route"], linked, stdin=json.dumps(dec))
        self.assertEqual(r.returncode, 0, r.stderr)
        routing = first_json(r.stdout)
        self.assertEqual([t["id"] for t in routing["to_condukt"]], ["t1"])
        self.assertEqual([t["id"] for t in routing["parked"]], ["t2"])
        self.assertIsNone(routing["edge"])
        # the handoff text after the JSON carries the 3 context fields the SKILL asks for
        self.assertIn("- north_star: ship it", r.stdout)
        self.assertIn("- current_gap: gap", r.stdout)
        self.assertIn("- measuring_stick: tests passing", r.stdout)
        # route writes parked tasks to .claude/progress.md of the worktree
        self.assertIn("huge", (linked / ".claude" / "progress.md").read_text())
        # route needs a decomposition JSON on stdin (or --file); without one it fails
        bad = sb.run(["compass", "route"], linked, stdin="")
        self.assertNotEqual(bad.returncode, 0)
        # and, being a writer, it is refused from the primary checkout
        before = tree_snapshot(primary)
        pr = sb.run(["compass", "route"], primary, stdin=json.dumps(dec))
        self.assertNotEqual(pr.returncode, 0)
        self.assertEqual(tree_snapshot(primary), before)


class TestCarveSentinel(unittest.TestCase):
    def test_claim_carve_sentinel(self):
        sb, primary, linked = compass_fixture(self, charter=False)
        r = sb.run(["compass", "charter", "--write", CHARTER_JSON], linked)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((linked / ".compass" / "charter.md").is_file())
        self.assertFalse((primary / ".compass").exists())
        ev = sb.run(["compass", "evaluate"], linked)
        self.assertEqual(ev.returncode, 0, ev.stderr)
        j = json.loads(ev.stdout)
        self.assertEqual(set(j), {"open_questions", "status", "round"})
        self.assertEqual(j["status"], "open")
        self.assertTrue(j["open_questions"])
        # a human who walks away answers defer:true -> status becomes "sentinel"
        ans = {"gate": "C3", "reference": "definition_of_done[0]", "value": "x", "defer": True}
        ap = sb.run(["compass", "apply", "--answer", json.dumps(ans)], linked)
        self.assertEqual(ap.returncode, 0, ap.stderr)
        self.assertEqual(json.loads(ap.stdout)["status"], "sentinel")
        skill_has("`sentinel` に落ちた場合は")

    def test_claim_carve_sentinel_no_defer_stays_open(self):
        sb, _primary, linked = compass_fixture(self, charter=False)
        sb.run(["compass", "charter", "--write", CHARTER_JSON], linked)
        sb.run(["compass", "evaluate"], linked)
        ans = {"gate": "C3", "reference": "definition_of_done[0]", "value": "x", "defer": False}
        ap = sb.run(["compass", "apply", "--answer", json.dumps(ans)], linked)
        self.assertEqual(ap.returncode, 0, ap.stderr)
        self.assertNotEqual(json.loads(ap.stdout)["status"], "sentinel")

    def test_claim_carve_sentinel_bare_gap_write_is_usage_error(self):
        """The earlier prose said bare `compass gap --write`; that is a clap usage
        error (exit 2). The test pins the corrected claim: `--write` takes a TEXT
        value, as the SKILL now writes it."""
        skill_has('`compass gap --write "<gap>"`')
        sb, _primary, linked = compass_fixture(self)
        bare = sb.run(["compass", "gap", "--write"], linked)
        self.assertEqual(bare.returncode, 2)
        self.assertIn("a value is required for '--write <TEXT>'", bare.stderr)
        ok = sb.run(["compass", "gap", "--write", "new gap text"], linked)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("new gap text", (linked / ".compass" / "charter.md").read_text())


class TestCompassFailDetectable(unittest.TestCase):
    def test_claim_compass_fail_detectable_no_charter(self):
        sb, _primary, linked = compass_fixture(self, charter=False)
        r = sb.run(["compass", "gap"], linked)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout.strip(), "", "failure must not emit a plausible empty move")
        self.assertIn("charter", r.stderr)

    def test_claim_compass_fail_detectable_unreadable_charter(self):
        sb, _primary, linked = compass_fixture(self, charter=False)
        (linked / ".compass").mkdir()
        (linked / ".compass" / "charter.md").write_bytes(b"\xff\xfe\x00bad")
        r = sb.run(["compass", "gap"], linked)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout.strip(), "")

    def test_claim_compass_fail_detectable_garbage_charter(self):
        """The claim is that a corrupt charter is a detectable failure. A UTF-8
        charter that parses to nothing (here: unclosed front matter) once exited
        0 with dod=[] and measuring_stick="" - an empty move that reads as
        'no gap' (backlog 50bceba2). This asserts the claimed behaviour."""
        sb, _primary, linked = compass_fixture(self, charter=False)
        (linked / ".compass").mkdir()
        (linked / ".compass" / "charter.md").write_text("---\nnorth_star: [unclosed\n---\n")
        r = sb.run(["compass", "gap"], linked)
        self.assertNotEqual(r.returncode, 0, f"exit 0 with stdout {r.stdout[:120]!r}")


class TestBacklogNextError(unittest.TestCase):
    def test_claim_backlog_next_error(self):
        sb, _primary, linked, _ = backlog_fixture(self)
        store = linked / ".backlog" / "tasks.toml"
        good = store.read_bytes()
        store.write_text("this is = = not toml [[\n")
        for extra in ([], ["--claim"]):
            r = sb.run(["backlog", "next", *extra], linked)
            self.assertNotEqual(r.returncode, 0, f"next {extra}: {r.stdout!r}")
            self.assertIn("failed to parse", r.stderr)
            self.assertNotIn("no pending tasks", r.stdout)
        # control: the same command on the intact store succeeds and hands out a task
        store.write_bytes(good)
        ok = sb.run(["backlog", "next"], linked)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn('"id"', ok.stdout)


def hyp_id(sb, cwd, text):
    r = sb.run(["hypothesis", "add", text], cwd)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip().split()[0]


class TestRat(unittest.TestCase):
    def test_claim_rat(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        hid = hyp_id(sb, repo, "h1")
        for text, risk, ev in (("low strong", "low", "strong"),
                               ("BIG leap", "high", "none"),
                               ("mid weak", "medium", "weak")):
            r = sb.run(["hypothesis", "assume", hid, "--text", text, "--risk", risk,
                        "--evidence", ev], repo)
            self.assertEqual(r.returncode, 0, r.stderr)
        rat_cmd = skill_cmd("RAT=$(hypothesis rat <hid>)")
        self.assertEqual(rat_cmd[0], "RAT=$(hypothesis")
        rat = lambda: sb.run(["hypothesis", "rat", hid], repo)
        r = rat()
        self.assertEqual(r.returncode, 0, r.stderr)
        line = r.stdout.strip()
        self.assertRegex(line, r"^1\t", "output line must START with the assumption index")
        self.assertIn("BIG leap", line)
        # tested: <hid> <index>  -- the SKILL's closing call
        t = sb.run(["hypothesis", "tested", hid, "1"], repo)
        self.assertEqual(t.returncode, 0, t.stderr)
        r2 = rat()
        self.assertEqual(r2.returncode, 0)
        self.assertEqual(r2.stdout.strip(), "", "tested assumption must no longer be the RAT")
        skill_has("`hypothesis tested <hid> <index>`")

    def test_claim_rat_unknown_hypothesis_is_an_error_not_empty(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        r = sb.run(["hypothesis", "rat", "nonexist"], repo)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(r.stdout.strip(), "")

    def test_claim_rat_tested_out_of_range_fails(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        hid = hyp_id(sb, repo, "h1")
        r = sb.run(["hypothesis", "tested", hid, "9"], repo)
        self.assertNotEqual(r.returncode, 0)


class TestHypStatus(unittest.TestCase):
    def test_claim_hyp_status(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        a = hyp_id(sb, repo, "stays open")
        b = hyp_id(sb, repo, "shipped")
        r = sb.run(["hypothesis", "await-measurement", b], repo)
        self.assertEqual(r.returncode, 0, r.stderr)
        awaiting = skill_cmd("hypothesis list --status awaiting-measurement")
        opened = skill_cmd("hypothesis list --status open")
        ra = sb.run(awaiting, repo)
        ro = sb.run(opened, repo)
        self.assertEqual(ra.returncode, 0, ra.stderr)
        self.assertEqual(ro.returncode, 0, ro.stderr)
        self.assertIn(b, ra.stdout)
        self.assertNotIn(a, ra.stdout)
        self.assertIn(a, ro.stdout)
        self.assertNotIn(b, ro.stdout)
        js = json.loads(sb.run(awaiting + ["--json"], repo).stdout)
        self.assertEqual([(h["id"], h["status"]) for h in js], [(b, "awaiting-measurement")])


OVERWATCH_SESSION = "flowb-session"


def ow(sb, repo, argv, **kw):
    return sb.run(argv, repo, CLAUDE_CODE_SESSION_ID=OVERWATCH_SESSION, **kw)


class TestOverwatchBegin(unittest.TestCase):
    def begin_cmd(self, key, title, scope=None):
        argv = skill_cmd("overwatch begin --key", {
            "<pdo-unit-id>": key, "<task title>": title,
            "<done_criteria>": "dc"})
        argv = flag_value(argv, "--scope", scope or "")
        if scope is None:
            i = argv.index("--scope")
            del argv[i:i + 2]
        return ["overwatch", *argv[1:]]

    def test_claim_overwatch_begin(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        r1 = ow(sb, repo, self.begin_cmd("K1", "T1", "a.rs,b.rs"))
        self.assertEqual(r1.returncode, 0, r1.stderr)
        j1 = json.loads(r1.stdout)
        self.assertTrue(j1["scope_declared"])
        lease = json.loads(ow(sb, repo, ["overwatch", "lease", "--session", OVERWATCH_SESSION,
                                         "--json"]).stdout)
        self.assertEqual(lease["key"], "K1")
        self.assertEqual(lease["scope"], ["a.rs", "b.rs"], "comma list is split into files")
        # a distinct key coexists; no --scope => scope_undeclared, NOT 'no overlap'
        r2 = ow(sb, repo, self.begin_cmd("K2", "T2"))
        self.assertEqual(r2.returncode, 0, r2.stderr)
        j2 = json.loads(r2.stdout)
        self.assertFalse(j2["scope_declared"])
        self.assertEqual(j2["scope_overlap"], [])
        self.assertIn("K1", [p["key"] for p in j2["scope_undeclared"]])
        sessions = ow(sb, repo, ["overwatch", "sessions"]).stdout
        self.assertIn("(2 live)", sessions)
        self.assertIn("K1", sessions)
        self.assertIn("K2", sessions)

    def test_claim_overwatch_begin_overlap_is_reported_for_shared_scope(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        self.assertEqual(ow(sb, repo, self.begin_cmd("K1", "T1", "a.rs,b.rs")).returncode, 0)
        other = self.begin_cmd("K3", "T3", "b.rs,c.rs") + ["--session", "other"]
        r = ow(sb, repo, other)
        self.assertEqual(r.returncode, 0, r.stderr)
        j = json.loads(r.stdout)
        self.assertEqual([p["key"] for p in j["scope_overlap"]], ["K1"])


class TestOverwatchEnd(unittest.TestCase):
    def end_cmd(self, key, status):
        argv = skill_cmd("overwatch end --key", {"<pdo-unit-id>": key, "<done|abandoned>": status})
        return ["overwatch", *argv[1:]]

    def test_claim_overwatch_end(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        for k in ("K1", "K2", "K3"):
            r = ow(sb, repo, ["overwatch", "begin", "--key", k, "--title", k, "--scope", "a.rs"])
            self.assertEqual(r.returncode, 0, r.stderr)
        r = ow(sb, repo, self.end_cmd("K1", "abandoned"))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("K1", r.stdout)
        live = ow(sb, repo, ["overwatch", "sessions"]).stdout
        self.assertIn("(2 live)", live)
        self.assertNotIn("K1", live)
        self.assertIn("K2", live)
        self.assertIn("K3", live)
        r = ow(sb, repo, self.end_cmd("K2", "done"))
        self.assertEqual(r.returncode, 0, r.stderr)
        live = ow(sb, repo, ["overwatch", "sessions"]).stdout
        self.assertIn("(1 live)", live)
        self.assertIn("K3", live)

    def test_claim_overwatch_end_unknown_key_is_distinguishable(self):
        sb = Sandbox(self)
        repo = sb.init_repo()
        r = ow(sb, repo, self.end_cmd("NOPE", "done"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no lease held under key `NOPE`", r.stderr)


class TestFailVsPending(unittest.TestCase):
    def picked(self, sb, cwd):
        r = sb.run(["backlog", "next", "--claim", "--project", str(cwd)], cwd)
        self.assertEqual(r.returncode, 0, r.stderr)
        try:
            return first_json(r.stdout)["id"]
        except Exception:
            return None

    def test_claim_fail_vs_pending_fail_withdraws_task(self):
        sb, _p, linked, (a, b) = backlog_fixture(self)
        fail = skill_cmd("backlog fail <id> --reason", {"<id>": a, '<概要>': "x"})
        self.assertEqual(fail[:2], ["backlog", "fail"])
        r = sb.run(["backlog", "fail", a, "--reason", "x"], linked)
        self.assertEqual(r.returncode, 0, r.stderr)
        pending = sb.run(["backlog", "list", "--status", "pending"], linked).stdout
        self.assertNotIn(a, pending)
        self.assertEqual(self.picked(sb, linked), b, "failed task must not be handed out")
        self.assertIsNone(self.picked(sb, linked), "and nothing else is left to hand out")

    def test_claim_fail_vs_pending_edit_pending_keeps_task_deferred(self):
        """The earlier prose claimed `backlog edit <id> --status pending` requeues a
        failed task. The test pins the observed behaviour: `fail` sets defer_until
        (+2 days) and edit leaves it set, so list shows `deferred` and
        `next --claim` skips it."""
        skill_has("`backlog edit <id> --status pending`")
        sb, _p, linked, (a, b) = backlog_fixture(self)
        self.assertEqual(sb.run(["backlog", "fail", a, "--reason", "x"], linked).returncode, 0)
        e = sb.run(["backlog", "edit", a, "--status", "pending"], linked)
        self.assertEqual(e.returncode, 0, e.stderr)
        rows = {t["id"]: t for t in json.loads(sb.run(["backlog", "list", "--json"], linked).stdout)}
        self.assertEqual(rows[a]["status"], "deferred")
        self.assertIsNotNone(rows[a]["defer_until"])
        self.assertEqual(self.picked(sb, linked), b)
        self.assertIsNone(self.picked(sb, linked), "A is still withheld after edit --status pending")

    def test_claim_fail_vs_pending_done_closes_only_that_id(self):
        sb, _p, linked, (a, b) = backlog_fixture(self)
        r = sb.run(["backlog", "done", b, "--duplicate-of", a], linked)
        self.assertEqual(r.returncode, 0, r.stderr)
        rows = {t["id"]: t["status"] for t in
                json.loads(sb.run(["backlog", "list", "--json"], linked).stdout)}
        self.assertEqual(rows, {a: "pending", b: "done"})


def forge_config(agent_output):
    script = f"cat >/dev/null; cat <<'FORGE_EOF'\n{agent_output}\nFORGE_EOF"
    return (
        '[project]\nname = "Demo"\nroot = "."\n\n[agent]\ncommand = "bash"\n'
        f"args = [\"-c\", {json.dumps(script)}]\n\n"
        '[output]\nspec_dir = "specs"\nsentinel = ".forge-pending"\n'
    )


TWO_REQ_DRAFT = """[[requirement]]
id = "R1"
statement = "first"
acceptance = ["a1", "a2"]
canon = ["docs/a.md#x"]
falsifiable = true

[[requirement]]
id = "R2"
statement = "second"
acceptance = ["b1"]
canon = ["docs/a.md#y"]
falsifiable = true

<<<SPEC_DRAFT>>>
rigor: pass
needs_user: no
summary: s"""


class TestSpecforge(unittest.TestCase):
    def setUp(self):
        self.sb = Sandbox(self)
        self.primary = self.sb.init_repo(files={"specforge.toml": forge_config(TWO_REQ_DRAFT)})
        self.w = self.sb.worktree(self.primary, "w")
        (self.sb.base / "req.md").write_text("req text\n")
        self.req = str(self.sb.base / "req.md")

    def draft_argv(self, sid, req=True):
        argv = skill_cmd("specforge draft --id", {"<spec id>": sid, "<課題タイトル>": "title"})
        argv = ["specforge", *argv[1:]]
        self.assertIn("--req", argv)
        self.assertIn("--id", argv)
        if req:
            argv = flag_value(argv, "--req", self.req)
        else:
            i = argv.index("--req")
            del argv[i:i + 2]
        return argv

    def ratify_argv(self, sid):
        argv = skill_cmd("specforge ratify --id", {"<spec id>": sid, "<なぜこの spec を受け入れるか>": "because"})
        self.assertIn("--reason", argv)
        return ["specforge", *argv[1:]]

    def status_of(self, sid):
        return re.search(r'(?m)^status = "(\w+)"', (self.w / "specs" / f"{sid}.toml").read_text()).group(1)

    def fake_backlog(self, log):
        """A `backlog` on PATH that logs argv: list -> [], add -> ok."""
        f = self.sb.extra_bin / "backlog"
        f.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {shlex.quote(str(log))}\n"
            'if [ "$1" = "list" ]; then echo "[]"; fi\n'
            "exit 0\n")
        f.chmod(f.stat().st_mode | stat.S_IXUSR)

    def test_claim_specforge_positional_arg_rejected(self):
        r = self.sb.run(["specforge", "draft", "a positional issue text", "--id", "p"], self.w)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("unexpected argument", r.stderr)
        self.assertFalse((self.w / "specs" / "p.toml").exists())

    def test_claim_specforge_draft_requires_id(self):
        r = self.sb.run(["specforge", "draft", "--req", self.req], self.w)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("--id", r.stderr)

    def test_claim_specforge_draft_via_req_file_and_stdin(self):
        r = self.sb.run(self.draft_argv("viafile"), self.w)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.status_of("viafile"), "draft")
        r2 = self.sb.run(self.draft_argv("viastdin", req=False), self.w, stdin="req from stdin\n")
        self.assertEqual(r2.returncode, 0, r2.stderr)
        self.assertEqual(self.status_of("viastdin"), "draft")

    def test_claim_specforge_ratify_requires_reason(self):
        self.assertEqual(self.sb.run(self.draft_argv("a"), self.w).returncode, 0)
        r = self.sb.run(["specforge", "ratify", "--id", "a"], self.w)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("--reason", r.stderr)
        self.assertEqual(self.status_of("a"), "draft")

    def test_claim_specforge_ratify_refuses_contract_violation_and_queues_nothing(self):
        log = self.sb.base / "backlog.log"
        self.fake_backlog(log)
        self.assertEqual(self.sb.run(self.draft_argv("bad"), self.w).returncode, 0)
        spec = self.w / "specs" / "bad.toml"
        spec.write_text(spec.read_text().replace("falsifiable = true", "falsifiable = false"))
        r = self.sb.run(self.ratify_argv("bad"), self.w)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("批准を拒否", r.stderr)
        self.assertEqual(self.status_of("bad"), "draft")
        self.assertFalse(log.exists(), "nothing may reach the queue for a refused spec")

    def test_claim_specforge_ratify_queues_one_item_per_requirement(self):
        log = self.sb.base / "backlog.log"
        self.fake_backlog(log)
        self.assertEqual(self.sb.run(self.draft_argv("ok"), self.w).returncode, 0)
        r = self.sb.run(self.ratify_argv("ok"), self.w)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.status_of("ok"), "ratified")
        adds = [l for l in log.read_text().splitlines() if l.startswith("add ")]
        self.assertEqual(len(adds), 2)
        full = log.read_text()
        self.assertIn("--tag req:ok:R1", full)
        self.assertIn("--tag req:ok:R2", full)
        self.assertIn("2 件を起票", r.stdout)

    def test_claim_specforge_ratify_with_real_backlog_exits_9_and_files_nothing(self):
        """The earlier prose claimed a passing ratify makes the items appear in
        backlog. The test pins the observed behaviour: the real `backlog add` is
        REFUSED because specforge passes no --repro-test (no test, no ticket), so
        ratify promotes the spec to ratified, then exits 9 with nothing queued."""
        shutil.copy(bin_path("backlog"), self.sb.extra_bin / "backlog")
        self.assertEqual(self.sb.run(self.draft_argv("real"), self.w).returncode, 0)
        r = self.sb.run(self.ratify_argv("real"), self.w)
        self.assertEqual(r.returncode, 9, r.stdout + r.stderr)
        self.assertIn("no --repro-test was given", r.stderr)
        self.assertEqual(self.status_of("real"), "ratified")
        listed = self.sb.run(["backlog", "list", "--json"], self.w)
        tasks = json.loads(listed.stdout) if listed.stdout.strip() else []
        self.assertEqual([t for t in tasks if "spec:real" in t.get("tags", [])], [])

    def test_claim_specforge_without_config_exits_2(self):
        bare = self.sb.base / "bare"
        bare.mkdir()
        r = self.sb.run(self.draft_argv("x"), bare)
        self.assertEqual(r.returncode, 2)
        self.assertIn("specforge.toml", r.stderr)


class TestListPure(unittest.TestCase):
    def claims(self, sb):
        return tree_snapshot(sb.home / ".backlog" / "claims") \
            if (sb.home / ".backlog" / "claims").exists() else {}

    def test_claim_list_pure(self):
        skill_has("`backlog list --status pending` はピックに使ってはいけない。")
        sb, _p, linked, (a, b) = backlog_fixture(self)
        before = self.claims(sb)
        list_cmd = ["backlog", "list", "--status", "pending", "--json"]
        r1 = sb.run(list_cmd, linked)
        r2 = sb.run(list_cmd, linked)
        self.assertEqual(r1.returncode, 0, r1.stderr)
        self.assertEqual(r1.stdout, r2.stdout)
        ids = [t["id"] for t in json.loads(r1.stdout)]
        self.assertEqual(set(ids), {a, b})
        self.assertEqual(self.claims(sb), before, "list must not write a lease")
        # control: the SKILL's pick, `next --claim`, DOES lease and hands out distinct tasks
        pick = ["backlog", "next", "--claim", "--project", str(linked)]
        self.assertEqual(skill_cmd("backlog next --claim --project", {"$PWD": str(linked)})[:3],
                         ["backlog", "next", "--claim"])
        p1 = first_json(sb.run(pick, linked).stdout)["id"]
        p2 = first_json(sb.run(pick, linked).stdout)["id"]
        self.assertEqual({p1, p2}, {a, b})
        self.assertNotEqual(self.claims(sb), before)
        after = sb.run(["backlog", "list", "--json"], linked).stdout
        self.assertEqual({t["status"] for t in json.loads(after)}, {"claimed"})


class TestCompassOutcome(unittest.TestCase):
    def test_claim_compass_outcome(self):
        sb, primary, linked = compass_fixture(self)
        base_cmd = skill_cmd("compass outcome --verdict", {
            "<forward|unchanged|backward>": "forward", "<観測した成果>": "12 tests pass"})
        self.assertEqual(base_cmd[:2], ["compass", "outcome"])
        i = base_cmd.index("--evidence")
        no_evidence = base_cmd[:i]
        r = sb.run(no_evidence, linked)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("requires measured evidence", r.stderr)
        self.assertFalse((linked / ".compass" / "outcomes.json").exists(), "refusal wrote nothing")
        blank = sb.run(no_evidence + ["--evidence", "  "], linked)
        self.assertNotEqual(blank.returncode, 0)
        self.assertFalse((linked / ".compass" / "outcomes.json").exists())
        ok = sb.run(base_cmd, linked)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        gap = json.loads(sb.run(skill_cmd("compass gap"), linked).stdout)
        self.assertEqual(gap["last_outcome"]["verdict"], "forward")
        self.assertEqual(gap["last_outcome"]["evidence"], ["12 tests pass"])
        # and, as everywhere, the store write is refused from the primary checkout
        self.assertFalse((primary / ".compass" / "outcomes.json").exists())
        pr = sb.run(base_cmd, primary)
        self.assertNotEqual(pr.returncode, 0)
        self.assertFalse((primary / ".compass" / "outcomes.json").exists())


class TestLockRelease(unittest.TestCase):
    def test_claim_lock_release(self):
        sb, _p, linked, _ = backlog_fixture(self)
        rel = skill_cmd("backlog lock release --project", {"$PWD": str(linked)})
        self.assertEqual(rel[:3], ["backlog", "lock", "release"])
        acq = sb.run(["backlog", "lock", "acquire", "--session-id", "S1",
                      "--project", str(linked)], linked)
        self.assertEqual(acq.returncode, 0, acq.stderr)
        held = sb.run(["backlog", "lock", "status", "--project", str(linked)], linked)
        self.assertEqual(json.loads(held.stdout)["kind"], "exclusive-lock")
        # Only the holder releases (backlog 4493142a): the SKILL's command
        # identifies the caller via CLAUDE_CODE_SESSION_ID. A non-holder is
        # refused and the lock survives.
        other = sb.run(rel, linked, CLAUDE_CODE_SESSION_ID="S2")
        self.assertNotEqual(other.returncode, 0, other.stdout)
        still = sb.run(["backlog", "lock", "status", "--project", str(linked)], linked)
        self.assertEqual(json.loads(still.stdout)["kind"], "exclusive-lock")
        r = sb.run(rel, linked, CLAUDE_CODE_SESSION_ID="S1")
        self.assertEqual(r.returncode, 0, r.stderr)
        after = sb.run(["backlog", "lock", "status", "--project", str(linked)], linked)
        self.assertEqual(after.stdout.strip(), "none")
        self.assertNotIn("exclusive-lock", after.stdout)


class TestPivotCheckShape(unittest.TestCase):
    def test_claim_pivot_check_shape(self):
        sb, _p, linked = compass_fixture(self)
        m = re.search(r"compass pivot-check\s+#\s*(\{.*\})", skill_text())
        self.assertIsNotNone(m, "SKILL no longer documents the pivot-check shape")
        doc_keys = set(re.findall(r'"(\w+)":', m.group(1)))
        self.assertEqual(doc_keys, {"recommendation", "streak", "threshold", "reason"})
        self.assertIn('"persevere"|"pivot"', m.group(1))
        cmd = skill_cmd("compass pivot-check")
        self.assertEqual(cmd, ["compass", "pivot-check"])

        def check():
            r = sb.run(cmd, linked)
            self.assertEqual(r.returncode, 0, r.stderr)
            return json.loads(r.stdout)

        j = check()  # no outcomes at all
        self.assertEqual(set(j), doc_keys)
        self.assertEqual(j["recommendation"], "persevere")
        self.assertIsInstance(j["streak"], int)
        self.assertIsInstance(j["threshold"], int)
        self.assertIsInstance(j["reason"], str)
        thr = j["threshold"]
        seq = ["backward", "unchanged"] * thr
        for n, verdict in enumerate(seq[:thr], start=1):
            sb.run(["compass", "outcome", "--verdict", verdict, "--evidence", f"e{n}"], linked)
            j = check()
            self.assertEqual(j["streak"], n)
            self.assertEqual(j["recommendation"], "pivot" if n >= thr else "persevere", j)
        # a forward outcome resets the streak
        sb.run(["compass", "outcome", "--verdict", "forward", "--evidence", "ok"], linked)
        j = check()
        self.assertEqual((j["recommendation"], j["streak"]), ("persevere", 0))


def _section(text, start_pat, level_prefix):
    """Body of the section whose heading line matches `start_pat`, up to the
    next heading of the same or higher level (`level_prefix` e.g. '#### ')."""
    lines = text.splitlines()
    for i, ln in enumerate(lines):
        if re.match(start_pat, ln):
            out = []
            depth = len(level_prefix.strip())
            for nxt in lines[i + 1:]:
                m = re.match(r"^(#+) ", nxt)
                if m and len(m.group(1)) <= depth:
                    break
                out.append(nxt)
            return "\n".join(out)
    raise AssertionError(f"SKILL.md has no section matching {start_pat!r}")


def _first_bash_block(section):
    m = re.search(r"```bash\n(.*?)```", section, re.S)
    assert m, "section has no fenced bash block"
    return m.group(1)


class TestBudgetCheck(unittest.TestCase):
    """The 3-1 budget pre-check: `budgetguard check`, exit 0 only = proceed."""

    _built = False

    @classmethod
    def budgetguard(cls):
        if not cls._built:
            cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
            r = subprocess.run([cargo, "build", "-q", "-p", "budgetguard"],
                               cwd=ROOT, capture_output=True, text=True)
            assert r.returncode == 0, r.stderr
            cls._built = True
        p = target_dir() / "budgetguard"
        assert p.exists(), p
        return p

    def _cmd(self):
        block = _first_bash_block(_section(skill_text(), r"^#### 3-1\. ", "#### "))
        lines = [re.sub(r"\s{2,}#.*$", "", l).strip() for l in block.splitlines()]
        cmds = [l for l in lines if l.startswith("budgetguard check")]
        self.assertEqual(len(cmds), 1, f"3-1 bash block lacks `budgetguard check`: {block!r}")
        return cmds[0]

    def _run(self, config=None, ledger=None, with_binary=True):
        sb = Sandbox(self)
        cwd = sb.base / "proj"
        cwd.mkdir()
        if config is not None:
            (cwd / "budgetguard.toml").write_text(config)
        if ledger is not None:
            d = sb.home / ".budgetguard" / "state"
            d.mkdir(parents=True)
            (d / "ledger.json").write_text(ledger)
        if with_binary:
            (sb.extra_bin / "budgetguard").symlink_to(self.budgetguard())
        env = sb.env()
        env.pop("BUDGETGUARD_DISABLE", None)
        return subprocess.run(["sh", "-c", self._cmd()], cwd=cwd, env=env,
                              capture_output=True, text=True)

    @staticmethod
    def _today_ledger(usd):
        today = subprocess.run(["date", "+%Y-%m-%d"], capture_output=True, text=True).stdout.strip()
        return json.dumps({"days": {today: {"sessions": {"s1": usd}}}})

    CFG = "[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n"

    def _verdict(self, r):
        try:
            return json.loads(r.stdout.strip())["verdict"]
        except Exception:
            return f"<no verdict; stdout={r.stdout!r} stderr={r.stderr!r}>"

    def test_claim_budget_over_is_nonzero(self):
        r = self._run(self.CFG, self._today_ledger(10.0))
        self.assertEqual(self._verdict(r), "over", r)
        self.assertNotEqual(r.returncode, 0)

    def test_claim_budget_corrupt_ledger_is_nonzero(self):
        r = self._run(self.CFG, "{ not a ledger")
        self.assertEqual(self._verdict(r), "undetermined", r)
        self.assertNotEqual(r.returncode, 0)

    def test_claim_budget_corrupt_config_is_nonzero(self):
        r = self._run("[daily\nblock_usd = = 1\n", self._today_ledger(999.0))
        self.assertEqual(self._verdict(r), "undetermined", r)
        self.assertNotEqual(r.returncode, 0)

    def test_claim_budget_within_is_zero(self):
        r = self._run(self.CFG, self._today_ledger(1.0))
        self.assertEqual(self._verdict(r), "within", r)
        self.assertEqual(r.returncode, 0, r)

    def test_claim_budget_missing_binary_is_nonzero(self):
        # Not in PATH => the shell reports 127; "exit 0 以外 -> Step 4" must catch it.
        r = self._run(self.CFG, self._today_ledger(1.0), with_binary=False)
        self.assertNotEqual(r.returncode, 0, r)


if __name__ == "__main__":
    unittest.main()
