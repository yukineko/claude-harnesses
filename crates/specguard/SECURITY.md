# specguard — security notes

## Trust boundary: `agent.command` (repo config → process spawn)

`agent.command` (and `agent.args`) come from the project's `specguard.toml`
(repo config; see `config.rs` — `AgentConfig`). At audit time the value flows
into `std::process::Command::new(&cfg.command)` in `agent.rs` (`run_one`), which
spawns the executable **directly, with no shell**. That spawn site is the sink;
`specguard.toml` — an in-repo, potentially attacker-influenced file — is the
source.

### Threat

A malicious or compromised `specguard.toml` could set `agent.command` to a
value crafted to run something other than the intended read-only auditor. While
`Command::new` does not itself invoke a shell (so a `;`/`|`/backtick in the
program name would normally just fail to resolve as a path), such values are
never legitimate for an executable name/path and their presence is a reliable
signal of an injection attempt; we reject them rather than pass them to the OS.

### Control

`config.rs` validates `agent.command` during `Config::load` (via
`Config::validate` → `validate_agent_command`), so a bad config makes the load
return an `Err` — the value never reaches `Command::new`:

- The **default** command (the literal `claude`) is **trusted unconditionally**.
- Any **non-default** command is rejected if it contains a shell metacharacter:
  semicolon (`;`), pipe (`|`), ampersand (`&`), backtick (`` ` ``), or
  dollar-sign-followed-by-open-paren command substitution (`$(`). Newline/CR and
  redirection (`<`, `>`) are rejected too. A clean command — letters, digits,
  dash, underscore, slash, dot, and spaces separating args — is accepted.

Regression coverage: `agent_command_accepts_default_and_clean`,
`agent_command_rejects_metachars`, and `agent_command_rejection_fails_config_validate`
in `src/config.rs`.
