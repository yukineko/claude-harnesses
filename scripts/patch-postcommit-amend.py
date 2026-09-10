"""ONE-SHOT HANDOFF (backlog 6267bfbe): apply the amend carve-out to
.githooks/post-commit. Delete this file once it has been applied.

It lives in scripts/ rather than a scratch directory ON PURPOSE. The scratch
directory is shared between concurrent sessions and a peer session was observed
overwriting another session's file mid-task (backlog eb1f8021), so a patch that
only exists there is a patch that can silently vanish or be replaced.

To apply, from a worktree and never the main tree (CLAUDE.md 8):

    python3 scripts/patch-postcommit-amend.py
    cp .scratch/candidate-hooks/post-commit .githooks/post-commit
    sh -n .githooks/post-commit
    python3 scripts/test_gate_bypass.py     # expect: Ran 124 tests ... OK

Measured against the candidate this generates, 2026-09-11 at 6374a143:
"Ran 124 tests in 68.375s / OK" -- including the two tests that each kill a
one-sided version of the carve-out. Against the UNPATCHED hook the same suite
reports failures=1 (AmendIsNotABypass), which is the defect this closes.

Build a CANDIDATE .githooks/post-commit that stops recording `git commit
--amend` as an ungated commit (backlog 6267bfbe).

Writes to .scratch/candidate-hooks/post-commit ONLY. The real
.githooks/post-commit is deny-listed for this session and is not touched: this
exists so the candidate can be VERIFIED against the real test harness before a
human is asked to apply it (CLAUDE.md 5 -- hand the judgement back, but hand it
back measured rather than as a guess).
"""

import pathlib

SRC = pathlib.Path(".githooks/post-commit")
DST = pathlib.Path(".scratch/candidate-hooks/post-commit")

OLD = """    else
        for p in $parents; do
            [ "$p" = "$certified_head" ] && exit 0
        done
    fi
fi
"""

NEW = """    else
        for p in $parents; do
            [ "$p" = "$certified_head" ] && exit 0
        done
        # `git commit --amend` REPLACES the commit pre-commit judged, so the
        # certified HEAD (C1) is not among the new commit's parents -- its parent
        # is C1's parent. The gate really did run on this content and really did
        # go green, so recording it as ungated states the opposite of what was
        # observed. Amend is routine, and a ledger that fires on routine work
        # stops meaning "someone ran --no-verify".
        #
        # Read from the reflog, which separates the two deterministically
        # (measured 2026-09-11 in a throwaway repo):
        #   after an amend  reflog subject -> commit (amend): first
        #                   rev-parse HEAD@{1} -> C1, equal to certified_head
        #   after a commit  reflog subject -> commit: second
        #
        # BOTH conditions are required, and each half is pinned by its own test
        # in scripts/test_gate_bypass.py::CertificateBindsTheDiffNotTheOperation.
        # Neither was pinned by anything when this carve-out was first written:
        # the whole suite stayed green under either half alone, so "both are
        # required" was an argument until those two tests existed.
        #
        #   the marker alone is defeated by an ordinary wrong-base amend -- the
        #   certificate was earned against some OTHER head, so the diff that was
        #   inspected is not the diff that landed
        #   (test_amend_certified_against_another_head_is_recorded).
        #
        #   HEAD@{1} alone is subtler. For any non-amend commit with an INTACT
        #   reflog, HEAD@{1} is the commit's own first parent, so the parent loop
        #   above would already have accepted it and this branch is unreachable.
        #   Its independent value rests entirely on reflog GAPS -- and a ref
        #   update issued from a parallel worktree moves this worktree's HEAD
        #   without writing an entry here, which CLAUDE.md 8 makes routine rather
        #   than exotic
        #   (test_non_amend_commit_one_reflog_step_from_the_certified_head_is_recorded).
        #
        # This does NOT weaken test_certificate_does_not_survive_head_moving:
        # that case moves HEAD with update-ref, whose reflog subject is not an
        # amend, so it still lands in the ledger.
        reflog_subject="$(git reflog -1 --format=%gs HEAD 2>/dev/null)"
        case "$reflog_subject" in
            'commit (amend):'*)
                prev="$(git rev-parse 'HEAD@{1}' 2>/dev/null)" || prev=''
                if [ -n "$prev" ] && [ "$prev" = "$certified_head" ]; then
                    exit 0
                fi
                ;;
        esac
    fi
fi
"""


def main():
    s = SRC.read_text()
    n = s.count(OLD)
    if n != 1:
        raise SystemExit("FATAL: anchor found %d times, expected 1" % n)
    DST.parent.mkdir(parents=True, exist_ok=True)

    # Mirror every OTHER hook verbatim into the candidate dir. The test harness
    # copies a whole hooks directory into its throwaway repo, so a candidate dir
    # holding only post-commit would silently test a repo with no pre-commit --
    # and then the amend would legitimately be ungated, which would "pass" for
    # entirely the wrong reason. Verbatim mirror, one patched file.
    for src in sorted(SRC.parent.iterdir()):
        if not src.is_file() or src.name == SRC.name:
            continue
        out = DST.parent / src.name
        out.write_bytes(src.read_bytes())
        out.chmod(0o755)
        print("mirrored %s" % src.name)

    DST.write_text(s.replace(OLD, NEW))
    DST.chmod(0o755)
    print("wrote candidate to %s (%d bytes)" % (DST, DST.stat().st_size))


if __name__ == "__main__":
    main()
