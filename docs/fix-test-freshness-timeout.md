# FIX: `test_freshness::run_ignored_test` に subprocess タイムアウトが無い / `check-plugin-versions.py` の Windows 文字コードエラー

**状態**: 未修正。`docs/DESIGN-continuous-audit-triage.md` の実装（`rationale`/経過日数/`test_freshness.rs`/`bridge.rs` 統合）を review した際に発見。

## 1. 問題 A（本命）: `run_ignored_test` の `cargo test` 呼び出しにタイムアウトが無い

[`crates/overwatch/src/test_freshness.rs:71`](../crates/overwatch/src/test_freshness.rs#L71) の `run_ignored_test`:

```rust
pub fn run_ignored_test(crate_name: &str, fn_name: &str) -> TestFreshness {
    match Command::new("cargo")
        .args(["test", "-p", crate_name, "--", "--ignored", fn_name])
        .output()
    {
        Ok(out) => classify_test_output(out.status.success(), &String::from_utf8_lossy(&out.stdout)),
        Err(_) => TestFreshness::ExecutionError,
    }
}
```

`Command::output()` は子プロセスの終了を**無期限に待つ**。タイムアウトの仕組みが一切ない。

呼び出し元は [`crates/overwatch/src/bridge.rs:257-261`](../crates/overwatch/src/bridge.rs#L257) — `to_backlog()`/`run_in()`（`overwatch review-queue --to-backlog`）の中で、確認済み finding 1件ごとにループの中から呼ばれる:

```rust
let freshness = test_freshness::find_ignored_test(&f.finding_id, cwd).map(
    |(crate_name, _test_path, fn_name)| {
        test_freshness::run_ignored_test(&crate_name, &fn_name)
    },
);
```

`#[ignore]` 対象テストがデッドロック／ネットワーク待ちのビルド／壊れたコンパイルなどでハングした場合、`--to-backlog` コマンド全体が無期限に固まる。ループなので1件のテストがハングするだけで残り全件の橋渡しも止まる。

### 1.1 なぜこれがバグと言えるか — 同時期の姉妹修正との比較

このリポジトリの直近コミット群は、まさに同じバグクラス（タイムアウト無しの subprocess 呼び出し）を横断的に潰している最中である:

- `93f0f10 fix(ctxrot): bound overwatch lease call with a timeout` — UserPromptSubmit hook パス上の `overwatch lease` 呼び出しに `wait_timeout::ChildExt` で8秒の上限を追加。「a hung overwatch binary could wedge the turn indefinitely」という理由。
- `fix(condukt): bound git subprocess timeout` / `fix(fugu-router): bound cmd_sync's git subprocesses with a timeout` — 同様の理由で git subprocess にタイムアウトを追加。

いずれも「このコードベース全体が『fail-soft・ターンを絶対に壊さない』という不変条件を追求している」という設計思想に基づく修正であり、`test_freshness::run_ignored_test` だけがこの防御から漏れている。`--to-backlog` はホットな hook パスではなく人間/LLM が明示的に叩く CLI ではあるが、他の全経路が慎重にタイムアウトで守られている中でここだけ抜けているのは明確な見落としである。

### 1.2 修正方針 — `ctxrot::hooks::guard::run_with_timeout` と同じパターンを踏襲

`wait-timeout` crate は workspace 内で既に広く使われている（`Cargo.lock` 確認済み、ctxrot/condukt/fugu-router 等）。`overwatch` の `Cargo.toml` には未追加なので追加する。

```diff
# crates/overwatch/Cargo.toml
 [dependencies]
 toml = { workspace = true }
+wait-timeout = { workspace = true }
```

`test_freshness.rs` 側:

```rust
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Max time to wait on the reverse-looked-up regression test before giving up.
/// `cargo test` here includes a build step, so this is longer than the 8s used
/// for the plain `overwatch lease` subprocess call in ctxrot's
/// `run_with_timeout` — but it must still be bounded, since this runs inside
/// `overwatch review-queue --to-backlog`'s per-finding loop and a single
/// hung/deadlocking ignored test must not wedge the whole bridge.
const RUN_IGNORED_TEST_TIMEOUT_SECS: u64 = 60;

pub fn run_ignored_test(crate_name: &str, fn_name: &str) -> TestFreshness {
    let child = match Command::new("cargo")
        .args(["test", "-p", crate_name, "--", "--ignored", fn_name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return TestFreshness::ExecutionError,
    };
    match run_with_timeout(child, Duration::from_secs(RUN_IGNORED_TEST_TIMEOUT_SECS)) {
        Some((success, stdout)) => classify_test_output(success, &String::from_utf8_lossy(&stdout)),
        None => TestFreshness::ExecutionError,
    }
}

/// Wait on an already-spawned child for at most `timeout`, killing (and
/// reaping) it on timeout so it never lingers. Returns (success, stdout) on a
/// completed exit; None on a timeout or wait error. Mirrors
/// `ctxrot::hooks::guard::run_with_timeout`.
fn run_with_timeout(mut child: std::process::Child, timeout: Duration) -> Option<(bool, Vec<u8>)> {
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let out = child.wait_with_output().ok()?;
            Some((status.success(), out.stdout))
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        Err(_) => None,
    }
}
```

タイムアウト時は `TestFreshness::ExecutionError` を返すことで、`bridge.rs::build_notes` の既存の fail-soft 分岐（`NotFound`/`ExecutionError`/`None` → 「該当テストなし」に畳む）にそのまま乗る。`build_notes` 側の変更は不要。

### 1.3 テスト

`ctxrot` の `run_with_timeout_kills_and_returns_none_on_timeout` / `run_with_timeout_returns_stdout_on_fast_success` と同型のテストを追加する:

- ハングする子プロセス（例: 実際に長時間 `sleep` するダミー、または `cargo test` を模した長時間コマンド）を渡し、タイムアウト内に `ExecutionError` で返ってくること・実際に kill されること（`start.elapsed() < timeout + 余裕` で検証）。
- 高速に成功する子プロセスは通常通り `classify_test_output` の結果を返すこと（既存の `run_ignored_test_reports_failing_for_a_test_that_actually_fails` 等の end-to-end テストがこの経路を通ることを確認する回帰にもなる）。

### 1.4 受け入れ基準

- [ ] `overwatch/Cargo.toml` に `wait-timeout` を追加。
- [ ] `run_ignored_test` が `spawn` + `wait_timeout` 経由になり、無限待ちの `output()` 呼び出しが無くなっている。
- [ ] タイムアウト定数に、なぜこの秒数か（`cargo test` のビルドコストを含むため ctxrot の8秒より長い）を説明するコメントがある。
- [ ] タイムアウト時に子プロセスが kill & reap されることを検証するテストがある。
- [ ] 既存の `run_ignored_test_reports_failing_for_a_test_that_actually_fails` など既存テストが引き続き green。
- [ ] 変更した plugin（`overwatch`）の version が3ファイル lockstep で上がっている。

## 2. 問題 B: `check-plugin-versions.py` が Windows で `UnicodeDecodeError` を起こす

`python3 scripts/check-plugin-versions.py` 実行時、`.claude-plugin/marketplace.json` を開く箇所で:

```
UnicodeDecodeError: 'cp932' codec can't decode byte 0x94 in position 169: illegal multibyte sequence
```

原因: `MP_PATH` を開く `open()` 呼び出しに `encoding="utf-8"` が指定されておらず、Windows のデフォルトロケール（cp932）でデコードしようとして、UTF-8 のみで正当な日本語/記号バイト列（例: 全角ダッシュ等）を読めずに落ちる。

このスクリプトは `CLAUDE.md` の「バージョン整合（絶対厳守）」節で **commit 前・push 前・CI 必須ゲート**と明記されている。Windows 環境のコントリビューターでは、このゲートが実行前にクラッシュし、**バージョン drift の検出が一切機能しない**まま commit/push が進んでしまう可能性がある。

### 2.1 修正方針

`scripts/check-plugin-versions.py` 内で `.claude-plugin/marketplace.json`（および他に `encoding` 未指定で開いているファイルがあれば同様に）を開いている箇所すべてに `encoding="utf-8"` を明示する。

```diff
-with open(MP_PATH) as f:
+with open(MP_PATH, encoding="utf-8") as f:
     marketplace = json.load(f)
```

`plugin.json`/`Cargo.toml` を開いている箇所も同様に確認し、同じ問題があれば揃って直す（`check-version-bumped.py` に同種の箇所があれば同様に修正）。

### 2.2 受け入れ基準

- [ ] `scripts/check-plugin-versions.py` の全 `open()` 呼び出しに `encoding="utf-8"` が明示されている。
- [ ] `scripts/check-version-bumped.py` も同様に確認・必要なら修正。
- [ ] Windows（cp932 ロケール）環境で `python3 scripts/check-plugin-versions.py` がクラッシュせず実行できることを確認。
- [ ] 既存の CI（Linux/UTF-8 ロケール想定）の挙動に影響が無いことを確認（`encoding="utf-8"` の明示は元々 UTF-8 前提だった箇所なので non-breaking のはず）。

## 3. 優先度

問題 A（`run_ignored_test` タイムアウト欠如）が実害の大きさで優先。問題 B は必須ゲートの信頼性に関わるため、労力は小さいが見過ごされやすい。両方とも独立に着手可能。
