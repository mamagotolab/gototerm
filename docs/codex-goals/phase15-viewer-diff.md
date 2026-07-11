# Codex Goal: Phase 15 — ビューアーに diff（+/-）表示

## ゴール

ワークベンチのプレビュー（右上のリーダー）で、**変更のある追跡ファイルを開いたら、
ファイルの中身そのものではなく `git diff`（+/- 付き）を色付きで表示**する。
Claude Code のチャットでは +/- が見えるのに、gototerm のビューアーは中身を流すだけ
だった穴を埋める。「changed files 一覧」ではなく「ビューアーで +/-」が価値の核。

未変更ファイル・未追跡（新規）ファイル・git 管理外は、**従来どおり中身**を表示する。

## 前提（先に読むこと）

- `src/preview.rs`
  - `PreviewContent` enum（`Text`/`Image`/`Binary`/`Deleted`/`Empty`）。
  - `spawn_read()` … バックグラウンドスレッドで `read_image` または `read_tail` を呼ぶ。
    ここに diff 取得を足す。
  - `read_tail()` / `tail_lines()` … テキストを行に割る（タブ→空白4）。
  - `PreviewLines` enum（`Text(&[String])` / `Message(&'static str)`）と `lines()`。
- `src/reader.rs`
  - `refresh_reader_document()`（334行付近）… `preview.lines()` を
    `PreviewLines::Text` なら markdown か白テキストへ、`Message` なら灰色へ変換。
    ここに diff の色付けを足す。
  - `StyledLine`（=`Vec<StyledSegment>`）、`StyledSegment{ text, style: TextStyle{ fg, bold } }`、
    `styled_plain(text, fg)`、`wrap_line(line, cols)`。これらで色付き行を作る。
  - `rebuild()`（230行付近）… ヘッダ（ファイル名など）を組む。diff 表示中の目印をここに足す。

## 実装内容

### 1. `src/preview.rs`

- `PreviewContent` に **`Diff(Vec<String>)`** を追加（unified diff の各行を保持。
  タブは `tail_lines` と同様に空白4へ）。
- 新規関数 `fn read_diff(abs_path: &Path) -> Option<Vec<String>>`:
  - `abs_path` の親ディレクトリを `-C` に使い、
    `git -C <parent> diff HEAD --no-color -- <abs_path>` を `std::process::Command` で実行。
  - 成功（exit 0）かつ stdout が非空なら、`tail_lines(stdout_bytes, usize::MAX)` で行に割って `Some(lines)`。
  - git が無い／リポジトリでない／未変更／未追跡で空 → `None`。エラーにしない。
  - **上限**: diff が大きすぎる場合に備え、stdout が 512KB を超えたら先頭 512KB だけ使う
    （または行数 5000 で打ち切り）。どちらかで良い。
- `spawn_read()` の非画像パス:
  ```
  let content = if is_image_path(&abs_path) {
      read_image(...)
  } else if let Some(diff) = read_diff(&abs_path) {
      PreviewContent::Diff(diff)
  } else {
      read_tail(&abs_path)
  };
  ```
- `PreviewLines` に **`Diff(&'a [String])`** を追加し、`lines()` の `PreviewContent::Diff`
  を `PreviewLines::Diff(...)` にマップ。
- `FilePreview` に **`pub fn is_diff(&self) -> bool`**（`matches!(self.content, PreviewContent::Diff(_))`）を追加
  （リーダーのヘッダ表示に使う）。
- `set_memory_content`（リモート表示）は diff 不要＝従来どおり Text/Binary のまま。

### 2. `src/reader.rs`

- `refresh_reader_document()` の match に `PreviewLines::Diff(diff_lines)` を追加:
  - 各行を `wrap_line(line, self.reader_wrap_cols())` で折り返しつつ、
    **行頭で色を決めて** `styled_plain(wrapped, color)` にする。
  - 色分け（判定順に注意。`+++`/`---` を `+`/`-` より先に）:
    | 行頭 | 色 |
    |---|---|
    | `@@` で始まる（ハンク見出し） | `Color::Cyan` |
    | `+++ ` / `--- `（ファイル見出し） | `Color::BrightBlack`（淡色） |
    | `diff --git` / `index ` / `new file` / `deleted file` / `rename ` / `similarity ` | `Color::BrightBlack` |
    | `+` で始まる（追加行） | `Color::Green` |
    | `-` で始まる（削除行） | `Color::Red` |
    | それ以外（文脈行） | `Color::White` |
  - 折り返し後の各行に同じ色を適用する（先頭行だけでなく全ラップ行）。
- `rebuild()` のヘッダ: **diff 表示中は目印を出す**。`self.preview.is_diff()` が true のとき、
  ファイル名の行の近くに `Color::Cyan` で ` ● HEAD との差分` のような1行（または既存の
  ファイル名行末に付記）を足す。`reader_body_slots()` の header_rows 計算と整合させること
  （増やした行数ぶん本文スロットを減らす）。

## テスト（省略禁止・ネットワーク/実git不要）

- `preview.rs`: diff 行を保持する純関数か、`tail_lines` を使った変換を確認するテスト。
  `read_diff` 自体は実 git 依存なので、**色分けの判定ロジックを純関数に切り出してテスト**するのが良い。
  例: `fn diff_line_color(line: &str) -> Color` を `reader.rs`（または共通）に作り、
  - `@@ -1,3 +1,4 @@` → Cyan
  - `+added` → Green / `-removed` → Red
  - `+++ b/x` → BrightBlack / `--- a/x` → BrightBlack
  - `diff --git a/x b/x` → BrightBlack
  - ` context` → White
  をテストする。
- 既存の preview/reader テストは全て緑のまま。
- `cargo test` と `cargo build` を通し、緑を確認して報告する。

## 制約（厳守）

- 未変更・未追跡・git 管理外は**従来どおり中身表示**（挙動を壊さない）。
- diff の色付けは既存の `StyledLine`/`styled_plain`/`wrap_line` を使う（第2描画スタックを作らない）。
- **依存を増やさない**（git はシェルアウト。差分パーサ crate を入れない）。
- `git` 実行は `read_image`/`read_tail` と同じ**バックグラウンドスレッド内**（UIスレッドで同期実行しない）。
- **`cargo fmt` は自分が触ったファイルだけ**。
- 検収で**起動スモークテスト**（`timeout 2 gototerm` で panic なし）を通す前提で実装する。
- 完了したら、変更/追加ファイル・追加テスト名・`cargo test` 結果を報告する。
