"""Claim-reproduction tests for crates/daily-report (skill-only plugin).

Every test pins what is OBSERVED today about a prose claim in
crates/daily-report/{README*.md,skills/daily-report/SKILL.md}. Commands are
extracted from SKILL.md fenced blocks so a prose change breaks the test.
Run: python3 -m pytest scripts/test_prose_daily_report.py -q
"""
import datetime
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "crates" / "daily-report"
SKILL = Path(os.environ.get("DAILY_REPORT_SKILL_MD", CRATE / "skills" / "daily-report" / "SKILL.md"))
README = Path(os.environ.get("DAILY_REPORT_README_MD", CRATE / "README.md"))
README_JA = Path(os.environ.get("DAILY_REPORT_README_JA_MD", CRATE / "README.ja.md"))


def _cargo():
    c = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    return c


_BIN = None


def si_bin():
    """Build session-insights once; return the binary path."""
    global _BIN
    if _BIN is None:
        r = subprocess.run([_cargo(), "build", "-q", "-p", "session-insights"],
                           cwd=ROOT, capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        _BIN = ROOT / "target" / "debug" / "session-insights"
        assert _BIN.exists()
    return _BIN


def skill_text():
    return SKILL.read_text(encoding="utf-8")


def frontmatter():
    m = re.match(r"---\n(.*?)\n---\n", skill_text(), re.S)
    assert m, "SKILL.md has no YAML front matter"
    fm = {}
    for line in m.group(1).splitlines():
        k, _, v = line.partition(":")
        fm[k.strip()] = v.strip().strip('"')
    return fm


def allowed_tools():
    return [t.strip() for t in frontmatter()["allowed-tools"].split(",")]


def bash_block_after(marker):
    """First ```bash fenced block following the line containing `marker`."""
    text = skill_text()
    i = text.index(marker)
    m = re.search(r"```bash\n(.*?)```", text[i:], re.S)
    assert m, f"no bash block after {marker!r}"
    return m.group(1)


def si_status(home, cwd):
    env = {"PATH": os.environ["PATH"], "HOME": str(home)}
    r = subprocess.run([str(si_bin()), "status"], cwd=cwd, env=env,
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    out = {}
    for line in r.stdout.splitlines():
        k, _, v = line.partition(":")
        out[k.strip()] = v.strip()
    return out


def git_env(date=None):
    e = {"PATH": os.environ["PATH"], "HOME": "/nonexistent-home", "TZ": "UTC",
         "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null",
         "GIT_CONFIG_NOSYSTEM": "1"}
    if date:
        e["GIT_AUTHOR_DATE"] = date
        e["GIT_COMMITTER_DATE"] = date
    return e


def git(repo, *args, date=None, author=("Alice", "alice@example.com")):
    e = git_env(date)
    e.update(GIT_AUTHOR_NAME=author[0], GIT_AUTHOR_EMAIL=author[1],
             GIT_COMMITTER_NAME=author[0], GIT_COMMITTER_EMAIL=author[1])
    r = subprocess.run(["git", *args], cwd=repo, env=e, capture_output=True, text=True)
    assert r.returncode == 0, (args, r.stderr)
    return r.stdout


def commit(repo, fname, subject, date, author=("Alice", "alice@example.com"), body="x\n"):
    p = Path(repo) / fname
    p.write_text(body)
    git(repo, "add", fname)
    git(repo, "commit", "-q", "-m", subject, date=date, author=author)


def run_sh(cmd, cwd):
    r = subprocess.run(["sh", "-c", cmd], cwd=cwd, env=git_env(), capture_output=True, text=True)
    return r


class T(unittest.TestCase):
    # ---------------------------------------------------------------- layout
    def test_claim_skill_only_layout(self):
        tracked = subprocess.run(["git", "ls-files", "crates/daily-report"], cwd=ROOT,
                                 capture_output=True, text=True, env=git_env()).stdout.split()
        self.assertTrue(tracked)
        for f in tracked:
            rel = f[len("crates/daily-report/"):]
            for forbidden in ("Cargo.toml", "src/", "bin/", "hooks/"):
                self.assertFalse(rel.startswith(forbidden), f"{f} contradicts skill-only")
        self.assertIn("crates/daily-report/skills/daily-report/SKILL.md", tracked)
        for d in ("src", "bin", "hooks"):
            self.assertFalse((CRATE / d).exists(), d)
        self.assertFalse((CRATE / "Cargo.toml").exists())
        plugin = json.loads((CRATE / ".claude-plugin" / "plugin.json").read_text())
        for k in ("hooks", "mcpServers", "commands", "mcp_servers"):
            self.assertNotIn(k, plugin)
        # "no API key": the only tools granted are local ones, no network tool.
        tools = allowed_tools()
        self.assertEqual(
            sorted(tools),
            sorted(["Bash(git:*)", "Bash(ls:*)", "Bash(cat:*)", "Bash(mkdir:*)",
                    "Bash(date:*)", "Read", "Grep", "Glob", "Write"]))
        for t in tools:
            for net in ("WebFetch", "WebSearch", "curl", "wget", "mcp__"):
                self.assertNotIn(net, t)

    # ------------------------------------------------------------------ date
    def test_claim_date_today_yesterday(self):
        # prose (SKILL.md) names `date +%F`, BSD `date -v-1d +%F` and GNU `date -d yesterday +%F`
        self.assertIn("`date +%F`", skill_text())
        self.assertIn("`date -d yesterday +%F`", skill_text())
        self.assertIn("`date -v-1d +%F`", skill_text())
        self.assertIn("Bash(date:*)", allowed_tools())
        today = datetime.date.today()
        yday = today - datetime.timedelta(days=1)

        def d(*a):
            return subprocess.run(["date", *a], capture_output=True, text=True)

        r = d("+%F")
        self.assertEqual(r.returncode, 0)
        self.assertIn(r.stdout.strip(), {today.isoformat(),
                                         (today + datetime.timedelta(days=1)).isoformat(),
                                         yday.isoformat()})  # tolerate midnight rollover
        gnu = d("--version").returncode == 0
        dy = d("-d", "yesterday", "+%F")
        if gnu:
            self.assertEqual(dy.returncode, 0)
            self.assertRegex(dy.stdout.strip(), r"^\d{4}-\d{2}-\d{2}$")
        else:
            # OBSERVED on BSD/macOS date: the prose's GNU-only form is rejected...
            self.assertNotEqual(dy.returncode, 0)
            self.assertEqual(dy.stdout.strip(), "")
            # ...and the BSD form that does work:
            bsd = d("-v-1d", "+%F")
            self.assertEqual(bsd.returncode, 0)
            self.assertRegex(bsd.stdout.strip(), r"^\d{4}-\d{2}-\d{2}$")
            self.assertNotEqual(bsd.stdout.strip(), d("+%F").stdout.strip())
            self.assertIn(bsd.stdout.strip(), {yday.isoformat(), today.isoformat()})

    # -------------------------------------------------------------- argument
    def test_claim_arg_since(self):
        self.assertIn("--since <git-expr>", frontmatter()["argument-hint"])
        # the period-mode command actually uses the argument
        self.assertIn('--since="<git-expr>"', bash_block_after("期間モードなら"))

    def test_claim_arg_date(self):
        self.assertIn("YYYY-MM-DD | today | yesterday", frontmatter()["argument-hint"])

    def test_claim_arg_repo(self):
        self.assertIn("--repo <path>", frontmatter()["argument-hint"])

    def test_claim_arg_stdout(self):
        self.assertIn("--stdout", frontmatter()["argument-hint"])

    # ---------------------------------------------------------------- config
    def test_claim_config_home_keys(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            home, cwd = t / "h", t / "c"
            (home / ".session-insights").mkdir(parents=True)
            cwd.mkdir()
            (home / ".session-insights" / "config.toml").write_text(
                'obsidian_vault = "/some/abs"\nrecord_dir = "rx"\n')
            s = si_status(home, cwd)
            self.assertEqual(s["obsidian_vault"], "/some/abs")
            self.assertEqual(s["record_dir"], "rx")
            self.assertEqual(s["config"], str(home / ".session-insights" / "config.toml"))
            # PRECEDENCE (observed): a project-level session-insights.toml in the
            # cwd WINS over ~/.session-insights/config.toml (not merged).
            (cwd / "session-insights.toml").write_text(
                'obsidian_vault = "/proj/vault"\nrecord_dir = "pr"\n')
            s = si_status(home, cwd)
            self.assertEqual(s["obsidian_vault"], "/proj/vault")
            self.assertEqual(s["record_dir"], "pr")
            self.assertEqual(os.path.realpath(s["config"]), os.path.realpath(cwd / "session-insights.toml"))
            # and the project file does not inherit unset keys from home:
            (cwd / "session-insights.toml").write_text('record_dir = "pr"\n')
            s = si_status(home, cwd)
            self.assertEqual(s["record_dir"], "pr")
            self.assertEqual(s["obsidian_vault"], str(home / "Documents/vault/yukineko"))

    def test_claim_config_tilde(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            home, cwd = t / "h", t / "c"
            (home / ".session-insights").mkdir(parents=True)
            cwd.mkdir()
            (home / ".session-insights" / "config.toml").write_text('obsidian_vault = "~/v"\n')
            self.assertEqual(si_status(home, cwd)["obsidian_vault"], str(home / "v"))

    def test_claim_config_defaults(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            home, cwd = t / "h", t / "c"
            home.mkdir()
            cwd.mkdir()
            s = si_status(home, cwd)
            self.assertEqual(s["config"], "(defaults — no config file)")
            self.assertEqual(s["obsidian_vault"], str(home / "Documents/vault/yukineko"))
            self.assertEqual(s["record_dir"], "records")

    # ------------------------------------------------------------------- git
    def test_claim_git_log_single_day_all_authors(self):
        cmd = bash_block_after("単日なら")
        self.assertNotIn("--author", cmd)  # observed: no author filter
        with tempfile.TemporaryDirectory() as t:
            repo = Path(t)
            git(repo, "init", "-q", "-b", "main")
            A, B = ("Alice", "alice@example.com"), ("Bob", "bob@example.com")
            commit(repo, "a", "prev day", "2026-03-09T12:00:00+0000", A)
            commit(repo, "b", "alice target", "2026-03-10T09:05:00+0000", A)
            commit(repo, "c", "bob target", "2026-03-10T23:59:30+0000", B)
            git(repo, "checkout", "-q", "-b", "side")
            commit(repo, "d", "side work", "2026-03-10T10:00:00+0000", A)
            git(repo, "checkout", "-q", "main")
            git(repo, "merge", "--no-ff", "-q", "-m", "merge side", "side",
                date="2026-03-10T11:00:00+0000")
            commit(repo, "e", "next day", "2026-03-11T00:00:00+0000", A)
            r = run_sh(cmd.replace("<date>", "2026-03-10"), repo)
            self.assertEqual(r.returncode, 0, r.stderr)
            rows = [ln.split("\t") for ln in r.stdout.splitlines()]
            got = sorted((x[1], x[2], x[3]) for x in rows)
            # OBSERVED: both authors' non-merge target-day commits; merge, prev
            # and next day excluded; format %h\t%an\t%H:%M\t%s.
            self.assertEqual(got, sorted([("Alice", "09:05", "alice target"),
                                          ("Bob", "23:59", "bob target"),
                                          ("Alice", "10:00", "side work")]))
            for x in rows:
                self.assertEqual(len(x), 4)
                self.assertRegex(x[0], r"^[0-9a-f]{7,}$")

    def test_claim_git_log_period(self):
        cmd = bash_block_after("期間モードなら")
        with tempfile.TemporaryDirectory() as t:
            repo = Path(t)
            git(repo, "init", "-q", "-b", "main")
            commit(repo, "a", "old", "2026-03-01T12:00:00+0000")
            commit(repo, "b", "new1", "2026-03-10T09:05:00+0000")
            git(repo, "checkout", "-q", "-b", "side")
            commit(repo, "d", "side", "2026-03-11T10:00:00+0000")
            git(repo, "checkout", "-q", "main")
            git(repo, "merge", "--no-ff", "-q", "-m", "merge side", "side",
                date="2026-03-12T11:00:00+0000")
            commit(repo, "e", "new2", "2026-03-13T00:00:00+0000", ("Bob", "bob@example.com"))
            r = run_sh(cmd.replace("<git-expr>", "2026-03-05"), repo)
            self.assertEqual(r.returncode, 0, r.stderr)
            rows = [ln.split("\t") for ln in r.stdout.splitlines()]
            self.assertEqual(sorted((x[1], x[2], x[3]) for x in rows),
                             sorted([("Alice", "2026-03-10", "new1"),
                                     ("Alice", "2026-03-11", "side"),
                                     ("Bob", "2026-03-13", "new2")]))

    def test_claim_git_shortstat(self):
        text = skill_text()
        self.assertIn("`git log --shortstat`", text)
        self.assertIn("`git show --stat <sha>`", text)
        self.assertIn("Bash(git:*)", allowed_tools())
        with tempfile.TemporaryDirectory() as t:
            repo = Path(t)
            git(repo, "init", "-q", "-b", "main")
            commit(repo, "a", "one", "2026-03-10T09:00:00+0000", body="1\n2\n3\n")
            sha = git(repo, "rev-parse", "HEAD").strip()
            for args in (["log", "--shortstat"], ["show", "--stat", sha]):
                out = git(repo, *args)
                self.assertRegex(out, r"1 file changed, 3 insertions\(\+\)")

    # --------------------------------------------------------------- records
    def test_claim_record_list_grep(self):
        cmd = bash_block_after("対象日の record ノート")
        self.assertEqual(cmd.strip(), 'ls "<vault>/<record_dir>/" | grep "^<date>-"')
        with tempfile.TemporaryDirectory() as t:
            rd = Path(t) / "vault" / "records"
            rd.mkdir(parents=True)
            names = ["2026-03-10-projA-abcd1234.md", "2026-03-10-projB-ffff0000.md",
                     "2026-03-09-projA-abcd1234.md", "2026-03-11-projA-abcd1234.md",
                     "x2026-03-10-projA-abcd1234.md"]
            for n in names:
                (rd / n).write_text("x")
            c = cmd.replace("<vault>", str(Path(t) / "vault")).replace(
                "<record_dir>", "records").replace("<date>", "2026-03-10")
            r = run_sh(c, t)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.split(), names[:2])

    def test_claim_write_only_daily(self):
        # SKILL.md hard rule: "<vault>/daily/<date>.md 以外には書かないこと".
        # What the DEFINITION can show: no write-capable instruction targets
        # anything but <vault>/daily/, and records appear only in reads.
        text = skill_text()
        body = re.sub(r"\A---\n.*?\n---\n", "", text, count=1, flags=re.S)
        tools = allowed_tools()
        for t in ("Edit", "MultiEdit", "NotebookEdit"):
            self.assertNotIn(t, tools)
        # fenced bash blocks: read-only commands only, no redirect/tee/mkdir
        blocks = re.findall(r"```bash\n(.*?)```", body, re.S)
        self.assertGreaterEqual(len(blocks), 3)
        for b in blocks:
            b = b.replace("\\\n", " ")
            # drop <placeholder> tokens so only real redirects remain
            self.assertNotIn(">", re.sub(r"<[A-Za-z_-]+>", "", b))
            self.assertIsNone(re.search(r"\btee\b|\bmkdir\b|\bsed\s+-i|\bcp\b|\bmv\b|\brm\b", b), b)
            for cmd in re.split(r"\n|\|", b):
                w = cmd.split()
                if w:
                    self.assertIn(w[0], {"git", "ls", "grep", "cat", "date"}, cmd)
        # every prose line naming a write operation (mkdir / Write tool) has
        # path targets that are all under <vault>/daily/, and there is >= 1.
        prose = re.sub(r"```.*?```", "", body, flags=re.S)
        write_re = re.compile(r"mkdir|\bWrite\b|\bEdit\b|追記|上書き|書き込")
        targets = []
        for line in prose.splitlines():
            if not write_re.search(line):
                continue
            for span in re.findall(r"`([^`]+)`", line):
                if "/" in span:
                    targets.append(span)
                    self.assertTrue(span.startswith("<vault>/daily/"), (span, line))
        self.assertIn("<vault>/daily/<date>.md", targets)
        self.assertIn("<vault>/daily/", targets)
        # the records dir never co-occurs with a write verb, unless the line
        # is itself a prohibition
        prohib = re.compile(r"ないこと|禁止|Never|never")
        verbs = re.compile(r"mkdir|\bWrite\b|\bEdit\b|追記|上書き|書き込|書く|書き換え|保存|tee")
        for line in prose.splitlines():
            if re.search(r"record_dir|records|record|レコード", line) and verbs.search(line):
                self.assertIsNotNone(prohib.search(line), line)
        # the prohibition itself is present in the skill and both READMEs
        self.assertIn("`<vault>/daily/<date>.md` 以外には書かないこと", body)
        self.assertIn("`records/` には決して書かないこと", body)
        self.assertIn("Never write anywhere except `<vault>/daily/<date>.md`; "
                      "never write into `records/`.", README.read_text(encoding="utf-8"))
        ja = README_JA.read_text(encoding="utf-8")
        self.assertIn("`<vault>/daily/<date>.md` 以外には書かないこと。", ja)
        self.assertIn("`records/` には決して書き込まないこと。", ja)

    def test_claim_mkdir_write_allowed(self):
        tools = allowed_tools()
        self.assertIn("Bash(mkdir:*)", tools)
        self.assertIn("Write", tools)
        self.assertIn("`mkdir -p`", skill_text())


if __name__ == "__main__":
    unittest.main()
