# Phase 3 手動確認手順

1. `cargo run --release` で gototerm を起動する。
2. `Ctrl + Shift + F` でサイドバーを表示する。
3. 別ペインまたは別端末で、起動時の作業ディレクトリに対して次を実行する。

   ```fish
   for i in (seq 20); echo "line $i" >> demo.txt; sleep 0.2; end
   ```

4. `demo.txt` が changed files の先頭に表示され、サイドバー下部のプレビューに内容が流れることを確認する。
5. 追記が進んでも最後の行がサイドバー下部に見え続けることを確認する。
6. 次を実行する。

   ```fish
   rm demo.txt
   ```

7. プレビューに「(削除されました)」と表示されることを確認する。
8. 次を実行する。

   ```fish
   head -c 200M /dev/urandom > big.bin
   touch big.bin
   ```

9. 画面が固まらず、プレビューに「(バイナリファイル)」と表示されることを確認する。
