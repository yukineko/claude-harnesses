# hostaudit baseline v1（canon — 2026-11-04 承認）

> 状態: 2026-11-04 ユーザー承認（「実装していっていいよ」）。canon。hostaudit の実装・テストはこの文書を契約とする。
> 変更は version bump + 決定ログ（specguard decide）で行う。

## 0. ゴールと位置づけ

- ゴール（ユーザー原文 2026-11-04）: 「まともに設定管理できない管理者が管理しているサーバをきちんと運用できるようにすること。あと自動で監査できるようにすること」「aws loginからaws resourceを管理できるようにもする」
- 実装系監査（daily の cargo deny / scout L2）とは分離した、**設定・稼働系**監査の plugin crate `crates/hostaudit`。
- **監査は常に read-only**。唯一の例外は §5 のセキュリティグループ変更で、それも**既定は人間が手で行う**。

## 1. 消費者（完全列挙）と §7

| 消費者 | 受け取るもの |
|---|---|
| daily 登録 task（1日1回自動） | `hostaudit audit` / `hostaudit local` / `hostaudit aws` の exit code と 1 行要約 |
| overwatch review-queue | Violation / Undetermined の各 finding（既存 `review_findings.jsonl` へ追記。overwatch は改修しない） |
| SessionStart systemMessage | 件数 1 行（ユーザーに見える面） |

- .githooks / pre-commit / pre-push / Stop hook / flow / condukt には**配線しない**。exit code は何もブロックしない（§7: 外部 = OSV・AWS API が落ちても workflow は止まらない）。

## 2. 判定値

| 値 | 意味 | exit |
|---|---|---|
| Clean | 観測し、基準を満たす | 0 |
| NotApplicable | 観測し、対象が無い（例: 443 非 LISTEN、設定ファイル無し）。**Clean と別値**でレポートに明記 | 0 |
| Violation | 観測し、基準を満たさない。修正手順を必ず持つ | 非0 |
| Undetermined | 観測できない（接続失敗・timeout・sudo 失敗・想定外の非0・パース不能・advisory 取得不能・AWS 認証切れ） | 非0 |

全コマンドは `LC_ALL=C` で実行する。

## 3. サーバ（ssh）

### 3.1 対象

- inventory: `~/.ssh/config`（Include を辿る）の非ワイルドカード Host ＋ `--from-cmd`（終了 0 のときだけ stdout 各行）＋ §4 の EC2 一覧。
- 接続はユーザーが `select <alias>...` または `select --all` で選んだ host のみ。選択無し／`--all` が空 inventory に解決 → audit 拒否（非0）。

### 3.2 whitelist（完全列挙。[sudo] は `sudo -n`）

| id | command | 非0 を観測値として読む例外 |
|---|---|---|
| W1 | `cat /etc/os-release` | なし |
| W2 | `dpkg-query -W -f '${Package}\t${Version}\t${source:Package}\t${source:Version}\n'` | なし |
| W3 | `ss -H -tln` | なし |
| W4 [sudo] | `sshd -T` | なし |
| W5 [sudo] | `ufw status` | なし |
| W6 [sudo] | `nft list ruleset` | なし |
| W7 | `stat -c %Y /var/lib/apt/periodic/update-success-stamp` / `stat -c %Y /var/lib/apt/lists`（別呼び出し） | No such file → 「無し」 |
| W8 | `stat -c %Y /var/run/reboot-required` | No such file → 「無し」 |
| W9 | `systemctl is-enabled unattended-upgrades` | stdout が `disabled` / `masked` / `not-found` → その値 |
| W10 | `cat /etc/apt/apt.conf.d/20auto-upgrades` | No such file → 「無し」 |
| W11 | `date +%s` | なし |

### 3.3 baseline ルール

| id | Violation 条件 | 修正手順（Violation 時に出す） |
|---|---|---|
| B1 | W4 `permitrootlogin yes` | `/etc/ssh/sshd_config` の `PermitRootLogin` を `prohibit-password` か `no` に → `sudo sshd -t && sudo systemctl reload ssh` |
| B2 | W4 `passwordauthentication yes` | 同ファイル `PasswordAuthentication no`（鍵ログインを確認してから） |
| B3 | W5 が active でなく、W6 に policy drop の input chain も無い（片方でも Undetermined なら Undetermined） | `sudo ufw allow OpenSSH && sudo ufw enable` |
| B4 | W3 で 3306/5432/6379/27017/9200/11211 が 0.0.0.0 / [::] / * で LISTEN | 該当サービスの bind を 127.0.0.1 に（ファイル名をサービス別に提示） |
| B5 | W2 のソースパッケージ×バージョンが OSV（Ubuntu/Debian）に該当（dpkg のバージョン比較）。OSV にその OS の情報が 0 件 → Undetermined | `sudo apt update && sudo apt install --only-upgrade <pkg>`（advisory ID 併記） |
| B6 | W9 ≠ enabled、または W10 に `APT::Periodic::Unattended-Upgrade "1"` が無い | `sudo apt install unattended-upgrades && sudo dpkg-reconfigure -plow unattended-upgrades` |
| B7 | W11 − max(W7) > 7 日（両方無し → Undetermined） | `sudo apt update`（B6 を直すと継続的に解消） |
| B8 | W8 が存在し W11 − W8 > 7 日 | 計画停止枠で再起動 |
| B9 | 443 LISTEN 時、監査機から HostName:443 の証明書 notAfter ≤ 14 日（443 非 LISTEN → NotApplicable） | 証明書更新（certbot なら `sudo certbot renew`） |

## 4. AWS（read-only 監査）

- 認証は利用者の `aws login`（または既存 profile）。`aws sts get-caller-identity` 失敗 → 全 AWS ルール Undetermined（「ログインが切れている」と表示）。
- whitelist（完全列挙。いずれも Describe/List/Get）:
  AW1 `sts get-caller-identity` / AW2 `ec2 describe-regions` / AW3 `ec2 describe-security-groups`・`ec2 describe-security-group-rules`（sgr ID 取得用。2026-11-04 ユーザー裁定で追加） / AW4 `ec2 describe-instances` /
  AW5 `ec2 describe-network-interfaces` / AW6 `s3api list-buckets`・`get-public-access-block`・`get-bucket-policy-status` /
  AW7 `rds describe-db-instances` / AW8 `iam get-account-summary` / AW9 `cloudtrail describe-trails`・`get-trail-status`
- AW4 の EC2（public IP / private IP）は §3 の inventory に取り込む（ssh 接続は select した場合のみ）。

| id | Violation 条件 | 修正の場所（手動手順） |
|---|---|---|
| A1 | SG の ingress が 0.0.0.0/0 or ::/0 で 22 / 3389 / §3 B4 の DB ポートを許可 | §5 |
| A2 | SG の ingress が 0.0.0.0/0 or ::/0 で全プロトコル（-1）を許可 | §5 |
| A3 | S3 バケットの Public Access Block が全項目 true でない、または policy status が public | コンソール: S3 → バケット → 「アクセス許可」→「ブロックパブリックアクセス」 |
| A4 | RDS `PubliclyAccessible = true` | コンソール: RDS → DB → 「変更」→「接続」→「パブリックアクセス」= なし |
| A5 | `AccountMFAEnabled = 0` または `AccountAccessKeysPresent = 1`（root） | コンソール: IAM → ルートユーザーのセキュリティ認証情報 |
| A6 | logging 中の multi-region trail が無い | コンソール: CloudTrail → 証跡の作成 |
| A7 | EC2 `MetadataOptions.HttpTokens = optional`（IMDSv1 許可） | `aws ec2 modify-instance-metadata-options --instance-id <id> --http-tokens required`（人間が実行） |

## 5. セキュリティグループの変更（唯一の変更操作）

1. **既定 = 手動**。A1/A2 の各 finding に次を**具体的に**出す:
   - リージョン・SG ID・SG 名・アタッチ先（インスタンス ID / Name タグ / ENI）
   - 該当ルール（プロトコル・ポート範囲・CIDR・ルール ID `sgr-…`）
   - コンソールの場所: EC2 → セキュリティグループ → `<sg-id>` → 「インバウンドルール」→「インバウンドルールを編集」→ 該当行
   - 同等の CLI: `aws ec2 revoke-security-group-ingress --region <r> --group-id <sg> --security-group-rule-ids <sgr>`
     （置き換え先 CIDR を指定した場合は `authorize-security-group-ingress` も併記）
2. **自動適用は opt-in**: `hostaudit aws apply --finding <id>` のときだけ、
   (a) 変更前後の差分を表示 → (b) EC2 API の `--dry-run` で権限確認 → (c) finding ごとに人間の明示確認 → (d) 実行 → (e) AW3 で再監査して解消を確認、
   の順で行う。確認が取れない（非対話・自律モード）なら実行しない（自答しない）。対象は SG の ingress ルールの revoke / 置き換えのみ。
3. 22 番を閉じる変更で、**同じ SG の他の ingress ルールが 22 を一切カバーしなくなる**場合は警告して拒否し、置き換え CIDR（`--replace-cidr`）の指定を必須にする（2026-11-04 ユーザー裁定。同一インスタンスの他 SG は見ない）。

## 6. ローカル設定

- 対象: `~/.claude/settings.json`, `~/.claude/settings.local.json`, `<cwd>/.claude/settings.json`, `<cwd>/.claude/settings.local.json`。無し → NotApplicable、読めない/JSON 不正 → Undetermined。
- L1 `permissions.allow` に `Bash` / `Bash(*)` / `Bash(:*)` / `*` / L2 `permissions.defaultMode == "bypassPermissions"` / L3 `skipDangerousModePermissionPrompt == true`。

## 7. 閾値（既定値 / clamp）

| 名前 | 既定 | clamp |
|---|---|---|
| ssh ConnectTimeout | 10 秒 | 1..=60 |
| OSV キャッシュ鮮度上限 | 24 時間 | 1 時間..=7 日 |
| apt index 鮮度 | 7 日 | 固定 |
| reboot-required 放置 | 7 日 | 固定 |
| TLS 期限警告 | 14 日 | 固定 |

## 8. レポートと secret

- `~/.hostaudit/reports.jsonl` に target・rule・verdict・evidence（whitelist 出力の該当行のみ、各 512 byte 以下）・修正手順・時刻・advisory 取得時刻・apt index 時刻。
- secret 除去: `-----BEGIN` を含む行は除外、settings の env 値は `<redacted>`、40 文字以上の `[A-Za-z0-9_/+=-]` 連続はその部分だけ `<redacted>`（行は残す）。AWS のアクセスキー・セッショントークンは記録しない。
- 外部へ送るもの: OSV へのソースパッケージ名/バージョン、TLS 接続、AWS API（Describe/List/Get、§5 の apply のみ変更系）。
