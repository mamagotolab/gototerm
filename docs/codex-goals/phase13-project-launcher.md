# Codex Goal: Phase 13 — プロジェクトランチャー

## ゴール

「gototerm を開いたが、目的のフォルダへどう移動するのか分からない」という
`cd の壁` を消す。最近使ったプロジェクトを一覧から選ぶ／パスを入力する画面
（ランチャー）をオーバーレイ表示し、選んだフォルダを cwd にした**新しいタブ**を開く。

非エンジニア寄りの「Terminal へ一歩踏み出した人」が対象。黒い画面に放り込まず、
「どこで作業する？」を最初に見せる。

## 前提（先に読むこと）

- `docs/workbench-v2.md` … アーキ3判断。特に「**第2描画スタック禁止**（全ウィジェットは
  TerminalView のセルグリッド描画）」。ランチャーもセルグリッドで描く。
- `src/multiplexer.rs`
  - `Multiplexer::new`（685行〜）… 最初のペインは常に通常どおり spawn する（**startup 経路は変えない**）。
  - `status_view` / `Sidebar` … 「タブツリーの外の特別ビュー」の前例。ランチャーも同じく
    Multiplexer 直下のオーバーレイとして持つ。
  - タブ追加（`Ctrl+Shift+T` 経路、937行付近の `Node::Leaf(Box::new(TerminalWindow::with_viewport(...)))`）
    … **選択確定時はこの経路で cwd 付きの新タブを開く**。
- `src/window.rs` の `TerminalWindow::with_viewport(window, display, viewport, cwd)`
  … cwd を渡せる。ランチャーはこれを使う。
- `src/keybindings.rs` / `src/config.rs` … v0.3.5 でキーバインドは config 化済み。
  新アクション `open_launcher` を追加し、既定 `Ctrl+Shift+N` を割り当てる（$mod=SUPER なので空き）。
- `src/main.rs` の `crash_log_path()` … cache ディレクトリ解決の実例。recent 保存先の
  ベースディレクトリ解決はこれと同じ方式（Win=`%LOCALAPPDATA%`、他=`$XDG_CACHE_HOME` or `~/.cache`）。

## 実装内容

### 1. 新規 `src/recent.rs` — 最近使ったプロジェクトの永続化（純ロジック中心）

```rust
pub struct RecentProjects { /* Vec<PathBuf> を新しい順で保持、上限 20 */ }

impl RecentProjects {
    pub fn load() -> Self;              // ファイルが無ければ空
    pub fn record(&mut self, path: &Path);  // 既存は先頭へ移動（dedup）、上限で切り詰め
    pub fn entries(&self) -> &[PathBuf];
    fn save(&self);                    // record 内で呼ぶ
}
```

- 保存先: `<cache>/gototerm/recent_projects.json`（ベースは `crash_log_path()` と同じ解決）。
- 形式は JSON 配列（パス文字列のみ。日時は持たない＝順序だけで十分）。**serde_json を使わず、
  1行1パスのプレーンテキスト**でよい（依存を増やさない。改行区切り・UTF-8・trim）。
  ※テキストにするなら拡張子は `.txt`（`recent_projects.txt`）にすること。
- **`record` は純ロジックとしてユニットテスト必須**（先頭移動・重複排除・上限20・存在しないパスも
  そのまま保持＝記録時に stat しない）。
- 記録タイミング: **cwd を持つペイン（タブ／分割）を新規 spawn したとき**、その cwd を `record`。
  起動時の最初のペインの cwd も記録する。`multiplexer.rs` の spawn 箇所から呼ぶ。

### 2. 新規 `src/launcher.rs` — オーバーレイ UI

```rust
pub enum LauncherOutcome {
    OpenIn(std::path::PathBuf),  // このフォルダで新タブを開く
    Cancelled,                    // Esc（何もしない）
    None,                         // まだ操作中
}

pub struct Launcher { /* TerminalView, 選択インデックス, 入力モード, ... */ }

impl Launcher {
    pub fn new(display: Display, viewport: Viewport, recent: &[PathBuf]) -> Self;
    pub fn set_viewport(&mut self, vp: Viewport);
    pub fn draw(&mut self, surface: &mut Frame);
    /// キー入力を処理。確定/キャンセルを返す。
    pub fn handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> LauncherOutcome;
}
```

画面イメージ（セルグリッドで描画。中央寄せの矩形、周囲は既存背景を暗くする等はしなくてよい＝全面塗り）:

```
  gototerm

  最近使ったプロジェクト                （↑↓ で選択 / Enter で開く）

  > toyterm                 ~/work/programs/toyterm
    fuchu-compass           ~/work/programs/fuchu-compass
    freee-crowdworks-gas    ~/work/programs/...

  ─────────────────────────────
  [ この場所で開く ]  現在: ~/work
  [ パスを入力 ]        i キー

  Esc で閉じる
```

- **キー操作**（[[操作の二重化原則]]に沿い、まずキーボードを完備。マウスは v2）:
  - `↑`/`↓`（および `k`/`j`）… recent 行と下部アクション行を一続きに移動。
  - `Enter` … 選択中の項目を確定 → `OpenIn(path)`。「この場所で開く」は現在の cwd。
  - `i` … パス入力モード（下部に1行のテキスト入力。`~` 展開・入力中はカーソル表示）。
    Enter で `OpenIn(入力パス)`、Esc で入力モード解除。
  - `Esc` … `Cancelled`。
- **フォルダ選択（重要な設計判断）**: v1 は **recent 一覧 ＋ パス入力** で完結させる。
  ネイティブのフォルダ選択ダイアログ（rfd 等）は**入れない**（Linux で portal 依存が増え
  「exe 単体完結」方針に反する）。内蔵ミニファイラ（sidebar 側）の再利用も v1 では見送り、
  「i でパス入力」で代替する。将来 v2 で検討。
- **存在チェック**: 確定した OpenIn のパスが存在しないディレクトリなら、画面下にエラー
  （赤字1行「そのフォルダは見つかりません」）を出して確定を無効化。ファイルを指した場合は
  その親ディレクトリを cwd にする。

### 3. `src/multiplexer.rs` への配線

- フィールド追加: `launcher: Option<Launcher>`、`recent: RecentProjects`。
- 新アクション `open_launcher`（既定 `Ctrl+Shift+N`）で `launcher = Some(Launcher::new(..., self.recent.entries()))`。
- ランチャー表示中は:
  - **キーイベントはランチャーが独占**（既存ペイン／サイドバーへは流さない）。
  - 描画は既存レイアウトの**上に**ランチャーの `draw` を最後に呼ぶ（全面オーバーレイ）。
  - `handle_key` が `OpenIn(path)` を返したら → **cwd 付きの新タブを開く**（既存のタブ追加経路を
    関数化して `open_tab_in(cwd)` にし、`Ctrl+Shift+T` もこれを使う）→ `record(path)` → `launcher=None`。
  - `Cancelled` → `launcher=None`（元の画面に戻る）。
- `set_viewport`/resize はランチャーにも伝播。
- **startup 経路は変更しない**（最初のペインは常に spawn）。将来 config `show_launcher_on_start`
  で「起動直後にオーバーレイ」を足せる余地は残すが、**今回は実装しない**（Super+Enter で頻繁に
  開くヘビーユーザーを邪魔しないため。既定は「キーで開く」だけ）。

### 4. `src/config.rs` — 追加不要（startup 表示は今回やらない）

キーバインドの追加のみ（keybindings.rs 側）。

## テスト（省略禁止）

- `recent.rs`: `record` の純ロジック（先頭移動 / 重複排除 / 上限20 / 空ロード / 存在しないパス保持）。
- `launcher.rs`: `handle_key` のロジックを、描画を伴わずテストできる形にする
  （選択 index の上下移動が端で止まる／`Enter` が正しい `OpenIn` を返す／`Esc` が `Cancelled`／
  存在しないパスで確定が無効化される）。`Launcher::new` に描画非依存の内部状態を分離できるなら分離する。
- 既存テストが全て緑のままであること。

## 制約（厳守）

- **第2描画スタックを作らない**。ランチャーは TerminalView のセルグリッドで描く。
- **依存を増やさない**（rfd・serde_json 等を足さない。recent はプレーンテキスト）。
- **startup の挙動を変えない**（最初のペインは従来どおり spawn。ランチャーはキーで開くオーバーレイ）。
- **cargo fmt は自分が触ったファイルだけ**にかける（リポジトリ全体に fmt をかけない）。
- 新しい config フィールドを足す場合は必ず `#[serde(default)]` を付ける（起動クラッシュ防止）。
- 検収に**起動スモークテスト**（`timeout 2 gototerm` で panic なし）を含める前提で実装する。
