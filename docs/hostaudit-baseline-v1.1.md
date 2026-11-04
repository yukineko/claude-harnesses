# hostaudit baseline v1.1（canon — 2026-11-04 承認）

> 状態: 2026-11-04 ユーザー承認（草案に対し「あげた通りにすすめて」「草案を承認するのですすめて」、個別裁定は §9）。canon。
> v1（`docs/hostaudit-baseline-v1.md`）への**追加と変更**だけを書く。ここに書いていないことは v1 のまま。v1 と食い違う箇所はこの文書が優先する。
> 原則は v1 と同じ: サーバでは read-only、判定不能は Undetermined、外部情報は advisory（workflow を止めない）、接続は select した host のみ。

## 0. ゴール（v1 に追加）

- 「CVE や他のセキュリティ警告についての対応がほしい。apt でチェックできたりする項目もあるので、それを確認できるようにしてほしい」
- 「security patch すら提供されない version の処理系、ミドルウェアなどを使いつづけているものまであるから、それを指摘しないといけない」
- 「ディストリビューションのレベルからありえるし Kernel のパッチだって含めるべき」「Windows の version とかも」
（いずれもユーザー原文 2026-11-04）

## 1. 判定値の追加: Warning（v1 §2 を変更）

| 値 | 意味 | exit |
|---|---|---|
| Clean / NotApplicable / Violation / Undetermined | v1 §2 のまま | v1 のまま |
| **Warning**（新設） | 観測し、基準は満たすが人間の確認・計画が要る（意図的なこともある非公式リポジトリ、期限が近い、延長サポートのみ、など）。**助言（fix 欄）を必ず持つ** | **0** |

- exit は Violation / Undetermined が 1 件でもあれば非0、そうでなく Warning があっても 0（2026-11-04 ユーザー裁定）。
- ただし Warning を**沈黙させない**: テキスト出力・`--json`・`reports.jsonl`・daily の 1 行要約・SessionStart の件数行のすべてで、warning の件数を clean とは**別に**出す（例 `violation 2 / undetermined 1 / warning 3 / clean 40`）。
- overwatch review-queue へは送らない（送るのは v1 どおり Violation / Undetermined のみ）。
- 1 ルールが複数の項目（CVE、製品など）をまとめて 1 finding になる場合の verdict は、項目の最悪値: Violation > Undetermined > Warning > Clean。項目が 0 件なら NotApplicable（ルールごとに下で定める）。

## 2. 追加 whitelist（すべて read-only。[sudo] は `sudo -n`、すべて `LC_ALL=C`）

| id | command | 非0 を観測値として読む例外 |
|---|---|---|
| W0 | `cmd /c ver`（W1 が非0 のときだけ実行。OS 判定用、§7） | なし |
| W12 [sudo] | `ss -H -tlnp` | なし |
| W12b [sudo] | `readlink /proc/<pid>/exe`（pid は W12 の `pid=<数字>` から取った**数字のみ**。それ以外は実行しない） | なし |
| W13 | `dpkg -S <path>`（path は W12b の出力で、`/` 始まり・空白／引用符／`;|&$\`` を含まないものに限る） | `no path found` → 「dpkg 管理外」 |
| — | （W13 の条件を満たさない path は ssh へ渡さない。その実行ファイルに関わる項目〈§3.1 の外部公開、S8/W22〉は Undetermined） | — |
| W14 | `apt-get -s -o Debug::NoLocking=1 dist-upgrade` | なし |
| W15 | `pro security-status --format json`（Ubuntu のみ） | command not found → 「ツール未導入」 |
| W16 [sudo] | `needrestart -b -r l` | command not found → 「ツール未導入」 |
| W17 | `ubuntu-distro-info --supported`（Ubuntu）/ `debian-distro-info --supported`（Debian） | command not found → 「ツール未導入」 |
| W18 | `cat /etc/apt/sources.list` ＋ `ls /etc/apt/sources.list.d` ＋ 列挙された各 `*.list` / `*.sources` の `cat`（ファイル名は ls 出力のうち `[A-Za-z0-9._-]+` に一致するものだけ） | sources.list の No such file → 「無し」 |
| W19 | `dpkg --audit` | なし |
| W20 | `apt-mark showhold` | なし |
| W21 | 製品ごとの固定 version コマンド（§4.1 の表。存在するものだけが答える） | exit 127 / `command not found` / `No such file` → 「その製品は無い」 |
| W22 | W12b で得た実行ファイルのうち、basename が §4.1 の表にあるものに対する同表の version 引数 | 同上 |
| W23 [sudo] | `docker ps --format '{{.Image}}'` | docker 無し（exit 127 / command not found）→ 「docker 無し」 |
| W24 | `uname -r` | なし |
| W25 | `pro status --format json`（Ubuntu のみ、K5 用） | command not found → 「ツール未導入」 |
| WN1〜WN5 | §7 の固定 PowerShell | §7 |

- **ツール未導入**（pro / needrestart / distro-info）のサーバは、そのルールを NotApplicable ではなく **Undetermined**（「ツール未導入のため確認不能」＋導入コマンド `sudo apt install ubuntu-pro-client` / `needrestart` / `distro-info`）にする（2026-11-04 裁定）。
- **1 コマンドの実行時間上限**: ssh の ConnectTimeout は接続だけを縛り、リモートコマンドの実行時間を縛らない。v1.1 の各コマンドは既定 60 秒（clamp 5..=600）で打ち切り、打ち切ったら Undetermined（「時間切れ」）。`pro security-status` が監査機上で 120 秒以上戻らなかったことを 2026-11-04 に観測したため。
- Debian の `debsecan` は v1.1 では使わない（サーバからネットワーク取得するため）。

## 3. CVE 評価（v1 B5 の拡張）

B5 は v1 どおり **host ごとに 1 finding**（該当あり → Violation）。v1.1 ではその finding に、該当した CVE ごとの評価と優先度を付ける。

### 3.1 各 CVE の項目

| 項目 | 取得元 | 取れないとき |
|---|---|---|
| CVE ID | OSV レコードの `id` / `aliases` のうち `CVE-` で始まるもの（`UBUNTU-CVE-2024-1234` → `CVE-2024-1234`） | advisory ID のみ |
| 深刻度 | OSV `severity[]` の `type: "Ubuntu"` の score（negligible/low/medium/high/critical）。無ければ `type: "CVSS_V3"` のベクトルから基本スコアを計算して ≥9.0 critical / ≥7.0 high / ≥4.0 medium / >0 low | `unknown`（low 扱いにしない） |
| 悪用の実績（KEV） | CISA KEV の JSON（`vulnerabilities[].cveID`）をローカルで取得して照合 | `kev: unknown`（「KEV 不明」と明示） |
| 修正版 | OSV の `fixed` ＋ W14 の候補版 | `unknown` |
| 外部公開 | W12 でその**ソースパッケージ**に属する実行ファイル（W12b → W13 → W2 の source）が 0.0.0.0 / [::] / * で LISTEN | `unknown` |

### 3.2 優先度（決定論。2026-11-04 承認のまま）

| 優先度 | 条件 |
|---|---|
| P0 | KEV に載っている |
| P1 | 深刻度 critical / high **かつ** 外部公開 yes または unknown |
| P2 | 深刻度 critical / high で外部公開 no、または medium、**または深刻度 unknown** |
| P3 | low / negligible |

- KEV が unknown のときは P0 判定ができないので、残りの条件で P1〜P3 を付けたうえで「KEV 不明」を併記する（優先度を下げた結論として扱わない）。
- 修正版が無い CVE は「回避策が必要」と明記し、修正コマンドの代わりに対象サービスの停止・bind 変更・FW 制限を提示する。
- 出力は host ごとの優先度別件数 ＋ CVE 一覧。修正は**パッケージ単位でまとめる**（1 回の `sudo apt install --only-upgrade a b c` で複数 CVE が解消する形）。
- EPSS は入れない（2026-11-04 裁定）。

## 4. apt / OS のセキュリティ警告（新ルール S1–S9）

| id | 条件 → 判定 | 修正手順・助言 |
|---|---|---|
| S1 | W14 に `-security` を含む取得元の `Inst` 行がある（セキュリティ更新が未適用）→ **Violation**。B7 が Violation（index が古い）なら「件数は下限」と明記。`Inst` 行が無い → Clean | `sudo apt update && sudo apt upgrade`（対象パッケージを列挙） |
| S2 | W15 で「ESM でしか修正されない、ESM 未有効のため適用できない」パッケージがある → **Violation**。無い → Clean。JSON が想定スキーマでない → Undetermined。Ubuntu 以外 → NotApplicable | `sudo pro attach <token>` / ESM 有効化（または後継リリースへの移行計画） |
| S3 | W16 に `NEEDRESTART-SVC:` 行がある（古いライブラリのまま動くサービス）→ **Violation**。`NEEDRESTART-KSTA:` が 0（不明）→ Undetermined。どちらも無く KSTA=1 → Clean（カーネルは K3 が見る） | `sudo systemctl restart <svc>`（サービスを列挙）／計画再起動 |
| S5 | W18 に公式リポジトリ以外がある → **Warning**（意図的な導入もあるため。一覧を提示して確認を促す）。公式のみ → Clean | 一覧と「意図したものか確認。不要なら該当ファイルを削除」 |
| S6 | W19 が何か出力する → **Violation**。空 → Clean | `sudo dpkg --configure -a` |
| S7 | W20 に、S1 の対象または B5 の該当パッケージが含まれる → **Violation**（hold のせいで修正できない）。hold があり、S1（W14）か B5 が Undetermined で Violation が出ていない → Undetermined。hold が無い、または hold が両方の観測結果と重ならない → Clean（hold 一覧は evidence に出す） | hold の理由を確認 → `sudo apt-mark unhold <pkg>` |
| S8 | **dpkg 管理外**（W21/W22/W23 で見つかり、W13 で dpkg 管理外、または W23 のコンテナ）の処理系・ミドルウェアについて、版のサイクルが upstream のサポート期限切れ → **Violation**（「security patch が提供されない版」）。期限まで 90 日以内 → **Warning** | 後継のサポート版への移行手順（例 `mysql:5.7` → 8.0 / 8.4 LTS） |
| S9 | **apt（dpkg）で入っている**処理系・ミドルウェアについて、版のサイクルが upstream のサポート期限切れ → **Violation**（2026-11-04 裁定で warning から変更。ディストリのバックポートの範囲は S2 を参照と併記） | 後継版への移行手順 |

- S4（リリースのサポート期限）は §5 の **K1 に置き換える**。
- S9 には期限接近の Warning 帯を設けない（承認済み草案どおり。期限前は Clean）。期限接近の Warning は S8（dpkg 管理外）だけ。
- S5 の公式リポジトリ（固定）: `archive.ubuntu.com`, `*.archive.ubuntu.com`, `security.ubuntu.com`, `ports.ubuntu.com`, `esm.ubuntu.com`, `deb.debian.org`, `security.debian.org`, `*.ec2.archive.ubuntu.com`（AWS 内ミラー）。
- S8/S9 は host ごとに各 1 finding（項目 = 製品×版×出所）。どの出所にも製品が 1 つも見つからない → NotApplicable。版が取れない（コマンド失敗・パース不能・docker image の tag が `latest`／無し／digest のみ）、製品が endoflife.date に無い、取得できない → その項目は Undetermined（合格にしない）。
- 同じ製品が dpkg と PATH 上で別の版（例: dpkg の nodejs 20 と PATH の node v25）なら、**両方を別項目**として出す（2026-11-04 実測で起きた事例）。

### 4.1 監視する製品（初期リスト。2026-11-04 ユーザー回答「このマシンにはある」と実測に基づく）

| 製品（endoflife.date の slug） | W21 / W22 の version コマンド | dpkg パッケージ名の写像 | docker image 名 |
|---|---|---|---|
| nodejs | `node --version` | `nodejs` | `node` |
| python | `python3 --version` / `python2 --version` | `python<X.Y>` | `python` |
| java（Eclipse Temurin / OpenJDK の LTS サイクル） | `java -version`（stderr） | `openjdk-<N>-*` | `openjdk` / `eclipse-temurin` |
| ruby | `ruby -v` | `ruby<X.Y>` | `ruby` |
| docker-engine | `docker --version` | `docker.io` / `docker-ce` | — |
| containerd | `containerd --version` | `containerd` / `containerd.io` | — |
| openssl | `openssl version` | `openssl` / `libssl<N>` | — |
| php | `php -v` | `php<X.Y>-*` | `php` |
| nginx | `nginx -v`（stderr） | `nginx` | `nginx` |
| apache-http-server | `apache2 -v` / `httpd -v` | `apache2` | `httpd` |
| postgresql | `postgres --version`（PATH 外は `/usr/lib/postgresql/*/bin/postgres --version`） | `postgresql-<N>` | `postgres` |
| mysql | `mysqld --version` | `mysql-server-<X.Y>` / `mysql-server-core-<X.Y>` | `mysql` |
| mariadb | `mariadbd --version` / `mysqld --version` の出力に `MariaDB` | `mariadb-server*` | `mariadb` |
| redis | `redis-server --version` | `redis-server` | `redis` |
| mongodb | `mongod --version` | `mongodb-org-server` | `mongo` |
| tomcat | `catalina.sh version` | `tomcat<N>` | `tomcat` |
| go | `go version` | `golang-<X.Y>-go` | `golang` |

- stdout と stderr の両方を版の読み取りに使う（java / nginx は stderr に出す）。
- サイクルの照合: endoflife.date のサイクル文字列（`20`, `3.12`, `8.0` など）のうち、版の先頭からドット境界で一致する**最長**のもの。一致なし → Undetermined。
- `eol` が日付 → その日を過ぎていれば期限切れ。`eol: true` → 期限切れ、`false` → サポート中。
- openssh は endoflife.date に対象が無く、脆弱性は B5（CVE 評価）で見るので S8/S9 の対象にしない。
- 表に無い製品（Elasticsearch、Memcached、Perl など）は v1.1 では追加しない。追加は canon 改訂で行う。

## 5. ディストリビューション・カーネル（新ルール K1–K6）

| id | 観測 | 条件 → 判定 | 修正手順・助言 |
|---|---|---|---|
| K1 | W1 ＋ endoflife.date（`ubuntu` / `debian` / `rhel` / `centos` / `amazon-linux`）。取れなければ W17 | 標準サポート（`eol`）も延長（`extendedSupport`）も切れ → **Violation**。標準切れ・延長のみ → **Warning**（「延長サポート中（ESM／ELTS の契約が要る）」）。サポート中 → Clean。endoflife.date が取れず W17 だけ → supported に現在のコードネームがあれば Clean、無ければ Violation。両方取れない → Undetermined | リリースアップグレード計画（`do-release-upgrade` は人間が計画的に実行） |
| K2 | W24 ＋ W2 ＋ OSV | **動いているカーネル**: W2 で `linux-image-<W24>` のソースパッケージ／ソース版を引き、§3 と同じ OSV 照合・CVE 評価をする。該当 → **Violation**。該当なし → Clean。W2 に `linux-image-<W24>` が無い → Undetermined | 新しいカーネルの導入と再起動 |
| K3 | W24 ＋ W2 | インストール済み `linux-image-*` のうち、動作中と同じ flavour（`-generic`, `-aws` など末尾）で最新（dpkg の版順）のものが動作中のものより新しい（再起動待ち）→ **Violation**（K2 の脆弱性が残ることを併記）。同じ → Clean | 計画停止枠で再起動 |
| K4 | W14 | `Inst linux-` 行のうち取得元に `-security` を含むもの（カーネルのセキュリティ更新が未適用）→ **Violation**。無い → Clean | `sudo apt update && sudo apt upgrade` → 再起動 |
| K5 | W25 | livepatch が `enabled` でなく、K2 に high / critical がある → **Warning**（「再起動まで無防備」）。K2 に high / critical が無い → Clean。ツール未導入 → Undetermined（§2）。Ubuntu 以外 → NotApplicable | `sudo pro enable livepatch` |
| K6 | — | カーネル flavour（古い HWE など）自体のサポート切れ。**v1.1 では保留**（下記） | — |

- K2 は「インストール済み」ではなく「**動いている**」版で判定する。
- **K6 は保留**: 草案で承認されたが、flavour ごとのサポート期限を決定論的に得る情報源（endoflife.date の該当製品や OS 内データ）が canon 化の時点で特定できていない。判定条件を推測で作らず、v1.1 では finding を出さない。情報源を決めて canon を改訂してから実装する（2026-11-04、要ユーザー裁定として報告済み）。
- Ubuntu / Debian 以外の Linux（RHEL 系など）は v1.1 では K1 のみ判定し、K2〜K5・S1〜S9 は Undetermined（「この OS の判定は未実装」）とする。

## 6. 外部情報源（v1.1 で増えるもの）

| 情報源 | 用途 | 鮮度上限 | 取れないとき |
|---|---|---|---|
| OSV（v1 から） | 該当判定・深刻度・修正版（B5 / K2） | 24h（v1 §7 の clamp） | B5 / K2 Undetermined |
| CISA KEV | 悪用実績（§3） | 24h（同じ clamp） | `kev: unknown`（B5 自体の判定は変えない） |
| endoflife.date（製品ごと） | S8 / S9 / K1 / WN1 のサポート期限 | 24h（同じ clamp） | その項目 Undetermined（K1 は W17 にフォールバック） |

- いずれもローカルにキャッシュし、取得時刻をレポート行に残す（v1 の `advisory_fetched_at` と同様）。外部へ送るのは製品 slug と既定の公開 URL への GET だけで、host 名や版は送らない。
- 取得に失敗しても workflow は止めない（v1 §1 の §7 方針のまま）。

## 7. Windows（新ルール WN1–WN5）

- 2026-11-04 ユーザー回答: 「今は無い・草案どおり実装」。Windows サーバは現時点で存在しないが、将来のために実装する。
- 接続: Windows の OpenSSH Server 経由で、固定の**読み取り専用 PowerShell** だけを実行（`powershell -NoProfile -NonInteractive -Command "<固定文字列>"`）。WinRM は使わない（ssh と同じ select・whitelist に乗せるため）。
- OS 判定: W1 が非0 で、W0 の出力に `Microsoft Windows` を含む → Windows。Windows host では B*/S*/K* は **NotApplicable**、Linux host（W1 が読めた）では WN* は **NotApplicable**。どちらとも判定できない → 全ルール Undetermined。

| id | 固定コマンド（`-Command` に渡す文字列） | 条件 → 判定 | 修正手順・助言 |
|---|---|---|---|
| WN1 | `Get-CimInstance Win32_OperatingSystem \| Select-Object Caption,Version,BuildNumber \| ConvertTo-Json` ＋ endoflife.date（`windows-server` / `windows`） | 版・ビルドのサポート切れ → **Violation**。標準切れで延長（ESU）期間内 → **Warning**（ESU 契約の有無は観測できないため） | 後継版への移行計画 |
| WN2 | `Get-HotFix \| Sort-Object InstalledOn -Descending \| Select-Object -First 1 HotFixID,InstalledOn \| ConvertTo-Json` | 最後の更新適用から 45 日超（月例更新を 1 回以上飛ばしている）→ **Violation**（2026-11-04 裁定で 45 日確定）。hotfix が 0 件・日付が読めない → Undetermined | Windows Update の実行 |
| WN3 | `Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired'` | `True` かつ WN2 の最新適用から 7 日超 → **Violation**（再起動待ちの放置）。`True` で 7 日以内 → Clean。`False` → Clean | 計画停止枠で再起動 |
| WN4 | `Get-NetFirewallProfile \| Select-Object Name,Enabled \| ConvertTo-Json` | いずれかのプロファイルが無効 → **Violation** | `Set-NetFirewallProfile -Profile <name> -Enabled True`（人間が実行） |
| WN5 | `Get-ItemProperty 'HKLM:\System\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp' -Name UserAuthentication \| ConvertTo-Json` | `UserAuthentication` ≠ 1（RDP の NLA 無効）→ **Violation**（AWS の A1 で 3389 公開も併せて指摘） | システムのプロパティ →「リモート」→「ネットワークレベル認証でリモートデスクトップを実行しているコンピューターからのみ接続を許可する」 |

- WN3 の「7 日」は、レジストリキーに作成時刻が無いため WN2 の最新適用日を起点にする [canon 化時の選択]。

## 8. 閾値（v1 §7 に追加）

| 名前 | 既定 | clamp |
|---|---|---|
| KEV / endoflife.date キャッシュ鮮度上限 | 24 時間 | OSV と同じ設定値・同じ clamp（1 時間..=7 日） |
| S8 の期限接近 Warning | 90 日 | 固定 |
| WN2 更新未適用 | 45 日 | 固定 |
| WN3 再起動待ち放置 | 7 日 | 固定 |

## 9. 裁定の記録（2026-11-04、すべてユーザー原文の要旨）

1. 優先度 §3.2 は案どおり。2. EPSS は入れない。3. S5 は Warning。4. ツール未導入は Undetermined。
5. apt で入っている upstream EOL 版は **Violation**（S9）。
6. docker 利用は未確認。W23 は「docker があれば監査、無ければ NotApplicable」のまま残し、実在は監査結果で判明させる。
7. 監視製品は「このマシンにはある」→ このマシンの実測（Ubuntu 24.04.4、nodejs 20.20.2 nodesource ＋ PATH 上 node v25.6.0、openjdk-21、python3.12 / 3.14（PPA）、ruby3.2、docker.io 29.1.3、containerd 2.2.1、openssh-server 9.6p1、openssl 3.0.13）から §4.1 を作る。草案承認で確定。
8. Windows は今は無い。草案どおり OpenSSH 経由で実装する。
9. WN2 の閾値は 45 日。
10. Warning は新しい判定値として足し、exit は 0。件数は必ず別に出す（§1）。
11. K6 は情報源未特定のため保留（§5、要裁定）。
12. canon 化後の機械的な補正（2026-11-04、テスト作成で判明。いずれも既存裁定の範囲内）: W12b は §2 の表どおり [sudo]（`sudo -n LC_ALL=C readlink …`。root 所有プロセスの exe は root でしか読めないため。CONTRACT 初版が sudo 無しで書いていたのを訂正）／W13 の条件を満たさない path は Undetermined／S7 の Undetermined 条件／S9 に Warning 帯なし／コマンド実行時間上限。

## 10. このマシンで v1.1 を当てたときに出ると予想されるもの（未観測の予測。実装後に実測で確かめる）

- S9: dpkg の nodejs 20（nodesource）は upstream のサポート期限（2026-04-30）を過ぎているので Violation になるはず。
- S8: PATH 上の node v25 は奇数版（短期サポート）なので、期限切れか期限接近のはず。
- S5: python3.14 の PPA と nodesource は公式外として Warning になるはず。
