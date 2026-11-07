cd /Users/yuki/src/.harness-worktrees/donegate-pass-cache; git add -A && git commit -q -F - <<'EOF'
donegate 0.1.83: pass cache for Stop/SubagentStop checks (backlog e04711be)

A check that passed is not re-run while root, check name, cmd, workdir,
timeout, HEAD sha, main sha and the working-tree tree (built in a temporary
GIT_INDEX_FILE, real index only read) are unchanged. Only passes are stored;
a failure forgets the entry; any key/store failure is a miss; passes are
stored only if the tree is unchanged after the checks ran. A hit is marked
CACHED on stderr and in the human report.

Removes the #[ignore] on pass_cache_20261107::p1.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
git log --oneline -1