# hostaudit

Configuration / operations security audit for servers (over ssh), an AWS
account, and the local Claude Code settings. It checks them against a fixed
baseline and prints a fix for every violation.

- Canon (the contract): [`docs/hostaudit-baseline-v1.md`](../../docs/hostaudit-baseline-v1.md)
- Testable interface (CLI, exit codes, JSON, env seams): [`CONTRACT.md`](CONTRACT.md)

## Verdicts and exit codes

Every rule yields `clean`, `not_applicable` (observed, nothing to check — kept
separate from clean), `violation` (always carries a `fix`) or `undetermined`
(could not observe — connection failure, sudo failure, unexpected exit,
unparseable output, advisories unavailable, AWS login expired). An
undetermined check is never reported as clean.

| exit | meaning |
|---|---|
| 0 | every finding is clean / not_applicable |
| 1 | at least one violation or undetermined |
| 2 | usage error |
| 3 | refused (no selection, unknown alias, missing confirmation, …) |
| 4 | inventory undetermined (ssh config unreadable, `--from-cmd` failed, AWS login expired) |

## Usage

```sh
hostaudit inventory [--json] [--from-cmd CMD] [--aws]   # ~/.ssh/config (Include followed), CMD output, EC2
hostaudit select web1 db1                                # or: hostaudit select --all
hostaudit audit [--json]                                 # B1–B9 over ssh, selected hosts only
hostaudit local [--json]                                 # L1–L3 over the 4 Claude Code settings files
hostaudit aws [--json]                                   # A1–A7, read-only
hostaudit aws apply --finding A1:<region>:<sg>:<sgr> --confirm <sgr> [--replace-cidr <cidr>]
hostaudit daily-task --print                             # ~/.daily/config.toml stanza
```

Every run appends one row per finding to `~/.hostaudit/reports.jsonl`. Every
violation or undetermined finding also goes to overwatch's
`review_findings.jsonl`, written through overwatch's own library and stored
as `unverified`. A SessionStart hook prints one line with the latest counts;
when there is no report yet, it says the state is UNKNOWN.

Thresholds live in `~/.hostaudit/config.toml`: `ssh_connect_timeout_secs`
(default 10, clamped to 1..=60) and `osv_cache_max_age_hours` (default 24,
clamped to 1..=168).

## Read-only guarantee

- Auditing is read-only. The ssh side can only send the twelve whitelisted
  commands W1–W11 (a closed enum, `src/ssh.rs`); `sudo -n` is used only for
  `sshd -T`, `ufw status` and `nft list ruleset`. The AWS side can only issue
  the Describe/List/Get calls AW1–AW9 (a closed enum, `src/aws.rs`). No code
  path builds an arbitrary command string.
- Only hosts you `select` are ever passed to ssh. `ssh` runs with
  `BatchMode=yes` and a connect timeout, and the whole call has a time limit.
- What leaves the machine: source package names and versions to OSV, TLS
  connections to port 443 of the audited hosts, and AWS API calls.
- Secrets: lines containing `-----BEGIN` are dropped, runs of 40 or more
  token characters become `<redacted>`, and settings `env` values and AWS
  credentials are never written.

## Security-group remediation is manual by default

Each A1/A2 finding carries the region, the SG id and name, the attachments
(instance, Name tag, ENI), the rule (`sgr-…`), the console path
(EC2 → セキュリティグループ → `<sg>` → 「インバウンドルール」→「インバウンドルールを編集」)
and the equivalent `aws ec2 revoke-security-group-ingress` command. You
apply the fix yourself.

`hostaudit aws apply` is the single opt-in exception, and it only revokes or
replaces one ingress rule. The order is fixed: show the diff, `--dry-run`
permission check, explicit per-finding confirmation (`--confirm <sgr-id>` or
typing the id on a TTY), authorize the replacement before the revoke,
revoke, then re-audit. It refuses when `CLAUDECODE` is set (autonomous
context), when stdin is not a TTY and `--confirm` is missing, and when
closing the rule would leave no other rule in the same SG admitting tcp/22
and `--replace-cidr` was not given.

## Not covered by the test suite

The real default paths are not covered by the tests (they use fakes):
`ssh`, `aws`, the https OSV fetch (`curl -fsSL` plus `unzip -p` on the OSV
`<ecosystem>/all.zip` dump), the built-in TLS probe
(`openssl s_client` | `openssl x509 -noout -enddate`), the interactive TTY
confirmation, the overwatch review-queue append and the SessionStart line.
