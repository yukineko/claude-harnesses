---
name: session-master
description: 同じ git repo（common dir）で複数セッションが動くときの master/member 運用。master は queue を next→実行→complete で直列処理し、member は push / rollout / main への merge を自分で行わず queue に積んで results で結果を受け取る。宛先名の登録（name）と status の読み方も扱う。
---

# session-master

同じ git common dir を共有するセッション群（worktree が違っても同じグループ）の中で、
**master は 1 つだけ**。master だけが push・rollout（`scripts/rollout-plugins.sh`）・
main への merge・relay の裁定を行う。member がこれらを Bash で実行しようとすると
PreToolUse gate が Deny し、依頼を master の queue に積む。

役割は SessionStart / UserPromptSubmit の additionalContext に毎回出る。
手で確かめるときは `session-master role`（stdout `master` / `member` / `undetermined`、
exit 0 / 1 / 2）。

## master の手順（queue を直列に処理する）

1. `session-master next` — in_progress の item があればそれを返す（`"resumed":true`）。
   無ければ最古の pending を in_progress にして返す。`{"item":null}` なら空。
2. item の `command` を item の `cwd` で実行する（内容を読んで妥当か裁定してから）。
3. `session-master complete <ID> --outcome ok|failed --result "<要約>"` — 結果を記録する。
   依頼元 member には次の prompt で届く。
4. 1 に戻る。同時に in_progress にできるのは 1 件だけ（直列）。

Stop は block しない。queue が残っていれば systemMessage で件数が出る。

## member の手順

- push / rollout / main への merge が Deny されたら、**別の書き方で再試行しない**。
  Deny 理由に queue の item id が書いてある。master の処理を待つ。
- 結果は次の prompt の additionalContext に届く。手で見るなら
  `session-master results`（`--ack` で配達済みにする）。
- Bash 以外の依頼（裁定・相談など）は
  `session-master request --kind relay|other --command "<内容>" [--note "<補足>"]`。
- member 同士の SendMessage は Deny される。宛先は master のみ。

## 宛先名の登録

SendMessage の宛先（ListAgents に出る名前）で master を判別するため、
各セッションは自分の名前を登録しておく:

```sh
session-master name <NAME>
```

登録されていない名前・確かめられない宛先への SendMessage は Deny（判定不能は制限側）。

## status の読み方

`session-master status` は JSON を返す。

- `master_state`: `live`（heartbeat が TTL 内）/ `expired`（TTL 超過。最古の live member が
  次の操作で昇格）/ `undetermined`（heartbeat が未来・欠落。昇格しない）/ `none`。
- `members[]`: `alive` は `true` / `false` / `null`（判定不能）。`role` は master / member。
- `queue[]`: `status` は pending / in_progress / done / failed。`delivered` は結果配達済みか。

## 暫定の扱い（coordinator 裁定 2026-11-22）

panic barrier の選び方、askrelay を変えないこと、宛先名の登録と未確認宛先の Deny、
Stop を block しないこと、GATE_CRATES 未登録 — の 5 点は **暫定**。
また gate は協調的な制御で、CLI を gate の外から直接呼ぶ id 詐称は防げない（限界）。
member は `next` / `complete` や他人の `--session` を Bash で呼ばない（Deny される）。
正典は `crates/session-master/README.ja.md` の「暫定の扱い」節（ここには写さない）。
