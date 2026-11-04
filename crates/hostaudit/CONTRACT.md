# hostaudit — implementation contract (v1 + v1.1, tests: `crates/hostaudit/tests/`)

Canon: `docs/hostaudit-baseline-v1.md` (baseline v1) and `docs/hostaudit-baseline-v1.1.md`
(baseline v1.1, contract in §12 below; where v1.1 contradicts v1, v1.1 wins, e.g. the `warning`
verdict and the exit code). This file fixes the
**testable interface** the canon leaves open. Where this file and the canon
disagree, the canon wins and this file is the bug. Choices made here that the
canon does not dictate are marked **[choice]**; gaps in the canon are marked
**[canon gap]** and need a decision log (`specguard decide`).

The black-box tests in `tests/` were written against this file by a different
agent than the implementer (CLAUDE.md §2 (a)). They spawn
`env!("CARGO_BIN_EXE_hostaudit")` with a **cleared environment** (only the
variables listed below, `HOME=<tempdir>`, `PATH=/usr/bin:/bin`), stdin = null
(never a TTY) unless stated otherwise.

## 1. Environment seams (all externals are replaceable; tests never touch the network)

| variable | meaning | default when unset |
|---|---|---|
| `HOME` | `~` for `~/.ssh/config`, `~/.claude/…`, `~/.hostaudit/…` | process HOME |
| `HOSTAUDIT_SSH` | path of the ssh executable | `ssh` on PATH |
| `HOSTAUDIT_AWS` | path of the aws CLI executable | `aws` on PATH |
| `HOSTAUDIT_OSV_URL` | advisory feed URL (§5). `file://` MUST be supported | implementation-defined https feed |
| `HOSTAUDIT_TLS_PROBE` | path of the B9 certificate probe (§4.9) | built-in (`openssl s_client` + `openssl x509 -noout -enddate`) |
| `HOSTAUDIT_NOW` | audit-machine clock, unix seconds (integer). Every local clock read (report `time`, OSV cache age, TLS expiry) uses it | system clock |
| `CLAUDECODE` | if set (any value), `aws apply` refuses (autonomous context, §7) | — |

An unparseable `HOSTAUDIT_NOW` is a usage error (exit 2).

## 2. Exit codes [choice]

| code | meaning |
|---|---|
| 0 | the command ran and every finding is `clean`, `not_applicable` or (v1.1) `warning` (for `inventory`/`select`/`daily-task`: success) |
| 1 | the command ran and at least one finding is `violation` or `undetermined` (for `aws apply`: the change ran but the re-audit is not clean) |
| 2 | usage error (bad flags) |
| 3 | **refused**: precondition not met — unknown alias, no selection, `--all` resolved to empty, `aws apply` without valid confirmation, unknown/non-applicable finding id, dry-run permission failure, port-22 lockout without `--replace-cidr` |
| 4 | **inventory undetermined**: ssh config unreadable/unparseable, `--from-cmd` exited non-zero, `--aws` with sts failure |

The scaffold exits 2 for every verb; no test accepts 2 as success.

## 3. Files

- `~/.hostaudit/selection.toml` — written only by `select` (replaced on each successful `select`; untouched on refusal):
  ```toml
  [[host]]
  alias = "web1"
  hostname = "10.0.0.1"   # optional
  user = "ubuntu"         # optional
  port = 2222             # optional
  proxy_jump = "bastion"  # optional
  source = "ssh_config"   # "ssh_config" | "from_cmd" | "ec2"
  ```
- `~/.hostaudit/reports.jsonl` — `audit`, `local`, `aws`, `aws apply` **append** one JSON object per finding (§8).
- `~/.hostaudit/config.toml` (optional) — `ssh_connect_timeout_secs` (default 10, clamp 1..=60),
  `osv_cache_max_age_hours` (default 24, clamp 1..=168). Unparseable config → exit 2.
- `~/.hostaudit/osv-cache/` — OSV cache, layout implementation-defined, but **keyed by ecosystem, not by URL**,
  and it records the fetch time (from `HOSTAUDIT_NOW`).

## 4. Subcommands

### 4.1 `inventory [--json] [--from-cmd CMD] [--aws]`

Sources, merged in this order: `~/.ssh/config`, `--from-cmd`, `--aws` (EC2).

ssh config parsing:
- keywords case-insensitive; `Key value` and `Key=value` both accepted; `#` comments and blank lines ignored.
- `Include <path>...`: relative paths resolve against `~/.ssh/`; glob (`*`, `?`) supported; a glob matching nothing is fine.
- `Host` lines may list several patterns. Each token without `*`, `?`, or a leading `!` is an alias; wildcard/negated tokens are skipped.
- per alias: `HostName`, `User`, `Port`, `ProxyJump` are the **first** value of that keyword inside the `Host` block(s) where the alias appears (no `Host *` inheritance; the connection uses the alias so ssh resolves the rest). `Match` ends the current Host block.
- `~/.ssh/config` absent → empty list from this source and `"ssh_config": "absent"` (exit 0).
- unreadable (I/O error, e.g. it is a directory, permission denied, non-UTF-8) or unparseable (`Port` not an integer 1..=65535) → exit 4, and stdout MUST NOT contain a host list.

`--from-cmd CMD`: run `/bin/sh -c CMD`; only exit 0 counts — non-zero → exit 4. Each stdout line, trimmed, non-empty, not starting with `#`, is one host with `alias = hostname = line`, `source = "from_cmd"`.

`--aws`: runs AW1, AW2, AW4 (§6); every instance becomes a host with `alias = <instance id>`, `hostname` = public IP if present else private IP, `source = "ec2"`, plus `instance_id`, `region`, `public_ip`, `private_ip`, `name` (Name tag). sts failure → exit 4.

`--json` output:
```json
{"ssh_config": "present" | "absent",
 "hosts": [{"alias": "web1", "hostname": "10.0.0.1"|null, "user": "ubuntu"|null,
            "port": 2222|null, "proxy_jump": "bastion"|null, "source": "ssh_config"}]}
```

### 4.2 `select [--from-cmd CMD] [--aws] (<alias>... | --all)`

Builds the inventory exactly as `inventory` does (same exit 4 conditions), then:
- every given alias must be in the inventory; any unknown alias → exit 3, selection file untouched.
- `--all` selects every inventory host; resolving to zero hosts → exit 3, selection file untouched.
- success → selection.toml replaced, exit 0.

### 4.3 `audit [--json]`

- no `selection.toml`, or zero hosts in it → exit 3, **no ssh invocation**.
- only hosts in `selection.toml` are ever passed to ssh.
- For each selected host, each whitelist command (§4.4) is one ssh invocation. argv:
  - contains the adjacent pairs `-o` `BatchMode=yes` and `-o` `ConnectTimeout=<n>` (n from config, default 10);
  - the **second-to-last** argv element is the destination: the alias for `ssh_config` hosts, the hostname for `from_cmd`/`ec2` hosts;
  - the **last** argv element is the remote command string, exactly one of the §4.4 strings. Other `-o` options may be added.
- Exactly one finding per (host, rule B1..B9) — no rule is ever omitted. `target` = alias.
- `--json` prints `{"findings":[ <finding>... ]}` (§8). Without `--json`, a human-readable listing (format free).

### 4.4 Remote command strings (complete — the fake ssh is matched on these byte-for-byte)

| id | remote command string |
|---|---|
| W1 | `LC_ALL=C cat /etc/os-release` |
| W2 | `LC_ALL=C dpkg-query -W -f '${Package}\t${Version}\t${source:Package}\t${source:Version}\n'` (literal backslash-t / backslash-n, single-quoted) |
| W3 | `LC_ALL=C ss -H -tln` |
| W4 | `sudo -n LC_ALL=C sshd -T` |
| W5 | `sudo -n LC_ALL=C ufw status` |
| W6 | `sudo -n LC_ALL=C nft list ruleset` |
| W7a | `LC_ALL=C stat -c %Y /var/lib/apt/periodic/update-success-stamp` |
| W7b | `LC_ALL=C stat -c %Y /var/lib/apt/lists` |
| W8 | `LC_ALL=C stat -c %Y /var/run/reboot-required` |
| W9 | `LC_ALL=C systemctl is-enabled unattended-upgrades` |
| W10 | `LC_ALL=C cat /etc/apt/apt.conf.d/20auto-upgrades` |
| W11 | `LC_ALL=C date +%s` |

[choice] For sudo commands `LC_ALL=C` is passed as a sudo `VAR=value` argument (so it survives `env_reset`).

Outcome of one invocation:
- exit 0 → observed stdout.
- exit 255 (ssh failure / timeout) → undetermined.
- W7a/W7b/W8/W10: exit 1 **and** stderr contains `No such file or directory` → observed "absent".
- W9: stdout first line `disabled` / `masked` / `not-found` (any exit code) → observed that value.
- anything else non-zero (incl. `sudo: a password is required`) → undetermined.

### 4.5 Rules (thresholds: canon §7; `now_remote` = W11)

- **B1** W4 line `permitrootlogin <v>`: `yes` → violation; any other value → clean; key missing → undetermined.
- **B2** W4 line `passwordauthentication <v>`: `yes` → violation; else clean; missing → undetermined.
- **B3** W5 `Status: active` = active; `Status: inactive` = inactive; other → undetermined. W6 has a chain whose body contains `hook input` and `policy drop` → has-drop. If W5 **or** W6 is undetermined → undetermined (canon: either side). Else active or has-drop → clean, otherwise violation.
- **B4** each W3 line, 4th whitespace column = local `addr:port`. Port in {3306,5432,6379,27017,9200,11211} with addr `0.0.0.0`, `[::]` or `*` → violation (evidence = the line(s); fix names the service and says bind to `127.0.0.1`). Else clean.
- **B5** §5.
- **B6** [choice: OR of observed violations] violation if W9 observed ≠ `enabled`, **or** W10 observed absent, **or** W10 lacks a line containing `APT::Periodic::Unattended-Upgrade "1"`. If no side shows a violation and a side is undetermined → undetermined. Else clean. Evidence includes the W9 value and every W10 line containing `APT::Periodic::`.
- **B7** `t = max(present W7a, W7b)`; both absent → undetermined; any of W7a/W7b/W11 undetermined → undetermined [choice: a missing side could hold the max]. `now_remote − t > 7*86400` → violation, else clean. Report row carries `apt_index_time = t`.
- **B8** W8 absent → clean; present and `now_remote − W8 > 7*86400` → violation; else clean; W8 or W11 undetermined → undetermined.
- **B9** no W3 line with local port 443 (any addr) → `not_applicable`; W3 undetermined → undetermined. Otherwise run the TLS probe: `$HOSTAUDIT_TLS_PROBE <hostname> 443` where hostname = the host's `hostname` (ssh `HostName`) or the alias if none. The probe exits 0 and prints one line `notAfter=<openssl date, e.g. Jan 20 00:00:00 2027 GMT>`. Non-zero / unparseable → undetermined. `notAfter − HOSTAUDIT_NOW ≤ 14*86400` → violation, else clean.

Every violation's `fix` is non-empty and contains the canon §3.3 instruction (e.g. B1 `sshd -t`, B3 `ufw enable`, B5 `--only-upgrade`, B6 `dpkg-reconfigure`, B7 `apt update`, B9 `certbot renew`).

## 5. OSV (B5)

- Feed: `HOSTAUDIT_OSV_URL`, where the literal `{ecosystem}` (if present) is replaced by the ecosystem string. The fetched body is a **JSON array of OSV vulnerability objects** (OSV schema: `id`, `affected[].package.{ecosystem,name}`, `affected[].ranges[]` with `type: "ECOSYSTEM"` and `events` `introduced`/`fixed`, optional `affected[].versions[]`). [choice] For `file://` the path is read directly.
- Ecosystem from W1: `ID=ubuntu` → records whose ecosystem is `Ubuntu:<VERSION_ID>` or starts with `Ubuntu:<VERSION_ID>:` (e.g. `Ubuntu:22.04:LTS`; `Ubuntu:Pro:…` does not match). `ID=debian` → `Debian:<VERSION_ID>`. Any other `ID` (ID_LIKE is not consulted) → B5 undetermined. W1/W2 undetermined → undetermined.
- Matching uses the **source** package and **source** version (`${source:Package}`, `${source:Version}`), never the binary name. A source version is affected if it is in `versions[]`, or for some ECOSYSTEM range `introduced ≤ v < fixed` under **dpkg version ordering** (epoch, upstream, revision; `~` sorts before everything, incl. end of string). `introduced: "0"` = from the beginning; no `fixed` = still affected.
- Any affected package → violation; `fix` contains `sudo apt update && sudo apt install --only-upgrade <binary package(s)>` and the advisory id(s); evidence names binary package, version and advisory id.
- Zero records for the host's ecosystem in the feed → undetermined (not clean).
- Fetch failure (unreadable file, non-JSON, HTTP failure): use the cache if its age (by `HOSTAUDIT_NOW`) ≤ `osv_cache_max_age_hours`; otherwise undetermined. A successful fetch refreshes the cache.
- B5 report rows (and JSON findings) carry `advisory_fetched_at` = the unix time the advisory data used was fetched (cache time when the cache was used).

## 6. `aws [--json]` (read-only)

- aws argv: `argv[1]` = service, `argv[2]` = operation, then options; every call includes `--output json`; regional calls include `--region <r>`.
- Allowed calls during `aws` / `inventory --aws` (complete): `sts get-caller-identity`, `ec2 describe-regions`, `ec2 describe-security-groups`, `ec2 describe-security-group-rules` **[canon gap: needed for the `sgr-…` ids §5 requires; treated as part of AW3]**, `ec2 describe-instances`, `ec2 describe-network-interfaces`, `s3api list-buckets`, `s3api get-public-access-block`, `s3api get-bucket-policy-status`, `rds describe-db-instances`, `iam get-account-summary`, `cloudtrail describe-trails`, `cloudtrail get-trail-status`.
- Order: AW1 first. sts non-zero → **no further aws call**; every rule A1..A7 gets one `undetermined` finding (target `aws`) whose `reason` contains `ログインが切れている`; exit 1.
- Regions from `ec2 describe-regions` (`Regions[].RegionName`); ec2/rds calls per region.
- Every rule A1..A7 yields ≥ 1 finding. No violations for a rule → one `clean` finding with target `aws`. A failed call for a rule → that rule `undetermined`.
- A1/A2: one finding per offending ingress rule (`IsEgress=false`); `-1` protocol rules are A2 only. A1 port set: 22, 3389, 3306, 5432, 6379, 27017, 9200, 11211, matched when `FromPort ≤ p ≤ ToPort`; CIDR `0.0.0.0/0` (`CidrIpv4`) or `::/0` (`CidrIpv6`).
- A3: per bucket — PAB missing (`NoSuchPublicAccessBlockConfiguration`) or any of the 4 flags false → violation; `PolicyStatus.IsPublic = true` → violation; `NoSuchBucketPolicy` = not public.
- A4: `PubliclyAccessible = true` → violation (target = DB identifier). A5: `AccountMFAEnabled = 0` or `AccountAccessKeysPresent = 1` → violation (target `aws`). A6: no trail with `IsMultiRegionTrail = true` and `get-trail-status` `IsLogging = true` → violation. A7: `MetadataOptions.HttpTokens = optional` → violation (target = instance id), fix contains `aws ec2 modify-instance-metadata-options --instance-id <id> --http-tokens required` (printed, **never executed**).
- AWS credentials (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`) are inherited by the aws process but never printed or recorded.

## 7. `aws apply --finding <id> [--confirm <sgr-id>] [--replace-cidr <CIDR>]`

Finding ids: `A1:<region>:<sg-id>:<sgr-id>`, `A2:<region>:<sg-id>:<sgr-id>` (other rules: `<rule>:<target>`).

Confirmation [choice]: accepted only from (a) `--confirm <sgr-id>` equal to the finding's sgr id, or (b) an interactive prompt on a TTY stdin where the human types the sgr id. Refuse (exit 3) **before any mutating or dry-run call** when: `CLAUDECODE` is set; stdin is not a TTY and `--confirm` is absent; `--confirm` does not match; the finding id is unknown, not A1/A2, or no longer present after re-locating it (AW1 + describe calls).

Port-22 lockout [choice of condition]: if the rule covers port 22 and, after removing it, no other ingress rule of the same SG covers tcp 22 → without `--replace-cidr` refuse (exit 3) with a warning on stderr that mentions `--replace-cidr`, before any mutating/dry-run call.

Sequence when confirmed:
1. print the diff (rule removed, rule added if `--replace-cidr`);
2. dry-run: `ec2 revoke-security-group-ingress --region <r> --group-id <sg> --security-group-rule-ids <sgr> --dry-run` (and, with `--replace-cidr`, `ec2 authorize-security-group-ingress … --protocol tcp --port <p> --cidr <CIDR> --dry-run`). Dry-run success = non-zero exit with stderr containing `DryRunOperation`; anything else (e.g. `UnauthorizedOperation`) → exit 3, no real call;
3. with `--replace-cidr`: real `authorize-security-group-ingress` **before** the real revoke;
4. real `revoke-security-group-ingress` for exactly that one sgr id (no other rule is touched);
5. re-audit with `ec2 describe-security-group-rules` / `describe-security-groups`: rule gone → exit 0; still present or undetermined → exit 1.

## 8. Finding JSON / report rows

```json
{"id": "B1:web1", "target": "web1", "rule": "B1",
 "verdict": "clean" | "not_applicable" | "violation" | "undetermined",
 "evidence": ["permitrootlogin yes"],     // array of strings, each ≤ 512 bytes
 "fix": "…" | null,                       // non-empty string when verdict = violation
 "reason": "…" | null,                    // non-empty string when verdict = undetermined
 // B5: "advisory_fetched_at": <unix secs>; B7: "apt_index_time": <unix secs>
 // A1/A2 additionally:
 "region": "ap-northeast-1", "sg_id": "sg-…", "sg_name": "…",
 "attached": [{"instance_id": "i-…", "name": "web", "eni": "eni-…"}],
 "protocol": "tcp", "port_range": "22" | "3300-3310" | "all", "cidr": "0.0.0.0/0", "sgr_id": "sgr-…",
 "console_path": "EC2 → セキュリティグループ → <sg-id> → 「インバウンドルール」→「インバウンドルールを編集」→ <sgr-id>",
 "cli": ["aws ec2 revoke-security-group-ingress --region <r> --group-id <sg> --security-group-rule-ids <sgr>"]}
```

`reports.jsonl` rows = the finding object plus `"time": <HOSTAUDIT_NOW unix secs>`.

Secret removal applies to stdout, stderr and the report: lines containing `-----BEGIN` are dropped; settings `env` values are `<redacted>`; every run of ≥ 40 chars of `[A-Za-z0-9_/+=-]` is replaced by `<redacted>` in place (the rest of the line stays); AWS access keys / secret keys / session tokens are never written.

## 9. `local [--json]`

Files: `~/.claude/settings.json`, `~/.claude/settings.local.json`, `<cwd>/.claude/settings.json`, `<cwd>/.claude/settings.local.json`. One finding per (file, L1..L3); `target` = absolute file path. Absent → `not_applicable`; unreadable or invalid JSON → `undetermined`; present → L1 violation if `permissions.allow` contains exactly `Bash`, `Bash(*)`, `Bash(:*)` or `*` (evidence = the entry); L2 if `permissions.defaultMode == "bypassPermissions"`; L3 if `skipDangerousModePermissionPrompt == true`; otherwise clean.

## 10. `daily-task --print`

Prints a TOML snippet for `~/.daily/config.toml` with one or more `[[task]]` tables (`name`, `command`); taken together the commands run `hostaudit audit`, `hostaudit local` and `hostaudit aws` (never `aws apply`). Exit 0.

## 11. Not covered by this test batch

overwatch `review_findings.jsonl` append and the SessionStart one-line systemMessage (canon §1) are part of the canon but have no tests yet; the http(s) OSV fetch path and the built-in TLS probe are not exercised (tests use `file://` and `HOSTAUDIT_TLS_PROBE`).

---

# 12. v1.1 additions (canon: `docs/hostaudit-baseline-v1.1.md`; tests: `tests/v11_*.rs`)

Everything in §1–§11 stays unless this section says otherwise. Marking as above:
**[choice]** = fixed here, canon does not dictate; **[canon gap]** = canon is silent or
inconsistent and a decision log is needed.

## 12.0 Test-harness changes the implementer must know

- `tests/common/mod.rs::Env::new()` now also sets `HOSTAUDIT_KEV_URL` and `HOSTAUDIT_EOL_URL`
  to **non-existent** `file://` paths (= fetch failure), so no test can reach the network.
- `common::clean_host()` now answers every v1.1 command with a clean Ubuntu 22.04 answer
  (jammy, kernel 5.15.0-100-generic). Existing v1 tests keep passing because they only assert
  B-rule behaviour and "exit 0 on a clean host"; the latter now requires the v1.1 rules to be
  clean/not_applicable on that host (K1 via the W17 fallback, because the endoflife feed is absent there).
- The v1 test `only_whitelisted_commands_reach_ssh...` now uses `common::is_whitelisted` /
  `is_sudo_cmd` (v1 plus v1.1 whitelist); the v1 report-row test accepts verdict `warning`.

## 12.1 Verdict `warning`, exit codes, summary

- Finding JSON `verdict` additionally takes `"warning"`. A `warning` has a non-empty `fix`
  (the advice). It is never silent: it appears in `--json`, text output, and `reports.jsonl`.
- Exit code: `1` iff any finding is `violation` or `undetermined`; **warnings alone exit 0**
  (this replaces the §2 wording "every finding is clean or not_applicable").
- Text output (no `--json`) of `audit` contains the literal fragment
  `violation <V> / undetermined <U> / warning <W> / clean <C>` (canon §1 example). [choice] a trailing
  ` / not_applicable <N>` may follow.
- `audit --json` prints `{"findings":[...], "summary": {"violation":V,"undetermined":U,"warning":W,"clean":C,"not_applicable":N}}`. [choice: the `summary` key]
- Multi-item rules (B5 CVEs, S8, S9) take the worst item: Violation > Undetermined > Warning > Clean.
  No item at all gives `not_applicable` (S8/S9).
- Rows appended to `reports.jsonl` follow §8 with `verdict: "warning"`.
- SessionStart count line, daily 1-line summary and the overwatch review-queue exclusion of warnings are
  canon requirements **not covered by tests** (same status as §11).

## 12.2 Findings per host, rule set, OS dispatch

For every selected host exactly one finding per rule in
`B1..B9, S1, S2, S3, S5, S6, S7, S8, S9, K1..K5, WN1..WN5` (target = alias, id `<rule>:<target>`).
**K6 is parked: no finding at all.** (S4 is replaced by K1; there is no S4.)

OS dispatch:
1. Run W1. exit 0 means Linux. `ID=ubuntu|debian` is the Debian family (all rules). Any other `ID`
   (e.g. `amzn`): B1-B4, B6-B9 as in v1, K1 evaluated, and S1,S2,S3,S5,S6,S7,S8,S9,K2,K3,K4,K5 are
   `undetermined` with a `reason` containing `未実装`. B5 keeps the v1 behaviour (non Ubuntu/Debian: undetermined).
2. W1 non-zero: run W0 (`cmd /c ver`). Output contains `Microsoft Windows` means Windows: B*/S*/K* are
   `not_applicable`, WN1..WN5 are evaluated. [choice] On a Windows host **no other command** than
   W1, W0 and WN1..WN5 is sent. W0 is **never** sent when W1 succeeded.
3. Neither (W1 non-zero and W0 not Windows / failed / ssh 255): **every** rule (incl. WN*) is
   `undetermined` with a reason.
4. Linux host: WN1..WN5 are `not_applicable`.
5. Debian: S2 and K5 are `not_applicable` (canon: not Ubuntu).

## 12.3 Remote command strings (byte-exact; the fake ssh matches on these)

All `LC_ALL=C`-prefixed; `[sudo]` ones are `sudo -n LC_ALL=C ...` (same convention as §4.4).
The outcome rules of §4.4 apply (255 is undetermined, other non-zero is undetermined) except the
per-command exceptions below. **[choice]** "command not found" is recognised by the substring
`command not found` in stderr regardless of exit code (so both `exit 127` and
`sudo: <tool>: command not found` with exit 1 mean "tool missing").

| id | remote command string |
|---|---|
| W0 | `cmd /c ver` (no `LC_ALL`, Windows) |
| W12 [sudo] | `sudo -n LC_ALL=C ss -H -tlnp` |
| W12b [sudo] | `sudo -n LC_ALL=C readlink /proc/<pid>/exe` where `<pid>` is digits only, taken from `pid=<digits>` in W12 |
| W13 | `LC_ALL=C dpkg -S <path>`; path starts with `/`, no whitespace, no quote, none of `;` `\|` `&` `$` or backtick. Exit 1 + stderr `no path found` means "not managed by dpkg" |
| W14 | `LC_ALL=C apt-get -s -o Debug::NoLocking=1 dist-upgrade` |
| W15 | `LC_ALL=C pro security-status --format json` |
| W16 [sudo] | `sudo -n LC_ALL=C needrestart -b -r l` |
| W17 | `LC_ALL=C ubuntu-distro-info --supported` (Ubuntu) / `LC_ALL=C debian-distro-info --supported` (Debian) |
| W18 | `LC_ALL=C cat /etc/apt/sources.list`, `LC_ALL=C ls /etc/apt/sources.list.d`, then per listed file `LC_ALL=C cat /etc/apt/sources.list.d/<name>`; name matches `[A-Za-z0-9._-]+` **and** ends in `.list` or `.sources` (other names are never sent) |
| W19 | `LC_ALL=C dpkg --audit` |
| W20 | `LC_ALL=C apt-mark showhold` |
| W21 | `LC_ALL=C <cmd>` with `<cmd>` from the list below |
| W22 | `LC_ALL=C <exe-path> <version-args>`; `<exe-path>` = W12b output of a listening process whose W13 says "not managed by dpkg", sanitised as for W13, whose basename is a §4.1 product executable. dpkg-owned and non-product executables are **never executed** |
| W23 [sudo] | `sudo -n LC_ALL=C docker ps --format '{{.Image}}'` (single quotes literal) |
| W24 | `LC_ALL=C uname -r` |
| W25 | `LC_ALL=C pro status --format json` |

W21 commands (exact; `common::W21_CMDS`): `node --version`, `python3 --version`,
`python2 --version`, `java -version`, `ruby -v`, `docker --version`, `containerd --version`,
`openssl version`, `php -v`, `nginx -v`, `apache2 -v`, `httpd -v`, `postgres --version`,
`/usr/lib/postgresql/*/bin/postgres --version` (glob left to the remote shell),
`mysqld --version`, `mariadbd --version`, `redis-server --version`, `mongod --version`,
`catalina.sh version`, `go version`. stdout **and** stderr are both read for the version.
Exit 127 / stderr `command not found` / `No such file` means "product absent" (not an error).
Any other non-zero exit, or 255, makes that item `undetermined`.

W22 [choice]: the version args are the same as the W21 command's args for that executable's product.

Windows [choice]: the remote string is `powershell -NoProfile -NonInteractive -Command "<canon string>"`
with the canon text verbatim and `|` unescaped (constants `common::WN1..WN5`):
- WN1 `Get-CimInstance Win32_OperatingSystem | Select-Object Caption,Version,BuildNumber | ConvertTo-Json`
- WN2 `Get-HotFix | Sort-Object InstalledOn -Descending | Select-Object -First 1 HotFixID,InstalledOn | ConvertTo-Json`
- WN3 `Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired'`
- WN4 `Get-NetFirewallProfile | Select-Object Name,Enabled | ConvertTo-Json`
- WN5 `Get-ItemProperty 'HKLM:\System\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp' -Name UserAuthentication | ConvertTo-Json`

[choice] Windows "now" is `HOSTAUDIT_NOW` (no remote clock command is in the whitelist).

## 12.4 External feeds, caches, env seams

| variable | meaning | default when unset |
|---|---|---|
| `HOSTAUDIT_KEV_URL` | CISA KEV feed. `file://` MUST be supported. Body `{"vulnerabilities":[{"cveID":"CVE-..."}, ...]}` | `https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json` |
| `HOSTAUDIT_EOL_URL` | endoflife.date, one fetch per product slug; the literal `{product}` is replaced by the slug. `file://` MUST be supported. Body: JSON array of `{"cycle":"20","eol":"2026-04-30" or true or false,"extendedSupport":"<date>" or true or false (optional),"latest":"..." (optional)}` | `https://endoflife.date/api/{product}.json` |

- Caches [choice]: `~/.hostaudit/kev-cache/` and `~/.hostaudit/eol-cache/` (layout free; EOL keyed
  **by slug**, KEV one entry); both record the fetch time (`HOSTAUDIT_NOW`). They **share**
  `osv_cache_max_age_hours` (default 24, clamp 1..=168) with OSV. Fetch failure: fresh cache, else "unobtainable".
- Only the product slug / the fixed KEV URL leave the machine; EOL fetches happen only for products actually found.
- Fields [choice]: B5 and K2 carry `kev_fetched_at` (unix secs of the KEV data used; absent or null when unobtainable);
  S8, S9, K1, WN1 carry `eol_fetched_at` when endoflife data was used.
- `eol` as a date: expired iff `HOSTAUDIT_NOW` is **after** that day (UTC). `true` = expired, `false` = supported.
  S8 Warning: expiry within 90 days (89 days left: warning, 91: clean).

## 12.5 B5 / K2 CVE evaluation fields

B5 stays one finding per host. It (and K2) additionally carries:

```json
"cves": [ {"id": "UBUNTU-CVE-2024-1234",
           "cve": "CVE-2024-1234",
           "package": "libssl3",
           "severity": "unknown",
           "kev": "yes",
           "fixed": "<version>",
           "exposed": "no",
           "priority": "P2"} ],
"priority_counts": {"P0": 0, "P1": 0, "P2": 1, "P3": 0}
```

- `cve` is a string taken from `id` or `aliases`, or `null` when only an advisory id exists. [choice] `package` = binary package.
- `severity` in `negligible|low|medium|high|critical|unknown`: OSV `severity[]` `type:"Ubuntu"` score if present;
  else `type:"CVSS_V3"` base score computed from the vector (>=9.0 critical, >=7.0 high, >=4.0 medium, >0 low); else `unknown`.
- [choice] `kev` and `exposed` are the strings `yes|no|unknown`; `fixed` is a version string or `"unknown"`;
  `cves` is ordered P0 first.
- `exposed`: `yes` iff a process whose executable is owned (W12b, W13) by a binary package of the CVE's **source**
  package listens on `0.0.0.0` / `[::]` / `*`; `no` iff W12 was observed and no such process; `unknown` iff W12/W12b/W13 failed.
- Priority: canon §3.2 verbatim. KEV unknown never yields P0, and the output states `KEV 不明`.
- Fix text: one `sudo apt update && sudo apt install --only-upgrade <pkgs...>` for all fixable CVEs (exactly one
  `--only-upgrade`); CVEs without a fixed version are listed with the word `回避策`.
- K2 uses the same objects but only for the source package/version of `linux-image-<uname -r>`.

## 12.6 Observation formats and rule details

- **W14**: only lines starting `Inst ` count. "security" = the parenthesised origin list contains `-security`
  (`jammy-security`, `stable-security`). `Conf`/`Remv` lines are ignored. Failure makes S1/K4 undetermined.
  K4 additionally requires the package name to start with `linux-`.
- **S1 with B7**: if B7 is a violation (stale index), S1's text contains `下限`.
- **W15** [canon gap: the real `pro` schema is not pinned by the canon]: top-level `packages` array of
  `{"package","service_name","status"}`; `status` in {`pending_attach`, `pending_enable`} means "fixed only by ESM, ESM off"
  and gives an S2 violation; other statuses are fine. Missing `packages` or non-JSON gives undetermined.
- **W25** [canon gap]: `services` array of `{"name","status"}`; livepatch is enabled iff the `livepatch` entry has
  `status:"enabled"`; no `services` array gives K5 undetermined.
- **Tool missing** (stderr contains `command not found`): `undetermined` with a `reason` containing the install
  command: `sudo apt install ubuntu-pro-client` (S2, K5), `sudo apt install needrestart` (S3), a string containing
  `distro-info` (K1 fallback). An ssh failure is NOT a missing tool (no install hint).
- **W16**: `NEEDRESTART-SVC: <unit>` lines give an S3 violation (units listed in evidence and in `systemctl restart <unit>`);
  `NEEDRESTART-KSTA: 0` is undetermined; no SVC and `KSTA: 1` is clean. (KSTA 2/3: [canon gap], untested.)
- **S5**: official hosts exactly as the canon list; the match is on the URL **host** (lookalikes such as
  `archive.ubuntu.com.evil.example`, `evilarchive.ubuntu.com`, userinfo tricks are not official). `#` lines ignored;
  `deb`/`deb-src` lines and deb822 `URIs:` are parsed. Evidence = the non-official lines/URIs and the file name.
  An absent `sources.list` (`No such file`) counts as no entries. A listed file whose `cat` fails: undetermined.
- **S7**: hold set intersecting (S1 package names union B5-affected binary packages) gives a violation whose fix contains
  `sudo apt-mark unhold <pkg>` only for those; otherwise clean with the hold list in evidence; empty list clean; W20 failure undetermined.
- **S8 items** = W21 version output per product (PATH) + W22 process executables + W23 images. **S9 items** = dpkg packages
  mapped by the §4.1 table to a product (version = dpkg version without epoch, leading dotted number; suffixes such as `nodesource1` ignored).
  An evidence line per item contains the product, the version string and the cycle.
  **[choice]** a W21 (PATH) version whose cycle equals the dpkg package's cycle for the same product is the dpkg binary:
  one S9 item, no S8 item. A different cycle is a separate S8 item (the measured nodejs 20 + node v25 case).
  Docker image: product by the repository name's last path segment; tag = leading dotted version of the tag
  (`7.2-alpine` gives 7.2); no tag / `latest` / digest makes that item undetermined. Images not in the §4.1 table are ignored.
  `mysqld --version` output containing `MariaDB` is a mariadb item, never mysql.
  W23: `command not found` (any exit) or an empty list means no container items; password required / 255 / other non-zero makes S8 undetermined.
- **K1**: slug by `ID` (`ubuntu`, `debian`, `rhel`, `centos`, `amzn` as `amazon-linux`), cycle = `VERSION_ID` (`22.04`, `12`, `2023`).
  The W17 fallback is used only when the endoflife data is unobtainable; the current codename is `VERSION_CODENAME` from W1.
  Warning text contains `延長サポート` and `ESM` or `ELTS`. Fix contains `do-release-upgrade`.
- **K2/K3**: the running package is `linux-image-<uname -r>` in W2. K3 flavour = the suffix after the version part (`-generic`, `-aws`);
  version order = dpkg order (so `5.15.0-100` is newer than `5.15.0-99`). K3 text mentions `K2` when K2 is a violation; K3 fix says `再起動`.
- **K5**: warning iff livepatch is not enabled and K2 has a `high`/`critical` CVE; text mentions `再起動`; fix `sudo pro enable livepatch`.
- **WN1** [canon gap]: slug `windows-server` when `Caption` contains `Server`, else `windows`; the cycle is the entry whose
  `latest` starts with the JSON `Version` (`10.0.20348`). No match: undetermined.
  Warning iff the standard support is over and `extendedSupport` is still in the future; violation iff both are over (`false` = none).
- **WN2** date forms accepted: ISO `YYYY-MM-DDTHH:MM:SS...` and PowerShell-5 `\/Date(<ms>)\/`. More than 45 days: violation; up to 45: clean;
  empty / `null` / unparseable: undetermined. Fix contains `Windows Update`.
- **WN3**: output `True`/`False` (CR/LF tolerated); `True` and WN2 age over 7 days: violation; `True` within 7: clean; `True` with no usable WN2: undetermined;
  other text: undetermined.
- **WN4**: JSON array/object of `{Name, Enabled}`; `Enabled` 0/false = disabled gives a violation (evidence names the profile; fix
  `Set-NetFirewallProfile -Profile <name> -Enabled True`); empty list or garbage: undetermined.
- **WN5**: JSON with numeric `UserAuthentication`; not 1 gives a violation (fix contains `ネットワークレベル認証`); key missing or garbage: undetermined.

## 12.9 Canon amendment 4322e061 (tests: `v11_timeout.rs` and the cases named below)

- **W12b** is `sudo -n LC_ALL=C readlink /proc/<pid>/exe` (corrects the first draft of this section).
- **Unsafe path** (fails the W13 conditions): never sent to ssh. Every item depending on that executable is Undetermined:
  B5/K2 `exposed` = `unknown` (`unsafe_executable_path_makes_exposure_unknown`), and the S8/W22 item
  (`s8_item_of_an_unsafe_executable_path_is_undetermined_and_never_sent`).
- **S7**: a hold exists, no violation is shown, and S1 (W14) or B5 is undetermined: Undetermined. An observed violation still wins.
- **S9** has no warning band: clean before EOL, even within 90 days (`s9_has_no_warning_band_near_eol`). The 90-day warning is S8 only.
- **Command timeout** [choice]: `~/.hostaudit/config.toml` key `remote_command_timeout_secs`, default 60, clamp 5..=600
  (0 becomes 5, never "no limit"). It bounds each remote command's execution (ssh `ConnectTimeout` only bounds the connection).
  A timed-out command makes the rules depending on it `undetermined` with a `reason` containing `時間切れ`; other rules are unaffected.
