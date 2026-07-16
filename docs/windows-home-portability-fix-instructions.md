# 指示文書: `HOME`環境変数がWindowsで無視される移植性バグの修正

`docs/loop-engineering-followup-instructions.md`の2タスク（adversarial verify SKILL配線、
delegation記録ゲート）を実装後にローカル(Windows)で検証した際に見つかった、今回の実装ロジックとは
無関係な既存バグ。次に着手するClaude Codeセッションへの実行指示。

## 発見の経緯

`cargo test -p autoflow`を実行したところ、Windows環境で次の6件がfailした（`--test-threads=1`でも
再現するため並行性由来ではないことを確認済み）:

- `condukt::tests::has_completed_tasks_true_when_a_task_is_verified`（今回追加、Tier 2）
- `condukt::tests::has_completed_tasks_true_when_a_task_is_failed`（今回追加、Tier 2）
- `delegation_audit::tests::flow_driven_and_completed_and_no_record_returns_true`（今回追加、Tier 2）
- `lock::tests::live_pid_lock_is_active_stale_is_not`（既存・今回のタスクと無関係）
- `lock::tests::this_session_holds_lock_matches_owner_only`（既存・今回のタスクと無関係）
- `tests::precompact_writes_marker_only_when_this_session_holds_lock`（既存・今回のタスクと無関係）

新規テストと既存テストが同時に、同じ原因でfailしていることから、今回のTier 2実装のロジック不備では
なく、このrepo既存の環境依存バグと判断した（CIはおそらくLinux/macOSで動いているため顕在化して
いない）。

## 根本原因

`crates/harness-core/src/config.rs:8-10`:

```rust
pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}
```

`dirs`クレートの`home_dir()`はWindowsでは`HOME`環境変数を見ず、`USERPROFILE`/
`SHGetKnownFolderPath`ベースで解決する。一方、このrepoのテストには`test_home_guard()` +
`std::env::set_var("HOME", tmpdir)`という共通パターン（`crates/autoflow/src/lock.rs`ほか）があり、
「`HOME`をテスト用一時ディレクトリに差し替えてstateを隔離する」ことを前提にしている。Unix上では
`HOME`が正の情報源なのでこのパターンは機能するが、Windows上では`dirs::home_dir()`がこの差し替えを
無視して実ユーザーのプロファイル（本物の`~/.condukt`等）を読みに行ってしまい、テストが期待する
隔離された状態が見えず失敗する。

`crates/autoflow`の該当モジュール（`condukt.rs`/`compass.rs`/`insights.rs`/`backlog.rs`/
`config.rs`/`lock.rs`）はいずれも`harness_core::config::home`/`base_dir`に正しく委譲しており、
autoflow自身が`dirs::home_dir()`を直接呼んでいる箇所は無い。したがって**修正は`harness-core`の
`home()`1箇所で足りる**。

## 修正内容

1. `crates/harness-core/src/config.rs`の`home()`を、`HOME`環境変数を優先するよう変更する:
   ```rust
   pub fn home() -> PathBuf {
       if let Ok(h) = std::env::var("HOME") {
           if !h.is_empty() {
               return PathBuf::from(h);
           }
       }
       dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
   }
   ```
   - Unix上の挙動は変わらない（`HOME`は既にOSが設定する正の情報源）。
   - 通常の対話シェル（Git Bash含む、このrepoのSKILL.mdはbashスクリプトを多用する）では
     `HOME`が既に実プロファイルと一致する値でセットされていることが多いため、実運用時の挙動は
     従来と実質変わらない。差が出るのはテストで明示的に`HOME`を上書きした場合のみ。
2. 関数のdocコメントに優先順位（`HOME`env var > `dirs::home_dir()`）を明記する。

## 受け入れ基準

- [x] `home()`が`HOME`環境変数を優先し、未設定/空文字の場合のみ`dirs::home_dir()`にフォール
      バックするよう変更されている（`2382201`）。
- [x] `cargo test -p autoflow`で、発見時にfailしていた6件のうち5件がPASSすることを確認した。
      残る1件（`lock::tests::live_pid_lock_is_active_stale_is_not`）は本バグとは別原因と判明
      （下記「検証結果」参照・受け入れ基準としては対象外に修正）。
- [x] `cargo test -p harness-core`の既存テスト（`base_dir_is_dotprefixed_under_home`等）は
      無変更でPASSしている。ただし別原因の既存fail 2件を新たに検出した（下記「検証結果」参照）。
- [ ] `cargo test --workspace`全体での確認は未実施（`harness-core::config::home`は全プラグイン
      バイナリに静的リンクされる共有ライブラリのため、影響範囲が広い。次のセッションで実施）。
- [x] `harness-core`のversionをmicro bump済み（0.1.1→0.1.2）。`marketplace.json`に
      `harness-core`のエントリが無いこと（build-only共有ライブラリで配布対象プラグインでは
      ない）を確認済み。`check-plugin-versions.py`は39プラグイン全てconsistentのままPASS。

## スコープ外（見つかったが今回は対応しない）

- **`crates/fugu-router/src/pathutil.rs`の既存3件のテスト失敗**（`strips_repo_root_from_absolute_path`
  等）。これは別原因（`git log`で確認済み、今回のセッションより前の`5bd38ee`等のコミットに由来し、
  Unix形式の絶対パスリテラル`/no/such/repo/root/...`をテストfixtureに使っていることが疑われる）で、
  `HOME`/`dirs::home_dir()`の問題とは無関係。原因を未調査のため本ドキュメントでは扱わない。
  別タスクとして切り出すこと。
- **`dirs::home_dir()`を直接呼んでいる25ファイル**（`grep -rln "dirs::home_dir" --include="*.rs"`で
  確認可能。主に各crateの`src/install.rs`）。`harness_core::config::home()`を経由しない重複実装。
  現状これらは`test_home_guard`のようなHOME差し替えテストパターンを使っていないため今回のバグとして
  顕在化していないが、将来同じ問題を踏む可能性がある。影響範囲を広げすぎないよう今回は対応しない
  （`home()`同様に`HOME`を優先させたい場合は、各ファイルを`harness_core::config::home()`呼び出しに
  置き換える別タスクとして扱う）。

## 検証結果・追加で見つかった論点(2026-07-16、`2382201`実装後の確認セッション)

`home()`修正（`2382201`）を`cargo test -p harness-core -p autoflow`（`--test-threads=1`でも再現性を
確認）で検証した結果、次の2点が新たに判明した。

1. **「発見の経緯」で挙げた6件のうち、5件は本バグの修正で解消したが、`lock::tests::
   live_pid_lock_is_active_stale_is_not`だけは別原因と判明した。** `crates/autoflow/src/lock.rs`の
   `pid_alive()`はLinuxで`/proc/<pid>`を見た後、それ以外のOSでは`kill -0 <pid>`をサブプロセスとして
   実行して生存確認する実装になっており、Windowsには同じ意味論の`kill -0`が無いため常に失敗する。
   `HOME`/`dirs::home_dir()`とは無関係な、プロセス生存確認自体のWindows非対応が原因（「発見の経緯」
   での「同じ原因でfailしている」という当初の診断はこの1件については誤りだった、と訂正する）。
   これは実運用にも影響しうる別バグ（Windows環境でロックの生存確認が常に`false`になり、autoflowが
   他プロセスによる駆動中を検知できない）で、別の指示文書として切り出す価値がある。
2. **`cargo test -p harness-core`で、`home()`修正とは無関係な既存fail 2件を新たに検出した**:
   `projkey::tests::rootless_paths_do_not_collide`と
   `store::tests::context_ledger_base_only_honors_absolute_state_dir`。いずれもコード確認済みで、
   `/tmp/cg-abs-test`・`/no-such-aaa/..`のようなUnix形式の絶対パスリテラルをテストfixtureに使って
   おり（前者は`Path::is_absolute()`がWindowsではドライブレター無しの`/...`を絶対パスと認めないため
   意図通りに動かない、後者は`..`解決の結果が異なるパスに収束してしまう）、今回の`home()`修正より
   前のコミット（`8a9334c`等）に由来する。上記スコープ外の`pathutil.rs`の3件と**同じ系統
   （Unix形式絶対パスリテラルのテストfixtureがWindowsで意図通り動かない）**の既存バグであり、
   `home()`修正の副作用ではない。

## 参照

- `crates/harness-core/src/config.rs:1-10`
- `crates/autoflow/src/lock.rs`, `condukt.rs`, `delegation_audit.rs`（今回failしたテストの所在）
- `docs/loop-engineering-followup-instructions.md`（この発見の元になった実装検証セッション）
