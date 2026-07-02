# Codex Goal: Phase 7 — プレビューUX刷新（全面ビュー・Markdown整形・エディタフォールバック）

## ゴール

ユーザーフィードバック「惜しい」への対応4点：

1. サイドバーのデフォルトモードを **files** にする
2. ファイルを開いたときのプレビューを「下半分の間借り」から**サイドバー全面のビュー**に昇格し、
   **サイドバー幅も一時的に広げる**（読み物として成立させる）
3. Markdown を**整形表示**（見出し・リスト・コード・引用）し、**折り返し＋スクロール**で全文読めるようにする
4. 編集コマンドが無い環境への対応：存在チェック＋**[OSの既定アプリで開く]** を常設

## 前提（先に読むこと）

- `src/sidebar.rs` … モード切替・RowAction・ブラウズ・現行プレビュー
- `src/preview.rs` … バックグラウンド読込（この経路は維持）
- `src/window.rs` の `open_url` … OS既定アプリ起動の既存分岐（xdg-open / Windows）
- `src/config.rs` の `resolve_editor`
- docs/codex-goals/phase3-live-preview.md の絶対制約＋**configの新フィールドはserde(default)必須**

## 実装内容

### 1. デフォルトモード変更

- `SidebarMode` の初期値を `Files` に変更（1箇所）。

### 2. ビュー構造の再編

サイドバーの表示状態を「モード」と「全面プレビュー」の2層にする：

```rust
enum SidebarView {
    List,                 // 現行の files / changes 表示（modeで切替）
    Reader,               // 全面プレビュー（手動でファイルを開いたとき）
}
```

- **Reader に入る契機**：files のファイル行クリック / changes の行クリック / 端末内パスクリック。
- **Reader から戻る**：先頭の `← 戻る` 行をクリック → 元の List（元のモード）へ。
- **changes モードの下部ライブ追従（AIの書きかけが流れる表示）は現状のまま残す**。
  Reader は「腰を据えて読む」用、ライブ追従は「流れを見る」用と役割を分ける。

### 3. Reader 時のサイドバー拡幅

- config に `preview_ratio: f64` を追加（**`#[serde(default = ...)]` で 0.5**。例tomlにも追記）。
- `Multiplexer::refresh_layout` に渡す比率を、サイドバーが Reader のときだけ `preview_ratio` に切り替える
  （`Sidebar` に `current_ratio()` を持たせ、multiplexer はそれを使う形が最小変更）。
- Reader 出入りのタイミングで `refresh_layout()` を呼び、端末ペインを縮め/広げ直す。

### 4. Reader のレイアウト

```
 ← 戻る                              ← RowAction::CloseReader
 airticles/articles/xxx.md 📌        ← ファイル名（ピン既存仕様のまま）
 [編集: nvim]                        ← RowAction::EditFile（既存）
 [OSの既定アプリで開く]               ← RowAction::OpenWithSystem（新設）
 ─────────────
 （本文。折り返し表示・先頭から・ホイールで上下スクロール）
```

- **折り返し**：切り詰めをやめ、表示幅で折り返す。純関数
  `fn wrap_line(line: &str, cols: usize) -> Vec<String>`（unicode-width で全角対応）＋テスト
  （ASCII / 全角混在 / ちょうど境界 / 空行）。
- **スクロール**：`on_scroll` を Reader でも処理（クランプは既存の純関数を流用）。表示は先頭から開始。
- Reader中もピン中ファイルの変更イベントで内容は再読込する（既存仕様）。再読込時はスクロール位置を維持
  （末尾より後になったらクランプ）。

### 5. Markdown 整形

- 依存追加：`pulldown-cmark`（これ以外は追加不可。バージョンは最新安定）。
- 対象：拡張子 `.md` / `.markdown` のときだけ。他は従来どおりプレーン表示。
- 純関数 `fn render_markdown(src: &str) -> Vec<StyledLine>` を新設（`StyledLine` = `Vec<(String, Style)>` 相当の
  中間表現。セル化は既存の push_segments 系に合流）＋代表ケースのテスト
  （見出し / リスト / コードブロック / インラインコード / 引用 / 通常段落 / 水平線）。
- 装飾ルール（既存パレットの `Color` を使う）：
  - H1/H2/H3: 太字＋Cyan（レベルで濃淡が出せなければ全部同色でよい。行頭に `#` の数を残す）
  - 箇条書き: `• `（ネストはインデント2つ）
  - コードブロック: fg=Green（背景色変更が既存機構で難しければ fg のみでよい）
  - インラインコード: fg=Yellow
  - 引用: 行頭 `│ ` を BrightBlack、本文はそのまま
  - 水平線: `────` を BrightBlack
  - 強調(bold)は太字属性、リンクはテキストのみ表示（URL は括弧で付記しない）
- 画像（`![]()`）は今回は `[画像: alt]` のプレースホルダ表示（インライン画像描画は次フェーズ）。
- 整形は読込完了時に一度だけ行い、スクロールは整形後の行に対して行う（毎フレーム再パースしない）。

### 6. エディタの存在チェック＋OS既定アプリ

- `[編集: nvim]` クリック時、コマンドが PATH に存在するかを確認（workspace.rs に既存の
  実行ファイル探索ヘルパーがあれば流用、無ければ同等の純関数）。
  存在しなければ Reader 内に1行 `（編集コマンドが見つかりません: nvim。config.toml の editor で設定できます）`
  を BrightBlack で表示（数秒で消す必要はない。次の再描画まで出しっぱなしでよい）。
- `[OSの既定アプリで開く]`：`open_url` の既存分岐と同じ方法でファイルパスを開く
  （Linux: xdg-open / Windows: 既存の rundll32 or `cmd /c start` 方式に合わせる）。
  spawn するだけで結果は追わない。
- `config.example.toml` / `config.windows.example.toml` の `editor` コメントを充実させる：
  ```toml
  # [編集] で使うエディタ。空なら $EDITOR → nvim の順で決まる。
  # 例: editor = ["nvim"] / ["vim"] / ["micro"] / ["helix"]
  # VSCode 等の GUI エディタ派は editor を設定せず
  # [OSの既定アプリで開く] を使うのが簡単（既定アプリの関連付けに従う）。
  ```

## 制約

- 新規依存は `pulldown-cmark` のみ。
- ファイル読込は既存の FilePreview 経路（バックグラウンド）以外で行わない。
- config 新フィールドは `#[serde(default)]`（または default 関数）必須。
- 既存のキーボード操作・分割・changes ライブ追従の挙動を変えない。
- 無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。コメントは日本語で「なぜ」。

## 完了条件（この順で検証）

1. `cargo test` 全パス（wrap_line / render_markdown / スクロールクランプの新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase7-verify.md` に出力：
   起動 → Ctrl+Shift+F → **最初から files 一覧** → README.md をクリック →
   サイドバーが広がり全面プレビュー（見出しが太字色付き・折り返し・ホイールでスクロール）→
   `← 戻る` で一覧と元の幅に戻る → [編集] で nvim → [OSの既定アプリで開く] で既定アプリ →
   changes モードのライブ追従は従来どおり → editor を存在しないコマンドにすると案内が出る。
