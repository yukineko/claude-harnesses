# hostaudit

サーバ（ssh 経由）・AWS アカウント・ローカルの Claude Code 設定を固定 baseline で監査し、
違反ごとに修正手順を出す設定・稼働系セキュリティ監査。

- canon（契約）: [`docs/hostaudit-baseline-v1.md`](../../docs/hostaudit-baseline-v1.md)
- テスト可能なインターフェース（CLI・exit code・JSON・環境変数 seam）: [`CONTRACT.md`](CONTRACT.md)

## 判定値と exit code

各ルールの判定は次の 4 値のどれか。

- `clean`
- `not_applicable`: 観測したが対象が無い。clean とは別の値として扱う。
- `violation`: 必ず `fix` を持つ。
- `undetermined`: 観測できなかった（接続失敗・sudo 失敗・想定外の非0・パース不能・
  advisory 取得不能・AWS ログイン切れ）。clean として報告することは無い。

| exit | 意味 |
|---|---|
| 0 | 全 finding が clean / not_applicable |
| 1 | violation か undetermined が 1 件以上 |
| 2 | usage error |
| 3 | 拒否（選択無し・未知 alias・確認無し 等） |
| 4 | inventory を判定できない（ssh config 読めない・`--from-cmd` 失敗・AWS ログイン切れ） |

## 使い方

```sh
hostaudit inventory [--json] [--from-cmd CMD] [--aws]
hostaudit select web1 db1            # または hostaudit select --all
hostaudit audit [--json]             # B1–B9（select した host のみ）
hostaudit local [--json]             # L1–L3
hostaudit aws [--json]               # A1–A7（read-only）
hostaudit aws apply --finding A1:<region>:<sg>:<sgr> --confirm <sgr> [--replace-cidr <cidr>]
hostaudit daily-task --print         # ~/.daily/config.toml 用 stanza
```

結果の行き先は 3 つ。

- `~/.hostaudit/reports.jsonl`: 毎回、finding ごとに 1 行追記する。
- overwatch の `review_findings.jsonl`: violation と undetermined を、overwatch 自身の
  ライブラリ経由で `unverified` として追記する。
- SessionStart hook: 最新の件数を 1 行で表示する。レポートがまだ無いときは「UNKNOWN」と出す。

閾値は `~/.hostaudit/config.toml` で設定する。

| キー | 既定値 | clamp |
|---|---|---|
| `ssh_connect_timeout_secs` | 10 | 1..=60 |
| `osv_cache_max_age_hours` | 24 | 1..=168 |

## read-only の保証

- 監査は read-only。
  - ssh で送れるのは whitelist W1–W11 の 12 コマンドだけ（closed enum、`src/ssh.rs`）。
    `sudo -n` を使うのは `sshd -T` / `ufw status` / `nft list ruleset` だけ。
  - AWS で呼べるのは Describe/List/Get の AW1–AW9 だけ（closed enum、`src/aws.rs`）。
  - 任意のコマンド文字列を組み立てる経路は無い。
- ssh に渡すのは `select` した host だけ。`BatchMode=yes` と接続 timeout を付け、
  呼び出し全体にも時間上限を掛ける。
- 外部へ出るのは次の 3 つ。
  - OSV へのソースパッケージ名とバージョン
  - 監査対象の 443 番への TLS 接続
  - AWS API
- secret の扱い。
  - `-----BEGIN` を含む行は除外する。
  - 40 文字以上のトークン文字の連続は `<redacted>` に置き換える。
  - settings の env 値と AWS 認証情報は記録しない。

## セキュリティグループの修正は既定で手動

A1/A2 の各 finding には次を出す。

- リージョン・SG ID・SG 名
- アタッチ先（インスタンス・Name タグ・ENI）
- 該当ルール（`sgr-…`）
- コンソールの場所（EC2 → セキュリティグループ → `<sg>` → 「インバウンドルール」→「インバウンドルールを編集」）
- 同等の `aws ec2 revoke-security-group-ingress`

修正は人間が手で行う。

唯一の opt-in 例外が `hostaudit aws apply`。対象は ingress ルール 1 本の revoke か置き換えだけで、
次の順に実行する。

1. 差分を表示する。
2. `--dry-run` で権限を確認する。
3. finding ごとに明示確認を取る（`--confirm <sgr-id>`、または TTY で sgr-id を入力）。
4. 置き換えルールを authorize する（revoke より先）。
5. revoke する。
6. 再監査する。

次の場合は実行を拒否する。

- `CLAUDECODE` が設定されている（自律モード）。
- 非 TTY で `--confirm` が無い。
- 同じ SG の他のルールが tcp/22 を一切許可しなくなるのに `--replace-cidr` が無い。

## テストで確かめていないこと

テストは fake を使うので、実際の既定経路はテストしていない。

- 実 `ssh` / 実 `aws`
- https の OSV 取得（`curl -fsSL` と OSV の `<ecosystem>/all.zip` に対する `unzip -p`）
- 組み込み TLS probe（`openssl s_client` | `openssl x509 -noout -enddate`）
- TTY での対話確認
- overwatch review-queue への追記
- SessionStart の 1 行
