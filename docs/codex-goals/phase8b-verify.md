# Phase 8b Verify

## 前提

- `cargo build --release` 済みの gototerm を起動する。
- `Ctrl+Shift+F` でワークベンチを表示する。
- 必要なら `assets/bin/gt` を PATH の通った場所へ置く。

## 手動確認

1. ローカル端末で event を手打ちする。

   ```sh
   printf '\033]7717;event;kind=mod;path=c3JjL21haW4ucnM=;tool=RWRpdA==\007'
   ```

   サイドバーに `● claude  MOD src/main.rs (Edit)` が表示され、changes に `src/main.rs` が流れる。

2. ローカル端末で file を手打ちする。

   ```sh
   printf '\033]7717;file;path=UkVNT1RFLm1k;seq=0;last=1;data=IyBoZWxsbyBmcm9tIGd0Cg==\007'
   ```

   Reader に `REMOTE.md (remote)` と本文 `# hello from gt` が表示される。

3. ローカル端末で `gt view` を確認する。

   ```sh
   assets/bin/gt view README.md
   ```

   Reader に `README.md (remote)` と README の末尾内容が表示される。

4. SSH 先から `gt view` を確認する。SSH 先が無い場合は `ssh localhost` を使う。

   ```sh
   scp assets/bin/gt localhost:~/bin/gt
   ssh localhost 'chmod +x ~/bin/gt && ~/bin/gt view README.md'
   ```

   手元の gototerm の Reader にリモートから送られた内容が表示される。

5. Claude Code hooks を確認する。

   ```sh
   gt init-hooks
   ```

   Claude Code で `Edit` / `Write` / `MultiEdit` / `NotebookEdit` がファイルを変更すると、サイドバーの AI 作業インジケータと changes に流れ、`mod` / `new` のときは Reader に内容が表示される。
