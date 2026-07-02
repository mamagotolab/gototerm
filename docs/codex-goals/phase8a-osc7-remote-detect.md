# Codex Goal: Phase 8a — OSC 7 受信とリモート検知（SSH対応の土台）

## ゴール

`docs/gt-protocol.md` の受信側のうち **OSC 7（cwd通知）** を実装する。

1. PTY バイト列から OSC 7 を抽出し、シェルの cwd（ローカル/リモート）を把握する
2. リモート接続中はサイドバーが「リモート表示」に切り替わる（誤ったローカル情報を出さない）
3. ローカルでは OSC 7 があれば /proc より優先して cwd 追従に使う（Windows の cwd 追従がこれで動くようになる）

OSC 7717（file/event）は次の Phase 8b。ただし splitter は 7717 も**抽出だけ**は行い、
中身は今回は捨ててよい（パーサの器だけ用意）。

## 前提（先に読むこと）

- `docs/gt-protocol.md` … プロトコル設計（本仕様の正）
- `src/vt.rs` の `SixelSplitter` … alacritty 前段でシーケンスを分離する既存パターン。
  チャンクまたぎ対応の状態機械。これを一般化する
- `src/window.rs` の `pane_cwd()` / `src/sidebar.rs` の cwd 追従・watcher ライフサイクル
- docs/codex-goals/phase3-live-preview.md の絶対制約＋config新フィールドは serde(default) 必須

## 実装内容

### 1. `vt.rs`: SixelSplitter の一般化

- 既存の Sixel(DCS) 分離に加えて、**OSC 7 と OSC 7717** を抽出する
  （`ESC ] 7 ;` … `BEL` または `ESC \`）。それ以外の OSC は従来どおり素通しで alacritty へ。
- 抽出は純粋な状態機械のまま保つ（チャンクまたぎ・BEL/ST 両終端・最大1MBで破棄）。
- 単体テスト：1チャンク完結 / 2チャンクまたぎ / BEL終端 / ST終端 / 他のOSC(例: OSC 0 タイトル)は素通し /
  1MB超は破棄 / Sixel との混在。

### 2. OSC 7 のパース（純関数＋テスト）

```rust
/// "file://host/path%20with%20space" → (host, PathBuf)。不正なら None。
fn parse_osc7(payload: &str) -> Option<(String, PathBuf)>
```

- percent デコードは自前実装でよい（依存追加不可。%XX のみ・不正は None）。
- テスト：ホストあり / ホスト空 / 日本語パス(percent encoded) / 不正 URI。

### 3. 位置の共有とリモート判定

- `VtTerminal` に共有状態を追加：
  ```rust
  pub enum ShellLocation {
      Local(PathBuf),                       // OSC 7 でローカルと判定
      Remote { host: String, path: PathBuf },
  }
  // Arc<Mutex<Option<ShellLocation>>> を読取スレッドが更新
  ```
- ローカル判定：host が空 / "localhost" / 自ホスト名と一致。
  自ホスト名は起動時に1回取得してキャッシュ：Linux は `/proc/sys/kernel/hostname`、
  Windows は env `COMPUTERNAME`、失敗時は空（＝host空のみローカル扱い）。純関数＋テスト。
- `TerminalWindow::pane_cwd()` を `pane_location() -> ShellLocation` に発展させる：
  - OSC 7 の報告があればそれを最優先
  - 無ければ従来の /proc 方式で `Local(...)`（Windows は起動ディレクトリ）
  - 既存の呼び出し箇所（サイドバー cwd・分割時の cwd 継承）は `Local` のときだけ
    パスを使い、`Remote` のときは従来のフォールバック（起動ディレクトリ等）でよい

### 4. サイドバーのリモート表示

- フォーカスペインが `Remote { host, path }` のとき：
  - ヘッダ1行目を ` remote  user@host のパス表示` 相当に（`host:path`、末尾省略は既存関数）
  - **watcher・files ブラウザ・changes を停止**し、本文領域に案内を表示：
    ```
    リモート接続中 (host)
    ローカルのファイル監視・一覧は
    このペインでは使えません。

    リモートの内容を見るには（Phase 8b で対応予定）:
      gt view <file>
    ```
  - watcher が動いていたら drop する（ローカルに戻ったら通常の経路で再生成される）
- ローカルに戻った（OSC 7 がローカル or ssh 終了）ら従来表示に自動復帰。

### 5. シェル統合スニペット（ドキュメントのみ）

- README に「SSH・cwd 追従（OSC 7）」の節を追加：
  - bash: `PROMPT_COMMAND` で `printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"`
  - zsh: `precmd` 同上
  - fish: 多くの環境で標準発行される旨と、されない場合の関数例
- スニペットファイル `assets/shell-integration/osc7.bash` / `osc7.zsh` / `osc7.fish` として同梱。

## 制約

- 新規依存なし。
- 読取スレッド内の処理は軽量に（ロック保持を短く。パースは受信スレッドで完結してよい）。
- 既存の Sixel・通常VTの挙動を変えない（回帰注意。既存の splitter テストが全て通ること）。
- 無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。コメントは日本語で「なぜ」。

## 完了条件（この順で検証）

1. `cargo test` 全パス（splitter拡張 / parse_osc7 / ホスト名判定の新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase8a-verify.md` に出力：
   `printf '\033]7;file://%s%s\033\\' "$(cat /proc/sys/kernel/hostname)" "$PWD"` → ローカル扱いで cwd 追従 →
   `printf '\033]7;file://otherhost/home/naoto\033\\'` → サイドバーがリモート表示に切替 →
   `printf '\033]7;file://%s%s\033\\' "$(cat /proc/sys/kernel/hostname)" "$PWD"` → ローカル表示に復帰 →
   yazi の画像表示・タイトル変更(OSC 0)等が従来どおり動く。
