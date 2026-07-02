# Codex Goal: Phase 9 — サイドバーのキーボード操作

## ゴール

サイドバー（files一覧・changes一覧・Reader）をキーボードで操作できるようにする。
ユーザーフィードバック：「上下キーで操作できない」「『ほか N 件』の先のフォルダに到達できない」。

## 前提（先に読むこと）

- `src/sidebar.rs` … browse_scroll / RowAction / Reader / モード切替の既存機構
- `src/multiplexer.rs` … キー入力は現在フォーカス中の端末ペインへ一直線。ここに分岐を足す
- docs/codex-goals/phase3-live-preview.md の絶対制約

## フォーカスモデル

- `Multiplexer` に `sidebar_focused: bool` を追加（サイドバー非表示なら常に false）。
- **フォーカスの入り**：
  - `Ctrl+Shift+B`：サイドバーへフォーカス。**非表示なら表示してフォーカス**（開く＋操作を1キーで）
  - サイドバー内をクリック → フォーカスも移る
- **フォーカスの出**：
  - `Esc`：端末へ戻す
  - 端末ペインをクリック → 戻る
  - `Ctrl+Shift+F` で非表示にしたとき → 戻る
- **見た目**：フォーカス中は選択行を反転表示（bg=Color::White/fg=Color::Black 等）。
  非フォーカス時は選択ハイライトを出さない（今の見た目のまま）。
- **入力の遮断**：サイドバーフォーカス中、下記以外のキー（文字入力等）は**どこにも送らない**
  （ユーザーがサイドバーを見ながら打った文字がシェルに流れる事故を防ぐ）。
  IME イベントも同様に握り潰す。`Ctrl+Shift+*` の既存ショートカットは従来どおり最優先で効くこと。

## キー割り当て（サイドバーフォーカス中）

### files 一覧 / changes 一覧

| キー | 動作 |
|---|---|
| ↑ / ↓ | 選択行を移動。**画面外に出たら自動スクロール**（これで「ほか N 件」の先に到達できる） |
| PageUp / PageDown | 1画面ぶん移動 |
| Home / End | 先頭 / 末尾 |
| Enter | 開く（ディレクトリ→潜る / ファイル→Reader） |
| Backspace または ← | 親ディレクトリへ（`../` 相当。files のみ） |
| Tab | モード切替（files ⇔ changes。切替行クリックと同義） |
| Esc | 端末へフォーカスを戻す |

- 選択状態 `browse_selected: usize` を追加し、クリックと同じ RowAction 経路に流す
  （Enter = 選択行のクリックと同義。実装を二重化しない）。
- ディレクトリ移動・モード切替時は選択を先頭へリセット。
- 「… ほか N 件」行は選択対象にしない（↓で自然にスクロールが進む）。

### Reader

| キー | 動作 |
|---|---|
| ↑ / ↓ / PageUp / PageDown / Home / End | 本文スクロール（既存のスクロール量計算を流用） |
| Backspace または ← | 一覧へ戻る（`← 戻る` と同義） |
| e | [編集] と同義 |
| o | [OSの既定アプリで開く] と同義 |
| Esc | 端末へフォーカスを戻す |

## 操作ヒントの表示

- サイドバーフォーカス中のみ、最下行に1行：
  - 一覧: ` ↑↓:選択  Enter:開く  BS:上へ  Esc:端末`
  - Reader: ` ↑↓:スクロール  e:編集  BS:戻る  Esc:端末`
  （BrightBlack。行数予算に織り込むこと）

## multiplexer.rs の変更

- `KeyboardInput`：`parse_shortcut`（既存の Ctrl+Shift 系）を先に判定 → 未処理で
  `sidebar_focused` なら `sidebar.on_key(&KeyEvent) -> SidebarKeyResult` へ。
  `SidebarKeyResult` は `Consumed` / `ReleaseFocus` / `Request(SidebarRequest)`（編集起動等の既存依頼）。
- `Ime` イベント：sidebar_focused 中は握り潰す。
- クリックによるフォーカス移動は既存の MouseInput 分岐に追記。
- 端末側の `focus_changed`（IME位置やカーソル形状）はサイドバーフォーカス中 false になるよう整合させる。

## 制約

- 新規依存なし。
- マウス操作は従来どおり全て動くこと（キーボードは追加であって置き換えではない）。
- 既存の Ctrl+Shift 系ショートカット・端末への通常入力に回帰がないこと。
- 無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。コメントは日本語で「なぜ」。

## 完了条件（この順で検証）

1. `cargo test` 全パス（選択移動と自動スクロールの整合＝selection視界維持の純関数テストを含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase9-verify.md` に出力：
   Ctrl+Shift+B でサイドバーが開いてフォーカス → ↓連打で「ほか N 件」の先のフォルダまで到達 →
   Enter で潜る → BS で戻る → ファイルで Enter → Reader を↑↓でスクロール → e で nvim →
   Esc で端末に戻り通常タイピングが打てる → サイドバーフォーカス中に文字を打ってもシェルに流れない。
