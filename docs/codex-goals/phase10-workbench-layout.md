# Codex Goal: Phase 10 — ワークベンチ3分割レイアウト

## ゴール

`Ctrl+Shift+F` を「右サイドバーのトグル」から「**ワークベンチレイアウト全体のトグル**」に変える。

```
┌─────────┬────────────────────────────┐
│ サイド   │ プレビュー（Reader）        │
│ バー     │  選んだ / AIが書いている    │
│ (左)    │  ファイルが常にここに出る    │
│ files/  ├────────────────────────────┤
│ changes │ ターミナル領域              │
│ 一覧のみ │ （タブ・分割は従来どおり）   │
└─────────┴────────────────────────────┘
```

ユーザーフィードバックの反映：
- Reader がサイドバーを乗っ取って幅が変わる現方式は「見づらい」→ **プレビューは右上に常駐**、開いても画面が組み変わらない
- サイドバーは**左**（Obsidian準拠）で**一覧専業**
- **[編集] はプレビュー枠の中でエディタに切り替わる**（Obsidianの閲覧⇄編集トグルの体験）

## 前提（先に読むこと）

- `src/sidebar.rs` … Phase 9 までの一覧・選択・フォーカス・Reader。**Reader 部分を今回分離する**
- `src/multiplexer.rs` … レイアウト計算（content_and_sidebar_viewport）・フォーカス・SidebarRequest
- `src/window.rs` … コマンド指定つき TerminalWindow（Phase 5。エディタ切替に使う）
- docs/codex-goals/phase3-live-preview.md の絶対制約＋config新フィールドは serde(default) 必須

## 実装内容

### 1. レイアウト計算

- ワークベンチON時（タブバーを引いた後の領域に対して）：
  1. 左から `sidebar_ratio` を**サイドバー**に（デフォルトを 0.30 → **0.25** に変更）
  2. 残り右側を上下に分割：上 `preview_ratio`（デフォルト 0.5）が**プレビュー**、下が**ターミナル領域**
  3. GAP は既存の分割と同じ
- ワークベンチOFF時：全面ターミナル（従来どおり）。サイドバー/Reader の状態は破棄せず保持
  （watcher はOFF時に止める＝既存の非表示時コストゼロ方針のまま）。
- Reader 表示による幅の変化（current_ratio 方式）は**廃止**。preview_ratio は右側の上下分割比に転用
  （config キー名は変えない。コメントを更新）。

### 2. Reader の分離（`src/reader.rs` 新設）

- `sidebar.rs` から Reader 関連（FilePreview 保持・reader_lines / scroll / notice・Markdown整形・
  折返し・ヘッダ行アクション）を `ReaderPane` として移設する。**ロジックは移動が主で、書き換えは最小限**。
- Reader 本文の**折返し幅に上限 100 桁**を追加（ペインが横に広くなったため。
  `wrap_line` 呼び出し時に `min(cols, 100)`）。左寄せでよい。
- 何も開いていないときのプレースホルダ：
  `ファイルを選ぶか、AI がファイルを書くとここに表示されます`（BrightBlack）。
- ヘッダは現行踏襲：`ファイル名 📌(クリックで追従に戻る)` / `[編集: nvim]` / `[OSの既定アプリで開く]`。
  `← 戻る` 行は**廃止**（常駐なので「戻る」概念がない）。
- 追従モード（AI の書きかけが流れる）も ReaderPane に統合：
  - 追従中＝生テキスト末尾追従（従来の changes ライブ表示と同じ）
  - ピン中＝先頭から・Markdown整形・スクロール可（従来 Reader と同じ）
  - サイドバー側は一覧だけを持ち、本文表示は一切しない

### 3. Multiplexer の配線

- `preview_slot: PreviewSlot` を追加：
  ```rust
  enum PreviewSlot {
      Reader(ReaderPane),
      Editor { win: Box<TerminalWindow>, saved: Box<ReaderPane> }, // エディタ表示中もReaderの状態を保持
  }
  ```
- **エディタ切替**：`SidebarRequest::EditFile(path)`（Reader ヘッダの [編集] / キー e）を受けたら、
  プレビュー枠の矩形で `TerminalWindow`（コマンド＝resolve_editor＋path、cwd＝ファイルの親）を生成し
  `PreviewSlot::Editor` に差し替え、**フォーカスをエディタへ**。
  エディタ終了（check_update が dead）で `saved` の ReaderPane に戻し、内容を再読込、
  フォーカスはターミナル領域へ。
- **追従の供給**：サイドバーの watcher イベントで最新変更ファイルが変わったら、
  Reader が追従モード（非ピン）のときだけ ReaderPane に反映する
  （サイドバーに `take_follow_target() -> Option<PathBuf>` を設け、AboutToWait で
   multiplexer が汲んで ReaderPane へ渡す。二重実装しない）。
- **端末内パスクリック**（Phase 4）→ ReaderPane のピン留めへ（ワークベンチOFFなら ON にしてから）。
- **マウスの領域ルーティング**：サイドバー矩形→sidebar、プレビュー矩形→reader
  （クリック＝行アクション、ホイール＝スクロール）、それ以外→従来どおり端末。
- **フォーカス**（Phase 9 の機構を拡張）：
  - サイドバーフォーカス中：↑↓/Enter/BS/Tab は一覧、**PageUp/PageDown は Reader のスクロール**、
    e / o は Reader のヘッダアクションと同義、Esc で端末へ
  - エディタ表示中はエディタが通常の端末としてキーを受ける（Esc は nvim に渡る）。
    エディタからの離脱＝`:wq`（終了）またはマウスで他領域クリックまたは Ctrl+Shift+B
  - 既存の Ctrl+Shift 系は常に最優先

### 4. config

- `sidebar_ratio` デフォルト 0.25 に変更（左幅）。`preview_ratio` はコメントを
  「右側の上下分割比（上=プレビュー）」に更新。example toml 2つも更新。
- 新キーが必要になった場合は必ず `#[serde(default)]`。

## 制約

- 新規依存なし。
- ワークベンチOFF時の挙動（全面ターミナル・タブ・分割・IME）は従来と完全に同一。
- Reader 分離は「移動」を基本にし、Markdown整形・折返し・スクロールの既存テストが
  （モジュールパス変更を除き）そのまま通ること。
- 無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。コメントは日本語で「なぜ」。

## 完了条件（この順で検証）

1. `cargo test` 全パス（レイアウト計算＝3領域の矩形分割の新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase10-verify.md` に出力：
   Ctrl+Shift+F で3分割になる（左一覧・右上プレビュー・右下ターミナル）→
   ファイル選択で右上に表示・**幅が変わらない** → AI/コマンドの書き込みが追従表示される →
   [編集] または e でプレビュー枠が nvim に変わる → :wq でプレビューに戻り内容が更新されている →
   もう一度 Ctrl+Shift+F で全面ターミナルに戻る → タブ・分割・yazi・IME が従来どおり。
