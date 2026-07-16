# 指示文書: `pid_alive()`のプロセス生存確認がWindowsで機能しないバグの修正

`docs/windows-home-portability-fix-instructions.md`の検証セッションで見つかった、
`HOME`/`dirs::home_dir()`とは別原因のバグ。次に着手するClaude Codeセッションへの実行指示。

**着手前に読むこと**: `docs/windows-home-portability-fix-instructions.md`の「検証結果・追加で
見つかった論点」節（本バグの発見経緯）。

## 発見の経緯

`docs/windows-home-portability-fix-instructions.md`の修正（`harness-core::config::home()`の
`HOME`優先化）を検証中、`cargo test -p autoflow`（Windows環境、`--test-threads=1`でも再現）で
`lock::tests::live_pid_lock_is_active_stale_is_not`だけが解消せずfailし続けた。他の5件は`HOME`の
修正で直ったため、これだけ別原因と切り分けて調査した。

## 根本原因

`crates/autoflow/src/lock.rs:63-79`:

```rust
fn pid_alive(pid: u32) -> bool {
    // Fast path on Linux: /proc/<pid> exists iff the process is alive.
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    // Portable fallback: `kill -0 <pid>` exits 0 when the process is signalable.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
```

`backlog_driver_active()`（同ファイル）はこの関数で「backlogのrun lockを持つPIDがまだ生きて
いるか」を判定し、autoflowが「他プロセスが駆動中なら自分は動かない」を決める安全弁として使って
いる。Linux以外のOSでは`kill -0 <pid>`をサブプロセスとして実行する「portable fallback」に
フォールバックするが、Windowsには同じ意味論の`kill`コマンドが存在しない（Git Bash上でも
`kill -0`の挙動はUnixの`kill(2)`システムコールと同一ではない）。この結果、Windows上では
`pid_alive()`が常に`false`を返し、`backlog_driver_active()`は**常に「非アクティブ」と誤判定**する
——つまりWindows環境では、他プロセス（`/flow`や`/backlog`）がbacklogのrun lockを実際に保持して
実行中でも、autoflowはそれを検知できず、二重駆動を防ぐ安全弁が機能しない。テストの失敗は
この実運用上のバグの症状である。

## 修正内容（設計案。実装セッションでコンパイル・動作確認しながら詰めること）

`autoflow`の`Cargo.toml`には既に`[target.'cfg(unix)'.dependencies] libc = "0.2"`があり
（`compass.rs`が`libc::kill`をプロセスグループ終了に既に使っている）、`Cargo.lock`には
`windows-sys`が（他クレート経由で）既に解決済みで載っている。新しい重量級の依存を増やさずに
両OSへ対応できる。

1. **Unix**: サブプロセス起動（`kill`コマンド）をやめ、`libc::kill(pid, 0)`を直接呼ぶ。シグナル
   `0`は実際には送らず、プロセスの存在確認のみに使う决まった慣用句。戻り値`0`なら生存、`-1`かつ
   `errno == ESRCH`なら死亡、`-1`かつ`errno == EPERM`なら「存在するが自分の権限では触れない」
   （＝生存扱いにする）。これにより`/proc/<pid>`のLinux専用fast pathも不要になり、Unix全体
   （Linux/macOS/BSD）で一本化できる（`unsafe`を使うので、境界条件・errno取得は
   `std::io::Error::last_os_error()`を使うか`libc`の`__errno_location`相当を確認すること）。
2. **Windows**: `windows-sys`クレートを`autoflow`の`[target.'cfg(windows)'.dependencies]`に追加し
   （必要フィーチャ: `Win32_Foundation`, `Win32_System_Threading`）、
   `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid)`でハンドル取得を試みる。ハンドルが
   NULLでなければ生存、NULLなら死亡（権限不足で失敗するケースもあるが、`PROCESS_QUERY_LIMITED_
   INFORMATION`は最小権限で通常のプロセスに対して取得できるはず）。取得できたハンドルは
   `CloseHandle`で必ず閉じること（リークさせない）。テキスト出力をパースする`tasklist`サブ
   プロセス方式より、構造化された正しい生存確認になる。
3. 既存の`Stdio`インポート（サブプロセス用）が不要になった場合は削除する。

**未検証点（実装時に確認すること）**: 上記のerrno判定・`windows-sys`の正確なAPI名/フィーチャ
フラグ名はドキュメントを見ながら書いたものであり、実際にコンパイルして確認していない。実装
セッションは`cargo build -p autoflow --target x86_64-pc-windows-msvc`（またはこのマシン上で
直接ビルド）で型・フィーチャフラグの整合を確認すること。

## 受け入れ基準

- [ ] `pid_alive()`がUnixでは`libc::kill(pid, 0)`ベース、Windowsでは`windows-sys`の
      `OpenProcess`ベースに置き換わっている。サブプロセス（`kill`コマンド）起動は無くなっている。
- [ ] `cargo test -p autoflow`で`lock::tests::live_pid_lock_is_active_stale_is_not`がWindows上で
      PASSすることを確認する。
- [ ] 同テスト内の「生きているPID→active」「存在しないPID(2147483646)→inactive」の両方のケースが
      Windows上で正しく判定されることを確認する（既存のテストケースそのままでよいはず）。
- [ ] Unix上（CI）でも既存の全テストが無変更でPASSすることを確認する（`libc::kill`への置き換えが
      Linux/macOSの挙動を壊していないこと）。
- [ ] `crates/autoflow/Cargo.toml`に`windows-sys`を追加した場合、`Cargo.lock`の変更が最小限
      （既存の解決済みバージョンを再利用）であることを確認する。
- [ ] `crates/autoflow`のversionをmicro以上bumpし、3ファイルlockstepチェックが通る。

## スコープ外

- `backlog_driver_active()`/`this_session_holds_lock()`自体のロジック（lockファイルの読み込み・
  JSON解析）は変更しない。`pid_alive()`の内部実装のみを対象とする。
- `crates/autoflow/src/compass.rs`の`libc::kill(-pid, libc::SIGKILL)`（プロセスグループ終了）は
  今回のバグと無関係のため変更しない。

## 参照

- `crates/autoflow/src/lock.rs:63-79`（修正対象）
- `crates/autoflow/Cargo.toml`（既存の`libc`条件付き依存、`windows-sys`追加の参考）
- `docs/windows-home-portability-fix-instructions.md`（この発見の元になった検証セッション）
