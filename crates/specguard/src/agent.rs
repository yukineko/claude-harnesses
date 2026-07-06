//! Invoke the read-only auditing agent — one process per audit shard.
//!
//! The agent is any command that reads a prompt on stdin and writes its report
//! to stdout (the Claude Code CLI in `--print` mode by default). Each shard
//! (one in-scope area, or the invariant set) is audited by its OWN agent process
//! with a fresh context, so a large multi-area run never accumulates unrelated
//! files into a single context window (context-rot mitigation). Shards run
//! concurrently up to [`MAX_PARALLEL`].
//!
//! For each process we stream the prompt in on a separate thread so a large
//! stdout can drain concurrently, avoiding a pipe-buffer deadlock.

use crate::config::AgentConfig;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;

/// Cap on concurrent agent processes. Each `claude` call is heavy, so we bound
/// fan-out rather than spawning one process per area unconditionally.
pub const MAX_PARALLEL: usize = 4;

pub struct AgentOutput {
    pub stdout: String,
    pub stderr: String,
    /// Process exit code (-1 if terminated by a signal or never spawned).
    pub code: i32,
}

/// One shard's prompt plus the label used for it in logs and the merged report.
pub struct ShardPrompt {
    pub label: String,
    pub prompt: String,
}

/// Result of auditing one shard.
pub struct ShardOutput {
    pub label: String,
    pub out: AgentOutput,
}

/// Audit every shard, at most [`MAX_PARALLEL`] processes at a time. Never errors:
/// a spawn/exec failure for a shard is captured as an [`AgentOutput`] with
/// `code = -1` and the error on stderr, so the caller's exit-code aggregation
/// treats it as an agent failure like any other non-zero exit.
pub fn run_shards(
    cfg: &AgentConfig,
    repo_root: &std::path::Path,
    shards: Vec<ShardPrompt>,
) -> Vec<ShardOutput> {
    run_shards_with(shards, |prompt| run_one(cfg, repo_root, prompt))
}

/// Core of [`run_shards`] with the per-shard runner injected. Production passes
/// [`run_one`]; tests pass a runner that can panic, to exercise the
/// never-break-a-turn path.
///
/// Never-break-a-turn: `thread::scope` **re-raises** the panic of any scoped
/// thread that isn't manually joined, so a single worker panic would abort the
/// whole audit and break the turn. Each shard's runner is therefore isolated in
/// `catch_unwind`; a caught panic leaves slot `idx` as `None`, folded below into
/// a `code = -1` agent failure exactly like a spawn failure — the other shards
/// still complete. The mutexes are additionally poison-tolerant (recover via
/// `into_inner`) as defence in depth.
fn run_shards_with(
    shards: Vec<ShardPrompt>,
    run: impl Fn(&str) -> AgentOutput + Sync,
) -> Vec<ShardOutput> {
    let n = shards.len();
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<ShardOutput>>> = Mutex::new((0..n).map(|_| None).collect());
    let workers = MAX_PARALLEL.min(n).max(1);

    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let idx = {
                    let mut g = next.lock().unwrap_or_else(|e| e.into_inner());
                    if *g >= n {
                        break;
                    }
                    let i = *g;
                    *g += 1;
                    i
                };
                let shard = &shards[idx];
                // Isolate the (heavy, external) runner: a panic here must never
                // escape the scoped thread, or `thread::scope` would re-raise it
                // and break the turn. On panic, skip writing the slot — it stays
                // `None` and is folded below into a `code = -1` agent failure.
                let out = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&shard.prompt)
                })) {
                    Ok(out) => out,
                    Err(_) => continue,
                };
                if let Ok(mut g) = results.lock().or_else(|e| Ok::<_, ()>(e.into_inner())) {
                    g[idx] = Some(ShardOutput {
                        label: shard.label.clone(),
                        out,
                    });
                }
            });
        }
    });

    // Recover from a poisoned mutex; fold missing slots (worker panic) as agent failures.
    results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .enumerate()
        .map(|(idx, o)| {
            o.unwrap_or_else(|| ShardOutput {
                label: shards[idx].label.clone(),
                out: AgentOutput {
                    stdout: String::new(),
                    stderr: format!(
                        "specguard: worker thread panicked for shard '{}'",
                        shards[idx].label
                    ),
                    code: -1,
                },
            })
        })
        .collect()
}

/// Run one agent process, feeding `prompt` on stdin. Spawn/exec failures are
/// folded into an `AgentOutput { code: -1 }` so fan-out never aborts midway.
fn run_one(cfg: &AgentConfig, repo_root: &std::path::Path, prompt: &str) -> AgentOutput {
    // `cfg.command` originates from repo config (`specguard.toml`) and is spawned
    // directly here with no shell. It is validated at load time
    // (`config::validate_agent_command`): the default (`claude`) is trusted, and
    // any non-default command containing shell metacharacters is rejected before
    // reaching this sink. See SECURITY.md.
    let child = Command::new(&cfg.command)
        .args(&cfg.args)
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return AgentOutput {
                stdout: String::new(),
                stderr: format!("spawning agent '{}': {e}", cfg.command),
                code: -1,
            };
        }
    };

    // Write the prompt on a dedicated thread; if writing fails (e.g. the agent
    // closes stdin early) we ignore it and let the exit code carry the failure.
    let stdin = child.stdin.take();
    let prompt_owned = prompt.to_string();
    let writer = thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(prompt_owned.as_bytes());
            // stdin dropped here -> EOF for the child.
        }
    });

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            let _ = writer.join();
            return AgentOutput {
                stdout: String::new(),
                stderr: format!("waiting for agent: {e}"),
                code: -1,
            };
        }
    };
    let _ = writer.join();

    AgentOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn shard(label: &str) -> ShardPrompt {
        ShardPrompt {
            label: label.to_string(),
            prompt: label.to_string(),
        }
    }

    /// Never-break-a-turn (backlog 733499ce): a worker that panics mid-audit must
    /// NOT propagate out of `run_shards` and kill the turn. `thread::scope`
    /// re-raises un-joined scoped-thread panics, so without the in-worker
    /// `catch_unwind` this call panics (RED). With it, the panicking shard is
    /// folded as a `code = -1` failure and every other shard still completes.
    #[test]
    fn worker_panic_is_folded_not_propagated() {
        // Silence the default panic hook so the injected panic doesn't clutter
        // test output (the panic is expected and handled).
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        // One shard's runner panics; the rest return a normal success output.
        // A shared counter proves the healthy shards actually ran.
        let ran = AtomicUsize::new(0);
        let run = |prompt: &str| -> AgentOutput {
            if prompt == "boom" {
                panic!("injected worker panic for shard 'boom'");
            }
            ran.fetch_add(1, Ordering::SeqCst);
            AgentOutput {
                stdout: format!("ok:{prompt}"),
                stderr: String::new(),
                code: 0,
            }
        };

        let shards = vec![shard("a"), shard("boom"), shard("b"), shard("c")];

        // Must return normally — a propagated panic here fails the test (RED on
        // the un-caught code path).
        let outs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_shards_with(shards, run)
        }));
        std::panic::set_hook(prev);

        let outs = outs.expect("run_shards must not propagate a worker panic (turn stays alive)");

        // Order is preserved (results indexed by shard position).
        assert_eq!(outs.len(), 4);
        assert_eq!(outs[0].label, "a");
        assert_eq!(outs[1].label, "boom");
        assert_eq!(outs[2].label, "b");
        assert_eq!(outs[3].label, "c");

        // The panicking shard is folded as an agent failure, not lost or fatal.
        assert_eq!(outs[1].out.code, -1, "panicked shard must fold to code -1");
        assert!(
            outs[1].out.stderr.contains("panicked"),
            "folded failure must note the panic: {}",
            outs[1].out.stderr
        );

        // Every healthy shard completed with its real output.
        for (i, label) in [(0usize, "a"), (2, "b"), (3, "c")] {
            assert_eq!(outs[i].out.code, 0, "healthy shard '{label}' must succeed");
            assert_eq!(outs[i].out.stdout, format!("ok:{label}"));
        }
        assert_eq!(
            ran.load(Ordering::SeqCst),
            3,
            "all three healthy shards must have run despite the sibling panic"
        );
    }

    /// The all-healthy path still returns one output per shard, in order.
    #[test]
    fn all_shards_run_and_preserve_order() {
        let run = |prompt: &str| AgentOutput {
            stdout: format!("ok:{prompt}"),
            stderr: String::new(),
            code: 0,
        };
        let shards = vec![shard("s0"), shard("s1"), shard("s2")];
        let outs = run_shards_with(shards, run);
        assert_eq!(outs.len(), 3);
        for (i, o) in outs.iter().enumerate() {
            assert_eq!(o.label, format!("s{i}"));
            assert_eq!(o.out.code, 0);
            assert_eq!(o.out.stdout, format!("ok:s{i}"));
        }
    }
}
