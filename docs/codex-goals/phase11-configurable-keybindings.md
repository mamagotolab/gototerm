# Codex Goal: Phase 11 — キーバインドを config.toml で変更可能にする

## ゴール

現状ハードコードされているアプリ側ショートカット（タブ・分割・フォーカス移動・リサイズ・
サイドバー開閉・フォント拡縮・コピー/ペースト・履歴クリア）を、`config.toml` の
`[keybindings]` テーブルで**個別に上書き**できるようにする。

- 何も書かなければ今まで通りの既定キーで動く（後方互換）
- 書いた項目だけが上書きされる（他はデフォルトのまま）— 既存の config 設計と同じ思想
- **対象外**（今回は変更しない）:
  - サイドバー内の vim 風ナビゲーション（`h`/`j`/`k`/`l`・`/`検索・`e`・`o` 等、`src/sidebar.rs`）
    — ranger/yazi 流の一覧内操作でありグローバルショートカットではないため
  - ターミナルの VT 制御シーケンス（矢印・Enter・Backspace・Tab・Space 等、`src/window.rs` の
    `(false, _, KeyCode::*)` 側）— これらはプロトコル上固定であり「ショートカット」ではない

## 対象となるアクション（既定値は現状維持）

`src/multiplexer.rs` の `Action`（`parse_shortcut`, L896-934）:

| action名（config.toml のキー） | 既定値 | 現在の動作 |
|---|---|---|
| `new_tab` | `Ctrl+Shift+T` | Action::NewTab |
| `close_pane` | `Ctrl+Shift+W` | Action::CloseFocused（元は `Ctrl+Shift+Q` も同じ動作だったが、config化にあたり `close_pane` は1つの既定値に絞る。`Ctrl+Shift+Q` の別名は廃止してよい） |
| `next_tab` | `Ctrl+Tab` | Action::NextTab |
| `prev_tab` | `Ctrl+Shift+Tab` | Action::PrevTab |
| `split_vertical` | `Ctrl+Shift+E` | Action::SplitVertical |
| `split_horizontal` | `Ctrl+Shift+O` | Action::SplitHorizontal |
| `toggle_sidebar` | `Ctrl+Shift+F` | Action::ToggleSidebar |
| `focus_left` | `Ctrl+Shift+H` | Action::Focus(Dir::Left) |
| `focus_down` | `Ctrl+Shift+J` | Action::Focus(Dir::Down) |
| `focus_up` | `Ctrl+Shift+K` | Action::Focus(Dir::Up) |
| `focus_right` | `Ctrl+Shift+L` | Action::Focus(Dir::Right) |
| `resize_up` | `Ctrl+Shift+Up` | Action::Resize(Dir::Up) |
| `resize_down` | `Ctrl+Shift+Down` | Action::Resize(Dir::Down) |
| `resize_left` | `Ctrl+Shift+Left` | Action::Resize(Dir::Left) |
| `resize_right` | `Ctrl+Shift+Right` | Action::Resize(Dir::Right) |

`src/window.rs` の `handle_key_input`（L948 の `match (ctrl, shift, keycode)` 内、アプリ側ショートカット部分のみ）:

| action名 | 既定値 | 現在の動作 |
|---|---|---|
| `increase_font` | `Ctrl+=` | `increase_font_size(1)` |
| `decrease_font` | `Ctrl+-` | `increase_font_size(-1)` |
| `copy` | `Ctrl+Shift+C` | `copy_clipboard()` |
| `paste` | `Ctrl+Shift+V` | `paste_clipboard()` |
| `clear_history` | `Ctrl+Shift+Delete` | `terminal.clear_history()` |

> 注意: `close_pane` の既定は `Ctrl+Shift+W` に統一し、`Ctrl+Shift+Q` の別名扱いは削除する
> （README のキー操作表も `Ctrl+Shift+W` のみに更新すること）。

## config.toml での指定方法

```toml
[keybindings]
# 書いた項目だけ上書き。書かなければ上表の既定値のまま。
focus_left = "Ctrl+Alt+H"
toggle_sidebar = "Ctrl+Shift+Space"
new_tab = "Ctrl+Shift+N"
```

### キー文字列の書式

`"Mod1+Mod2+...+Key"` の `+` 区切り。大文字小文字は区別しない。

- 修飾キー名: `Ctrl`, `Shift`, `Alt`, `Super`（順不同、重複不可）
- 最後のトークンがキー本体。対応するキー名（最低限これらをサポート）:
  - `A`〜`Z`, `0`〜`9`
  - `F1`〜`F12`
  - `Tab`, `Delete`, `Backspace`, `Enter`, `Escape`, `Space`
  - `Up`, `Down`, `Left`, `Right`
  - `Minus`（`-`）, `Equal`（`=`）
- 既存の `winit::keyboard::KeyCode` へのマッピングとして実装する

### バリデーション（起動時、ウィンドウを開く前に検証する）

設定ファイル読み込み直後（`Config` 構築後、ウィンドウ/PTY 起動より前）に以下を検証し、
**問題があれば分かりやすいエラーメッセージを stderr に出して `process::exit(1)`** する
（他の処理は一切開始しない）:

1. **修飾キー必須**: 修飾キー（Ctrl/Shift/Alt/Super のいずれか1つ以上）を含まない指定は拒否する。
   例: `new_tab = "T"` はエラー。理由: 修飾キーなしの単独キーを許すと、誤って通常の文字入力
   （シェルへの `t` 入力等）を横取りしてしまう事故が起きるため。
   エラーメッセージ例:
   `keybindings.new_tab = "T" は不正です: 修飾キー（Ctrl/Shift/Alt/Super）が最低1つ必要です`
2. **パース不能な文字列**: 未知のキー名・空文字列などは同様にエラーで終了。
   エラーメッセージ例:
   `keybindings.focus_left = "Ctrl+Foo" は不正です: 不明なキー名 "Foo"`
3. **重複バインド**: 複数の action（新規指定・既定値を問わず、最終的に有効な組み合わせ全体で）が
   同一のキー組み合わせに衝突していたらエラーで終了。
   エラーメッセージ例:
   `キーバインドが重複しています: "Ctrl+Shift+H" が focus_left と new_tab の両方に割り当てられています`

## 実装方針（案・変更してよい）

1. `src/config.rs` の `Config` に `#[serde(default)] pub keybindings: std::collections::HashMap<String, String>` を追加
   （空マップがデフォルト。既存の空Vec問題と同じ理由で `#[serde(default)]` 必須）。
2. 新規 `src/keybindings.rs`（案）:
   - `Action` 列挙体は `multiplexer.rs` から見えるように整理（既存の `Action`/`Dir` を再利用 or 拡張。
     `window.rs` 側のショートカット5つも同じ列挙体に含めるか、別の小さな列挙体にするかは実装しやすい方でよい）
   - 上表の action名 ↔ 既定キー組み合わせのテーブル
   - `"Ctrl+Shift+H"` 形式の文字列 ⇔ `(modifiers, KeyCode)` の相互変換関数
   - 起動時に `TOYTERM_CONFIG.keybindings` を読み、デフォルトテーブルを上書きしつつ
     上記3種のバリデーションを行い、`HashMap<(ModMask, KeyCode), Action>` のような解決済みマップを作る
     （`lazy_static` で1回だけ構築 → `main.rs` の初期化タイミングでバリデーションが走るようにする）
3. `src/multiplexer.rs::parse_shortcut` と `src/window.rs::handle_key_input` の該当箇所を、
   ハードコードの `match` から、この解決済みマップを引く形に置き換える。
   - **ハードコードの match を全部残したまま二重管理にしない**。既定値もこのテーブル1箇所に集約する。
4. `README.md` の「キー操作」節の冒頭注記（「キーバインドは現状すべて固定です」）を、
   config.toml で変更できる旨の説明に差し替える。上の action名テーブルもコンパクトな形でREADMEに転記する。
5. `config.example.toml` に `[keybindings]` の使用例（コメントアウト、2〜3行程度）を追記する。

## 絶対制約

- 既存のテスト・既存のキー操作（デフォルト設定時の挙動）は一切変わらないこと。
  デフォルト設定のままなら今まで通りの全キー操作が寸分違わず動く。
- IME 変換中のキー処理・VT制御シーケンス（`window.rs` の `(false, _, KeyCode::*)` 側）には触れない。
- `cargo build --release` が通ること。`cargo fmt` は**今回変更したファイルのみ**に適用する
  （無関係ファイルの再フォーマット・reformat 差分を出さないこと）。
- Windows ターゲット（`cfg(windows)`）を壊さないこと（プラットフォーム分岐は今回の変更に無関係のはず）。

## 検証（このあと Claude 側で実施する。Codex は実装のみでよい）

- `cargo build --release` が通る
- `timeout 2 ~/.local/bin/gototerm`（あるいはビルド後のバイナリ）で通常起動できる
  （config.toml 未設定 or keybindings 未設定でもデフォルト通り起動）
- 意図的に不正な `keybindings` エントリ（修飾キーなし／重複／不明キー名）を書いた config.toml で
  起動し、意図通りエラーで即終了することを確認する
- 正常なリバインド（例: `focus_left = "Ctrl+Alt+H"`）を書いた config.toml で起動し、
  新しいキーで実際にフォーカス移動が起きることを確認する
