# hostaudit — implementation contract (v1, tests: `crates/hostaudit/tests/`)

Canon: `docs/hostaudit-baseline-v1.md` (baseline v1). This file fixes the
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
| 0 | the command ran and every finding is `clean` or `not_applicable` (for `inventory`/`select`/`daily-task`: success) |
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
