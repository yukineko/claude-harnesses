# askrelay

subagent の permission **Ask は main session で裁定する** — ユーザー裁定 2026-11-14（逐語）:
「sub agentのAskはmainで裁定する」「この仕組みはコードで強制する、どうせわすれる」
「gateにそんな例外は追加してほしくない」
「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」。

subagent の中では誰も permission prompt に答えられないので、gate（現状 blastguard）は `Ask` を
`Deny` に硬化する。`askrelay` は、その Ask を main session が裁定し忘れないことを機械で強制する。
Stop を block する **GATE crate**。2026-11-17 に blastguard から分離した。

## 手順

1. **記録** — askrelay ではなく gate が行う。blastguard が `queued` レコード（id `bgr-<12 hex>`）を
   `~/.blastguard/relay/<session_id>.jsonl`（`BLASTGUARD_RELAY_DIR` で上書き）へ追記し、deny 理由に id を付ける。
2. **実行済み**（`askrelay mark-executed`、Bash の PostToolUse / PostToolUseFailure）— main session の
   Bash 呼び出しで command と cwd が pending entry とバイト一致したものを実行済みにする。subagent の呼び出しは何も付けない。
3. **返送済み**（`askrelay mark-relayed`、SendMessage の PostToolUse）— main session の SendMessage が
   relay id を含めば、実行済みの entry を返送済みにする。
4. **Stop**（`askrelay stop`）— 「実行済みかつ返送済み」でも「dismiss 済み」でもない entry があれば block し、
   id・agent_id・cwd・コマンドを列挙する。re-entry でも block する。queue が読めない・行がパースできない・
   未知 id の event・Stop payload がパースできない・panic（`run_guarded`）・プラットフォーム用バイナリ欠落（launcher）は
   すべて block（fail-closed）。queue ファイルが無ければ allow。
5. **dismiss**（`askrelay dismiss <id> --reason "<理由>"`）— 人間が prompt を拒否した、または再実行できないとき用。
   理由が空・未知 id は exit 2、queue が読めなければ exit 3。dismiss は queue に記録され、以後の Stop には表示されない。

コマンドは実行しない。保存した cwd を解決・stat・実行に使わない。

## 構成

* `crates/harness-core/src/relay.rs` — queue 形式と操作。blastguard（enqueue 側）と共有する。blastguard が
  この crate に依存せず、この crate も blastguard に依存しないよう harness-core に置いた。
* `src/main.rs` — hook と `dismiss`、`src/protection.rs` — `--protects`。
* `hooks/hooks.json` — Stop、PostToolUse（Bash / SendMessage）、PostToolUseFailure（Bash）。

## 互換性と fail-closed

queue のパス・`BLASTGUARD_RELAY_DIR`・`bgr-` id 接頭辞・JSONL 形式は blastguard 時代から変えていない。
`harden_for_subagent` は blastguard に残るので、askrelay が無い・無効でも subagent の `Ask` は `Deny` のまま
（失われるのは main の Stop リマインドだけ）。

panic barrier: `stop` は `run_guarded`（panic で block）。`dismiss` は exit status が結果そのもの。
`mark-*` は判定を持たず（失敗した mark は entry を pending のまま残す＝制限側）、失敗は stderr に出す。

## テスト

`cargo test -p askrelay --test <name>`。relay のテストは 2 つのバイナリを動かす: hook と `dismiss` は
`askrelay`、enqueue と拒否記録は blastguard（`tests/support/bounded.rs` が必要ならビルドする）。
