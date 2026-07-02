# Codex Goal: Phase 8b — gt スクリプトと Claude Code hooks 連携（OSC 7717）

## ゴール

`docs/gt-protocol.md` の OSC 7717（event / file）を実装し、**Claude Code が「どのファイルに対して
作業しているか」を正確に・リアルタイムに**ワークベンチへ流す。

- ローカルでも SSH 先でも**同じ仕組み**（hooks → OSC → gototerm）。HTTPサーバは立てない
- ファイル監視（notify）はフォールバックとして残る。hooks はそれより正確
  （どのツールが・どのファイルを触ったかが分かる）

体験：
1. `gt init-hooks` を一度実行したプロジェクトで Claude Code を使うと、
   サイドバーヘッダに `● claude: src/main.rs (Edit)` のような**作業中インジケータ**が出る
2. changes 一覧に hooks 由来のイベントが（watcherと合流して）流れる
3. **SSH 先**で `gt view 記事.md` すると、その中身が手元の右上プレビューに表示される
4. SSH 先で `gt init-hooks` すれば、リモートの Claude Code の作業もリアルタイムに手元へ流れる

## 前提（先に読むこと）

- `docs/gt-protocol.md` … プロトコル定義（本仕様の正）
- `src/vt.rs` … Phase 8a で OSC 7717 は**抽出済み・中身は捨てている**。今回パースして流す
- `src/reader.rs` / `src/sidebar.rs` / `src/multiplexer.rs` … 3分割レイアウト（Phase 10 後）
- docs/codex-goals/phase3-live-preview.md の絶対制約

## 実装内容

### 1. OSC 7717 のパース（`src/vt.rs` または新規 `src/gt.rs`）

```rust
pub enum GtMessage {
    Event { kind: ChangeKind, path: PathBuf, tool: Option<String> },
    FileChunk { path: PathBuf, seq: u32, last: bool, data: Vec<u8> },
}
/// "event;kind=mod;path=<b64>[;tool=<b64>]" 等をパース。不正は None（黙って捨てる）。
pub fn parse_gt_message(payload: &str) -> Option<GtMessage>
```

- base64 デコードは自前実装（標準アルファベットのみ・パディング必須・不正は None）。
  依存追加不可。純関数＋テスト（正常 / 不正b64 / キー欠落 / 未知type / 巨大seq）。
- プロトコル拡張：`event` に任意キー `tool=<b64>`（Edit / Write 等のツール名）を追加してよい。
  **docs/gt-protocol.md も同時に更新すること。**
- 受信スレッド（既存の PTY 読取スレッド）でパースし、`VtTerminal` の
  `Arc<Mutex<Vec<GtMessage>>>` に積む。上限（例: 溜まり 1000 件で古い方を捨てる）を設ける。

### 2. ファイル転送の組み立て（純関数＋テスト）

```rust
/// seq=0 で開始、連番で追記、last=1 で完成。パス違い・seq飛び・未完了中の新規開始は破棄。
pub struct GtFileAssembler { ... }
```

- 完成したら `(PathBuf, Vec<u8>)` を返す。合計 64KB 超は破棄（プロトコル上限）。
- テスト：正常2チャンク / seq飛び / 別ファイル割込み / 上限超過。

### 3. 配線（multiplexer.rs）

- `AboutToWait` で**全ペイン**（フォーカス外のペインで Claude が動いていることもある）から
  `GtMessage` を汲む（`for_each_leaf` で take）。
- `Event` → サイドバーの changes へ合流（既存の merge_kind 経路。パスは表示上そのまま使う。
  リモートパスはローカルに存在しなくてよい＝**stat しない**）。
  同時に「最新のAI作業」として保持し、サイドバーヘッダに表示（下記4）。
  Reader が追従モードなら、**ローカルで存在するパスのみ** FilePreview で読む
  （リモートパスは FileChunk が来たときだけ表示できる）。
- `FileChunk` → assembler へ。完成したら ReaderPane に **バイト列から直接**表示
  （`tail_lines` / Markdown 整形の既存経路を通す。ヘッダにはパスと `(remote)` 印。
  ピン留め扱いにする（勝手に切り替わらない）。[編集] はローカルに実体が無いので出さない。
  [OSの既定アプリで開く] も出さない）。

### 4. AI作業インジケータ（sidebar.rs）

- サイドバーヘッダ（workspace/git 行の下）に1行：
  ```
   ● claude  MOD src/main.rs (Edit)
  ```
  - 最新の `GtMessage::Event` を表示。60秒イベントが無ければ消す（Instant 比較。タイマー不要、
    再描画時に判定）
  - ● は Color::Green。パスは幅に合わせ省略（既存 abbreviate_start）
- この行のクリック / 一覧での Enter は不要（表示のみ。次フェーズで検討）。

### 5. `gt` スクリプト（`assets/bin/gt`・POSIX sh）

`docs/gt-protocol.md` のとおり。実装要点：

- 依存は `base64`・`printf`・`dd`（または `tail -c`）程度。bash 専用機能を使わない
- 出力先は必ず `/dev/tty`（hooks の stdout は Claude Code に食われる）。tty が無ければ何もせず exit 0
- `gt view <file>`：末尾 64KB を 8KB 生データずつチャンクして file メッセージ送出
- `gt event <new|mod|del> <path> [tool]`：event メッセージ送出
- `gt hook`：stdin の Claude Code hook JSON から `tool_name` と `tool_input.file_path` を抽出し
  event を送出（`jq` があれば使い、無ければ sed/grep でフォールバック）。file_path が無い
  ツール（Bash等）は何もしない。**続けて `gt view` 相当で内容も送る**（mod/new のとき）
- `gt init-hooks`：カレントに `.claude/settings.local.json` を作成/追記して
  PostToolUse（matcher: Edit|Write|MultiEdit|NotebookEdit）で `gt hook` を呼ぶ設定を書く。
  既存ファイルがある場合は**上書きせず**、追記が必要な旨と手動マージ用スニペットを表示して exit 1
- シェバンは `#!/bin/sh`。実行権限付与。README に導入手順
  （ローカル: PATH に置く／リモート: `scp assets/bin/gt remote:~/bin/` 等）を追記

### 6. セキュリティ（プロトコル文書の方針を厳守）

- 受信内容は**表示専用**。ディスクに書かない・実行しない・パスを stat しない（リモートパス）
- 不正メッセージは黙って捨てる（ログは debug レベルのみ）

## 制約

- 新規依存なし（base64 も自前）。
- 受信スレッドの処理は軽量に。UI スレッドで I/O しない（FileChunk の表示はメモリ上のバイト列から）。
- ワークベンチOFF時は GtMessage を捨てるだけ（コスト最小）。
- 無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。コメントは日本語で「なぜ」。

## 完了条件（この順で検証）

1. `cargo test` 全パス（parse_gt_message / GtFileAssembler / base64 の新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. `sh -n assets/bin/gt` が通る（構文チェック）。
4. 手動確認手順を `docs/codex-goals/phase8b-verify.md` に出力：
   ローカルで `printf` による event/file メッセージ手打ち → ヘッダに ● claude が出る／
   Reader に内容が出る → `gt view README.md` で同じことが起きる →
   ssh 先（無ければ `ssh localhost`）で `gt view` → 手元に表示される →
   `gt init-hooks` 後に Claude Code で編集 → インジケータと changes に流れる。
