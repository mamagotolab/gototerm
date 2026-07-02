# Phase 2 手動確認手順

## 事前準備

1. `cargo test` が全パスすることを確認する。
2. `cargo build --release` が成功することを確認する。
3. `target/release/gototerm` を起動する。

## changed files 表示

1. `Ctrl+Shift+F` でサイドバーを表示する。
2. 別ペインまたは同じシェルで `touch a.md` を実行する。
3. サイドバーの `changed files` に `NEW  a.md` が表示されることを確認する。
4. `echo x >> a.md` を実行する。
5. `a.md` が `NEW` のまま表示されることを確認する。
6. `rm a.md` を実行する。
7. `a.md` が `DEL` に変わることを確認する。

## 無視パターン

1. `mkdir -p .git` を実行する。
2. `touch .git/phase2-ignore-check` を実行する。
3. `changed files` に `.git/phase2-ignore-check` が表示されないことを確認する。

## cd 追従

1. `mkdir -p /tmp/gototerm-phase2-check` を実行する。
2. `cd /tmp/gototerm-phase2-check` を実行する。
3. `changed files` の履歴がクリアされることを確認する。
4. `touch after-cd.md` を実行する。
5. `changed files` に `NEW  after-cd.md` が表示されることを確認する。
