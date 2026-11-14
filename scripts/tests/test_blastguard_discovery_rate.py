"""Tests for scripts/blastguard-discovery-rate.py (weekly blastguard backlog
discovery-rate metric, fail-open vs false-positive).

The script is run as a real subprocess (`python3 scripts/blastguard-discovery-rate.py`)
against JSON fixtures shaped like `backlog list --json --all`.

What this proves:
  * selection (tag or title prefix, case-insensitive; others ignored),
  * classification (fp / fail_open / ambiguous / unclassified),
  * ISO-week bucketing in UTC (incl. Sunday/Monday boundary, one hard-coded label),
  * `undated` bucket for missing / non-numeric created_at (never dropped),
  * cancelled tasks excluded from weeks but counted in excluded_cancelled,
  * --weeks N keeps the last N dated weeks (undated always kept),
  * fail-closed: missing file / invalid JSON / non-array / failing `backlog`
    -> non-zero exit, non-empty stderr, no zero-count result on stdout,
  * an empty but successfully read input -> exit 0 with "weeks": [].

What this does NOT prove: the real `backlog list --json --all` schema beyond the
fields listed in the spec, the human table layout (only exit 0 and that week
labels appear), or behaviour of the live backlog store.
"""
import datetime
import json
import os
import subprocess
import sys
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(REPO, "scripts", "blastguard-discovery-rate.py")


def ts(y, mo, d, h=12, mi=0, s=0):
    return datetime.datetime(y, mo, d, h, mi, s, tzinfo=datetime.timezone.utc).timestamp()


def label(epoch):
    iso = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).isocalendar()
    return "%04d-W%02d" % (iso[0], iso[1])


_n = [0]


def task(title="blastguard thing", notes="", tags=None, status="open", created_at=None, **kw):
    _n[0] += 1
    t = {"id": "t%04d" % _n[0], "title": title, "notes": notes,
         "tags": ["blastguard"] if tags is None else tags, "status": status}
    if created_at is not None:
        t["created_at"] = created_at
    t.update(kw)
    return t


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def write(self, data, name="in.json", raw=False):
        p = os.path.join(self.tmp.name, name)
        with open(p, "w") as f:
            f.write(data if raw else json.dumps(data))
        return p

    def run_script(self, *args, env=None):
        return subprocess.run([sys.executable, SCRIPT, *args], capture_output=True,
                              text=True, env=env, timeout=60)

    def run_json(self, tasks, *extra):
        r = self.run_script("--input", self.write(tasks), "--json", *extra)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)

    def one(self, t):
        t.setdefault("created_at", ts(2026, 11, 10))
        out = self.run_json([t])
        self.assertEqual(len(out["weeks"]), 1, out)
        return out["weeks"][0]


class TestClassification(Base):
    def test_fp_by_tag_false_positive(self):
        w = self.one(task(tags=["blastguard", "false-positive"], title="x"))
        self.assertEqual((w["fp"], w["fail_open"], w["ambiguous"], w["unclassified"], w["total"]), (1, 0, 0, 0, 1))

    def test_fp_by_tag_fp(self):
        self.assertEqual(self.one(task(tags=["blastguard", "fp"], title="x"))["fp"], 1)

    def test_fp_by_title_regexes(self):
        for title in ("blastguard FP on cat", "blastguard false positive on ls",
                      "blastguard false-positive on ls", "blastguard 誤検知 rm"):
            with self.subTest(title=title):
                self.assertEqual(self.one(task(title=title))["fp"], 1)

    def test_fp_word_boundary(self):
        # "fpga" must not match \bFP\b
        self.assertEqual(self.one(task(title="blastguard fpga thing"))["unclassified"], 1)

    def test_fail_open_by_tag(self):
        self.assertEqual(self.one(task(tags=["blastguard", "fail-open"], title="x"))["fail_open"], 1)

    def test_fail_open_by_title(self):
        self.assertEqual(self.one(task(title="blastguard Fail-Open via env"))["fail_open"], 1)
        self.assertEqual(self.one(task(title="blastguard fail open via env"))["fail_open"], 1)

    def test_fail_open_by_notes(self):
        self.assertEqual(self.one(task(title="blastguard x", notes="this is a FAIL-OPEN"))["fail_open"], 1)

    def test_fp_in_notes_alone_does_not_match(self):
        # spec: FP-match looks at tags and title only
        self.assertEqual(self.one(task(title="blastguard x", notes="a false positive"))["unclassified"], 1)

    def test_ambiguous(self):
        w = self.one(task(title="blastguard FP and fail-open"))
        self.assertEqual((w["ambiguous"], w["fp"], w["fail_open"], w["total"]), (1, 0, 0, 1))

    def test_ambiguous_mixed_tag_and_notes(self):
        w = self.one(task(tags=["blastguard", "fp"], title="x", notes="fail open here"))
        self.assertEqual(w["ambiguous"], 1)

    def test_unclassified(self):
        w = self.one(task(title="blastguard docs cleanup"))
        self.assertEqual((w["unclassified"], w["total"]), (1, 1))


class TestSelection(Base):
    def test_tag_only_match(self):
        w = self.one(task(title="something else", tags=["blastguard"]))
        self.assertEqual(w["total"], 1)

    def test_title_prefix_only_match_case_insensitive(self):
        w = self.one(task(title="BlastGuard: odd thing", tags=[]))
        self.assertEqual(w["total"], 1)

    def test_title_not_prefix_ignored(self):
        out = self.run_json([task(title="about blastguard", tags=["other"], created_at=ts(2026, 11, 10))])
        self.assertEqual(out["weeks"], [])

    def test_non_blastguard_ignored(self):
        out = self.run_json([task(title="condukt FP", tags=["condukt", "fp"], created_at=ts(2026, 11, 10)),
                             task(created_at=ts(2026, 11, 10))])
        self.assertEqual(out["weeks"][0]["total"], 1)

    def test_cancelled_excluded_and_counted(self):
        out = self.run_json([task(created_at=ts(2026, 11, 10)),
                             task(status="cancelled", created_at=ts(2026, 11, 10)),
                             task(status="cancelled", created_at=ts(2026, 10, 1)),
                             task(status="cancelled", title="other", tags=["x"], created_at=ts(2026, 10, 1))])
        self.assertEqual(out["excluded_cancelled"], 2)
        self.assertEqual(len(out["weeks"]), 1)
        self.assertEqual(out["weeks"][0]["total"], 1)

    def test_empty_result_is_exit_zero(self):
        out = self.run_json([])
        self.assertEqual(out["weeks"], [])
        self.assertEqual(out["excluded_cancelled"], 0)

    def test_only_non_blastguard_is_empty_weeks(self):
        out = self.run_json([task(title="x", tags=["y"], created_at=ts(2026, 11, 10))])
        self.assertEqual(out["weeks"], [])


class TestWeeks(Base):
    def test_hardcoded_label(self):
        # 2026-11-10 is a Tuesday in ISO week 46 of 2026
        w = self.one(task(created_at=ts(2026, 11, 10)))
        self.assertEqual(w["week"], "2026-W46")

    def test_boundary_sunday_vs_monday(self):
        sun = ts(2026, 11, 8, 23, 59, 59)
        mon = ts(2026, 11, 9, 0, 0, 0)
        out = self.run_json([task(created_at=sun), task(created_at=mon)])
        labels = [w["week"] for w in out["weeks"]]
        self.assertEqual(labels, [label(sun), label(mon)])
        self.assertNotEqual(labels[0], labels[1])
        self.assertEqual(labels, ["2026-W45", "2026-W46"])
        self.assertEqual([w["total"] for w in out["weeks"]], [1, 1])

    def test_same_week_aggregates(self):
        out = self.run_json([task(created_at=ts(2026, 11, 9)), task(created_at=ts(2026, 11, 15)),
                             task(title="blastguard FP a", created_at=ts(2026, 11, 11))])
        self.assertEqual(len(out["weeks"]), 1)
        w = out["weeks"][0]
        self.assertEqual((w["total"], w["fp"], w["unclassified"]), (3, 1, 2))

    def test_year_boundary_iso_year(self):
        e = ts(2026, 12, 31)  # ISO year 2026 W53
        self.assertEqual(self.one(task(created_at=e))["week"], label(e))

    def test_sorted_ascending(self):
        out = self.run_json([task(created_at=ts(2026, 11, 10)), task(created_at=ts(2026, 9, 1)),
                             task(created_at=ts(2026, 10, 6))])
        labels = [w["week"] for w in out["weeks"]]
        self.assertEqual(labels, sorted(labels))
        self.assertEqual(labels, ["2026-W%02d" % n for n in range(36, 47)])
        for w in out["weeks"]:
            want = 1 if w["week"] in ("2026-W36", "2026-W41", "2026-W46") else 0
            self.assertEqual(w["total"], want, w)
            if want == 0:
                self.assertEqual((w["fail_open"], w["fp"], w["ambiguous"], w["unclassified"]),
                                 (0, 0, 0, 0), w)

    def test_undated_missing_and_nonnumeric(self):
        t1 = task()  # missing created_at
        t2 = task(created_at="yesterday")
        t3 = task(created_at=None)
        t3["created_at"] = None
        out = self.run_json([t1, t2, t3, task(created_at=ts(2026, 11, 10))])
        labels = [w["week"] for w in out["weeks"]]
        self.assertEqual(labels, ["2026-W46", "undated"])
        self.assertEqual(out["weeks"][-1]["total"], 3)

    def test_total_is_sum_of_four(self):
        e = ts(2026, 11, 10)
        out = self.run_json([task(title="blastguard FP", created_at=e),
                             task(title="blastguard fail-open", created_at=e),
                             task(title="blastguard FP fail-open", created_at=e),
                             task(title="blastguard other", created_at=e)])
        w = out["weeks"][0]
        self.assertEqual((w["fp"], w["fail_open"], w["ambiguous"], w["unclassified"], w["total"]), (1, 1, 1, 1, 4))

    def test_weeks_limit_keeps_last_n_and_undated(self):
        ts_list = [ts(2026, 10, 5), ts(2026, 10, 12), ts(2026, 10, 19), ts(2026, 10, 26)]
        tasks = [task(created_at=e) for e in ts_list] + [task()]
        out = self.run_json(tasks, "--weeks", "2")
        self.assertEqual([w["week"] for w in out["weeks"]],
                         [label(ts_list[2]), label(ts_list[3]), "undated"])

    def test_weeks_limit_larger_than_available(self):
        out = self.run_json([task(created_at=ts(2026, 11, 10))], "--weeks", "10")
        self.assertEqual(len(out["weeks"]), 1)

    def test_human_table(self):
        p = self.write([task(created_at=ts(2026, 11, 10)), task()])
        r = self.run_script("--input", p)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("2026-W46", r.stdout)
        self.assertIn("undated", r.stdout)


def iso_labels(start, end):
    """ISO week labels from the week of `start` to the week of `end` inclusive,
    computed by stepping 7 days from the Monday of the start week."""
    d = start - datetime.timedelta(days=start.weekday())
    out = []
    while d <= end:
        iso = d.isocalendar()
        out.append("%04d-W%02d" % (iso[0], iso[1]))
        d += datetime.timedelta(days=7)
    return out


class TestZeroWeeks(Base):
    def zero(self, w):
        return (w["fail_open"], w["fp"], w["ambiguous"], w["unclassified"], w["total"])

    def test_gap_weeks_are_rows(self):
        out = self.run_json([task(created_at=ts(2026, 10, 6)), task(created_at=ts(2026, 10, 27))])
        self.assertEqual([w["week"] for w in out["weeks"]],
                         ["2026-W41", "2026-W42", "2026-W43", "2026-W44"])
        self.assertEqual([w["total"] for w in out["weeks"]], [1, 0, 0, 1])
        self.assertEqual(self.zero(out["weeks"][1]), (0, 0, 0, 0, 0))
        self.assertEqual(self.zero(out["weeks"][2]), (0, 0, 0, 0, 0))

    def test_iso_year_boundary_range(self):
        # 2025 has 52 ISO weeks; 2025-12-22 is 2025-W52, 2026-01-12 is 2026-W03.
        a, b = ts(2025, 12, 24), ts(2026, 1, 14)
        out = self.run_json([task(created_at=a), task(created_at=b)])
        labels = [w["week"] for w in out["weeks"]]
        self.assertEqual(labels, ["2025-W52", "2026-W01", "2026-W02", "2026-W03"])
        self.assertEqual(labels, iso_labels(datetime.date(2025, 12, 24), datetime.date(2026, 1, 14)))
        self.assertEqual([w["total"] for w in out["weeks"]], [1, 0, 0, 1])

    def test_iso_year_boundary_with_week_53(self):
        # 2026 has 53 ISO weeks (2026-12-31 is W53); next is 2027-W01.
        out = self.run_json([task(created_at=ts(2026, 12, 16)), task(created_at=ts(2027, 1, 13))])
        labels = [w["week"] for w in out["weeks"]]
        self.assertEqual(labels, iso_labels(datetime.date(2026, 12, 16), datetime.date(2027, 1, 13)))
        self.assertIn("2026-W53", labels)
        self.assertEqual(labels[0], "2026-W51")
        self.assertEqual(labels[-1], "2027-W02")
        self.assertEqual(sum(w["total"] for w in out["weeks"]), 2)

    def test_cancelled_does_not_extend_range(self):
        out = self.run_json([task(created_at=ts(2026, 10, 6)), task(created_at=ts(2026, 10, 13)),
                             task(status="cancelled", created_at=ts(2026, 1, 6)),
                             task(status="cancelled", created_at=ts(2026, 12, 1))])
        self.assertEqual([w["week"] for w in out["weeks"]], ["2026-W41", "2026-W42"])
        self.assertEqual(out["excluded_cancelled"], 2)

    def test_non_blastguard_does_not_extend_range(self):
        out = self.run_json([task(created_at=ts(2026, 10, 6)),
                             task(title="other", tags=["x"], created_at=ts(2026, 1, 6))])
        self.assertEqual([w["week"] for w in out["weeks"]], ["2026-W41"])

    def test_weeks_limit_counts_calendar_weeks(self):
        # data in W36 and W46 only; --weeks 3 = W44,W45,W46 (zero rows included)
        out = self.run_json([task(created_at=ts(2026, 9, 1)), task(created_at=ts(2026, 11, 10))],
                            "--weeks", "3")
        self.assertEqual([w["week"] for w in out["weeks"]], ["2026-W44", "2026-W45", "2026-W46"])
        self.assertEqual([w["total"] for w in out["weeks"]], [0, 0, 1])

    def test_weeks_limit_zero_rows_only_window_and_undated_last(self):
        out = self.run_json([task(created_at=ts(2026, 9, 1)), task(created_at=ts(2026, 11, 10)),
                             task()], "--weeks", "2")
        self.assertEqual([w["week"] for w in out["weeks"]], ["2026-W45", "2026-W46", "undated"])
        self.assertEqual([w["total"] for w in out["weeks"]], [0, 1, 1])

    def test_weeks_limit_exceeding_range_returns_whole_range(self):
        out = self.run_json([task(created_at=ts(2026, 10, 6)), task(created_at=ts(2026, 10, 20))],
                            "--weeks", "8")
        self.assertEqual([w["week"] for w in out["weeks"]], ["2026-W41", "2026-W42", "2026-W43"])

    def test_undated_alone_has_no_zero_weeks(self):
        out = self.run_json([task()])
        self.assertEqual([w["week"] for w in out["weeks"]], ["undated"])

    def test_human_table_shows_zero_rows(self):
        p = self.write([task(created_at=ts(2026, 10, 6)), task(created_at=ts(2026, 10, 20))])
        r = self.run_script("--input", p)
        self.assertEqual(r.returncode, 0, r.stderr)
        for lbl in ("2026-W41", "2026-W42", "2026-W43"):
            self.assertIn(lbl, r.stdout)
        row = [l for l in r.stdout.splitlines() if l.startswith("2026-W42")]
        self.assertEqual(len(row), 1, r.stdout)
        self.assertEqual(row[0].split()[1:], ["0", "0", "0", "0", "0"])


class TestFailClosed(Base):
    def assert_closed(self, r):
        self.assertNotEqual(r.returncode, 0)
        self.assertTrue(r.stderr.strip(), "stderr must explain the failure")
        self.assertNotIn('"weeks"', r.stdout)
        self.assertNotIn("excluded_cancelled", r.stdout)

    def test_missing_file(self):
        r = self.run_script("--input", os.path.join(self.tmp.name, "nope.json"), "--json")
        self.assert_closed(r)
        self.assertEqual(r.returncode, 2)

    def test_invalid_json(self):
        r = self.run_script("--input", self.write("{not json", raw=True), "--json")
        self.assert_closed(r)
        self.assertEqual(r.returncode, 2)

    def test_empty_file(self):
        r = self.run_script("--input", self.write("", raw=True), "--json")
        self.assert_closed(r)

    def test_not_array(self):
        r = self.run_script("--input", self.write({"tasks": []}), "--json")
        self.assert_closed(r)
        self.assertEqual(r.returncode, 2)

    def test_not_array_human_mode(self):
        r = self.run_script("--input", self.write("null", raw=True))
        self.assertNotEqual(r.returncode, 0)
        self.assertTrue(r.stderr.strip())

    def test_backlog_command_fails(self):
        d = os.path.join(self.tmp.name, "bin")
        os.mkdir(d)
        fake = os.path.join(d, "backlog")
        with open(fake, "w") as f:
            f.write("#!/bin/sh\necho boom >&2\nexit 1\n")
        os.chmod(fake, 0o755)
        env = dict(os.environ, PATH=d)
        r = subprocess.run([sys.executable, SCRIPT, "--json"], capture_output=True,
                           text=True, env=env, timeout=60)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn('"weeks"', r.stdout)

    def test_backlog_command_missing(self):
        d = os.path.join(self.tmp.name, "empty")
        os.mkdir(d)
        env = dict(os.environ, PATH=d)
        r = subprocess.run([sys.executable, SCRIPT, "--json"], capture_output=True,
                           text=True, env=env, timeout=60)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn('"weeks"', r.stdout)


if __name__ == "__main__":
    unittest.main()
