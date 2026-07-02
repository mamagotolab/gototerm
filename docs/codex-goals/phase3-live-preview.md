# Codex Goal: Phase 3 — AI追従ライブプレビュー

## ゴール

サイドバーを「変更ファイル名のリスト」から「**最後に変更されたファイルの中身をリアルタイム表示するライブビューア**」に作り替える。
Claude Code / Codex がファイルに書き込むたび、その中身がサイドバーに流れ、追記に末尾追従する（`tail -f` の全ファイル自動版）。

## 前提（先に読むこと）

- `src/sidebar.rs` … 現在のサイドバー。watcher のライフサイクル（`watcher_pending` によるバックグラウンド生成・root変更時の作り直し）は**変えない**
- `src/watcher.rs` … `FileChange`（root からの相対パス）。イベント検知はこの資産をそのまま使う
- `docs/workbench-v2.md` … 判断1（第2描画スタック禁止＝セル描画のみ）

## ⚠️ 昨日の障害から来る絶対制約

1. **ファイル読込を UI スレッドで行わない。** NFS・ネットワークマウント・巨大ファイルで読込がブロックすると画面全体が固まる（昨日 watcher 生成の同期実行で実際に起きた）。読込は watcher 生成と同じパターン（`std::thread::spawn` ＋ `mpsc` ＋ `try_recv`）で行うこと。
2. **読込は末尾のみ・上限つき。** ファイル末尾 64KB だけを読む（`File::seek(SeekFrom::End(-64*1024))`、それより小さければ全体）。
3. **デバウンス。** 同一ファイルへの連続書込で読込を乱発しない。前回読込から 100ms 以内の再読込要求はスキップし、次の tick で拾う。
4. **cargo fmt は自分が変更したファイルにのみかける。** リポジトリ全体の整形禁止。

## 実装内容

### 1. サイドバーの新レイアウト

```
 workspace  ~/work/programs/toyterm   ← dir を1行に凝縮（末尾省略表示）
 git: main  +2 ~3 ?1                  ← staged/modified/untracked を1行に凝縮
 ─────────────
 changed files
   MOD  src/main.rs        ← 先頭（最新）がプレビュー対象。▶ を付ける
   NEW  docs/plan.md
   （最新5件まで。6件目以降は「… ほか N 件」）
 ─────────────
 ▶ src/main.rs                        ← プレビュー中ファイル名（BrightWhite）
 （ここから下、残り行ぜんぶがファイル内容。
   末尾追従＝ファイルの最後の行が常に見える）
```

- 既存の `workspace` 詳細表示（staged/modified/…の5行）と `ai tools`・`keys` セクションは**廃止**し、上記の凝縮2行に置き換える（画面の主役はファイル内容）。
  - keys の内容は README に移す（README の「キー操作」節に追記）。
- git 情報の取得（`workspace::collect`）と5秒更新・cd追従は現状のまま。

### 2. ライブプレビューの動作

- watcher から NEW / MOD イベントが来るたび、その相対パスを「プレビュー対象」にする（最新優先で自動切替）。
- DEL イベントのファイルがプレビュー中だったら「(削除されました)」を表示し、changed files の次のファイルがあればそれに切り替える。
- プレビュー対象が決まったら / 変更イベントが来たら、**バックグラウンドで**末尾 64KB を読み、UI スレッドは `try_recv` で受け取って表示を更新する。
- 表示は末尾追従：内容の最後の行が常にペイン最下部付近に見えるよう、収まる分だけ末尾から表示する。
- 長い行は幅で切り詰め（既存 `cells_for_line` の挙動でよい。折返しは不要）。
- タブ文字は空白4つに展開する。
- バイナリ検出：読んだ内容の先頭 8KB に NUL バイトがあれば「(バイナリファイル)」とだけ表示。
- UTF-8 として不正なバイト列は `String::from_utf8_lossy` で表示。
- まだ変更イベントが1件もないとき：「(AIやコマンドがファイルを変更すると、ここに中身が流れます)」を BrightBlack で表示。
- cwd が変わったら changed files と同様にプレビューもクリア。

### 3. 構造の指針

- 新規 `src/preview.rs`：
  ```rust
  /// プレビュー本文の状態。読込はバックグラウンドで行い、結果を try_recv で受ける。
  pub struct FilePreview {
      target: Option<PathBuf>,        // root からの相対パス
      content: PreviewContent,        // Text(Vec<String>) | Binary | Deleted | Empty
      pending: Option<Receiver<...>>, // 読込中
      last_read: Instant,             // デバウンス用
  }
  ```
  - `set_target(&mut self, root, rel_path)` / `notify_changed(...)`（デバウンス判定込み）/ `poll(&mut self) -> bool`（読込完了を受け取ったら true）/ `lines(&self)` を持つ。
  - 純関数 `fn tail_lines(bytes: &[u8], max: usize) -> Vec<String>`（末尾から最大 max 行・タブ展開・lossy変換）と
    `fn looks_binary(bytes: &[u8]) -> bool` を切り出してユニットテストを書く
    （UTF-8正常系 / 不正バイト / NUL入り / 空 / 末尾改行あり・なし / タブ展開）。
- `src/sidebar.rs`：`FilePreview` を持ち、`refresh_if_stale` の中で `poll` して必要なら `rebuild`。
  rebuild でレイアウト（凝縮ヘッダ＋changed files＋プレビュー）を組む。
- `src/multiplexer.rs`・`src/watcher.rs`：原則変更なし。

## 制約

- 新規依存なし（std のみ）。
- サイドバー非表示時は watcher もプレビューも持たない（現状どおりコストゼロ）。
- 既存コードの無関係な整形・リファクタ禁止。コメントは日本語で「なぜ」を書く。

## 完了条件（この順で検証）

1. `cargo test` 全パス（`tail_lines` / `looks_binary` の新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase3-verify.md` に出力：
   サイドバー表示 → 別ペインで `for i in (seq 20); echo "line $i" >> demo.txt; sleep 0.2; end`（fish）→
   demo.txt の中身がサイドバーに流れ、追記に末尾追従する →
   `rm demo.txt` → 「(削除されました)」 → 巨大ファイル（`head -c 200M /dev/urandom > big.bin`）を
   触っても画面が固まらず「(バイナリファイル)」表示になる。
