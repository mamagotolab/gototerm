# Phase 4 手動確認手順

1. toyterm を起動し、確認用ファイルを作る。
   ```sh
   mkdir -p airticles
   printf 'first line\n' > airticles/test.md
   ```

2. 端末画面に相対パスを表示する。
   ```sh
   echo airticles/test.md
   ```

3. 表示された `airticles/test.md` をクリックする。
   - サイドバーが閉じている場合は自動で開く。
   - プレビューに `airticles/test.md` の内容が表示される。
   - ヘッダにピン留め表示が出る。

4. ピン留め中に別ファイルを変更する。
   ```sh
   printf 'other\n' > airticles/other.md
   ```
   - プレビュー対象が `airticles/other.md` に切り替わらないことを確認する。

5. ピン留め中のファイル自身へ追記する。
   ```sh
   printf 'second line\n' >> airticles/test.md
   ```
   - プレビューに追記内容が反映されることを確認する。

6. プレビューヘッダ行をクリックする。
   - ピン留めが解除され、追従モードに戻ることを確認する。

7. サイドバーの changed files にあるファイル行をクリックする。
   - クリックしたファイルへプレビューが切り替わり、ピン留めされることを確認する。

8. URL クリックの従来動作を確認する。
   ```sh
   echo https://example.com
   ```
   - 表示された URL をクリックして、ブラウザが開くことを確認する。
