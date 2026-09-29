# Portable (BSD + GNU) tree fingerprint helpers for the canary tests.
# Sourced, not executed. Requires python3. FAIL CLOSED: an unreadable or empty
# tree, or any python error, yields a non-zero exit and NO output, so a caller
# can never compare two empty fingerprints and "pass" vacuously.

# fingerprint_tree DIR -> prints "<sha256> <entry-count>" of sorted
# path|size|mtime_ns for every entry under DIR. Exits non-zero if DIR has
# zero entries or cannot be walked.
fingerprint_tree() {
  python3 - "$1" <<'PY'
import hashlib, os, sys
root = sys.argv[1]
if not os.path.isdir(root):
    sys.exit("fingerprint_tree: not a directory: %s" % root)
rows = []
def onerr(e):
    raise e
for d, dirs, files in os.walk(root, onerror=onerr):
    for n in dirs + files:
        p = os.path.join(d, n)
        st = os.lstat(p)
        rows.append("%s|%d|%d" % (p, st.st_size, st.st_mtime_ns))
rows.append("%s|root" % root)
if len(rows) < 2:
    sys.exit("fingerprint_tree: EMPTY tree (vacuous fingerprint refused): %s" % root)
rows.sort()
print("%s %d" % (hashlib.sha256("\n".join(rows).encode()).hexdigest(), len(rows)))
PY
}

# file_mtime FILE -> prints integer mtime (ns); non-zero exit on failure.
file_mtime() {
  python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime_ns)' "$1"
}
