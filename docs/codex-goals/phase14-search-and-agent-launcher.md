# Codex Goal: Phase 14 — ランチャーに「絞り込み検索」と「AIエージェント選択」を追加

## 背景（現状のランチャー）

`src/launcher.rs` は既に **yazi 風の2ペインのディレクトリブラウザ**として動いている。
`Ctrl+Shift+N`（および config `show_launcher_on_start=true` で起動時）に開く。

- `LauncherState` は `Mode::{Browse, Recent}` を持つ。
- `Browse`: `dir` の中身を `entries: Vec<Entry>`（`Entry{ name, is_dir }`、先頭に `..`）で表示。
  左＝一覧、右＝選択中フォルダのプレビュー。
  操作: `j/k`・`↑↓`=移動 / `l`・`→`=中へ / `h`・`←`=上へ / `.`=隠し切替 / `r`=recent / `Enter`=開く / `Esc`=閉じる。
- `Recent`: 最近使ったプロジェクト一覧。
- 確定は `LauncherOutcome::OpenIn(PathBuf)` を返し、`src/multiplexer.rs` の
  `handle_launcher_outcome` が `open_tab_in(Some(&path))` で **そのフォルダを cwd にした新タブ**を開く。
- 色は Tokyo Night（フォルダ青 `BrightBlue`、暗色 `DIM=#565F89`、選択は白バー、背景 `panel_bg_color()`）。

この2機能を足す。**既存のブラウザ挙動・見た目・既存テストは壊さないこと。**

---

## 機能1: 絞り込み検索（`/`）

「ホームから `work` へすぐ飛べない」を解消する。yazi の filter 相当。

- `Browse` モードで `/` を押すと**絞り込みモード**に入る（`LauncherState` に
  `filter: Option<String>` を追加。`Some("")` で開始）。
- 絞り込み中の挙動:
  - **文字入力**（`event.text` の制御文字以外）→ クエリに追記。
  - **Backspace** → クエリ末尾を削除。空の状態でさらに Backspace → 絞り込み解除（`filter=None`）。
  - **`↑↓`** → 一致した項目間を移動（`j/k` は文字として打てるようにクエリ側へ回す＝移動は矢印のみ）。
  - **`Enter`** → 選択中を開く（通常の open と同じ）。
  - **`l`/`→`** → 選択中フォルダへ潜る。潜ったら `filter=None` にリセット。
  - **`Esc`** → 絞り込み解除（全件表示に戻る。閉じない）。
- 一致判定は **大文字小文字を無視した部分一致**（`name.to_lowercase().contains(&query.to_lowercase())`）。
  `..` は絞り込み中は除外してよい。
- 表示: 絞り込み中は**一致した項目だけ**を左ペインに出す。選択は先頭に合わせ、クエリを
  どこかに見せる（例: ヘッダ2行目のパスの右か、フッタに `検索: <query>_`）。色は `DIM`。
- `dir` を移動（descend/ascend）したら `filter=None` に戻す。
- **純ロジックのテスト**: 与えたエントリ列とクエリから一致リストを返す関数（例
  `fn filter_entries(entries: &[Entry], query: &str) -> Vec<usize>` のような純関数）を切り出して
  ユニットテスト（大小無視・部分一致・空クエリで全件・`..`除外）。

---

## 機能2: AIエージェント選択

フォルダを選んだあと、「Claude Code / Codex / Gemini / そのまま作業（シェル）」を選んで、
**その場所でそのコマンドを起動**する。

### 2-1. 設定（`src/config.rs`）

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AgentDef {
    pub name: String,          // 表示名（例: "Claude Code"）
    pub command: Vec<String>,  // 実行コマンド（例: ["claude"]）
}
```

`Config` に追加（`#[serde(default = "default_agents")]`）:

```rust
pub launcher_agents: Vec<AgentDef>,
```

`default_agents()`:

```rust
fn default_agents() -> Vec<AgentDef> {
    vec![
        AgentDef { name: "Claude Code".into(), command: vec!["claude".into()] },
        AgentDef { name: "Codex".into(),       command: vec!["codex".into()] },
    ]
}
// ※ Gemini は既定に入れない（利用者が少ないため）。使う人は config で追加する。
```

- **`Config::default()`（リテラル構築）にも `launcher_agents: default_agents()` を必ず足す**
  （足さないとコンパイルエラー）。
- config-rs の空 Vec バグ回避のため、default は**非空**であること（上記は非空なので OK）。

### 2-2. ランチャー（`src/launcher.rs`）

- `LauncherOutcome::OpenIn(PathBuf)` を **`OpenIn { dir: PathBuf, command: Option<Vec<String>> }`**
  に変更（`command=None`＝通常シェル）。
- `Mode` に `Agent` を追加。フォルダ確定時に**すぐ OpenIn を返さず**、`Agent` モードへ遷移する:
  - `Browse` の `Enter`／`Recent` の `Enter` で決まった対象ディレクトリ（`open_target` 相当で
    解決したフォルダ）を `chosen_dir: Option<PathBuf>` に保持し、`mode = Mode::Agent` にする。
  - ファイルを選んだ場合は従来どおりその親ディレクトリを対象にする。
- `Agent` モードの表示（TN 配色・選択は白バー）:
  ```
  gototerm — 何で開く？
  ~/work/programs/toyterm

  > そのまま作業（シェル）
    Claude Code
    Codex

  j/k:選択  Enter:起動  Esc:戻る
  ```
  - 一覧の**先頭は必ず「そのまま作業（シェル）」**（`command=None`）。**既定の選択位置は先頭**
    （＝フォルダ確定→Enter で素早くシェルが開く）。以降に config の `launcher_agents` を並べる。
  - `j/k`・`↑↓`=移動 / `Enter`=起動（`OpenIn{ dir, command }` を返す。シェルは `command=None`、
    エージェントは `Some(agent.command.clone())`） / `Esc`=`Browse` に戻る（`chosen_dir` を捨てる）。
- config の一覧は launcher から `crate::TOYTERM_CONFIG.launcher_agents` を直接読んでよい
  （font_size を読んでいるのと同じ要領）。

### 2-3. 配線（`src/multiplexer.rs`）

- `open_tab_in(&mut self, cwd: Option<&Path>)` を
  **`open_tab_in(&mut self, cwd: Option<&Path>, command: Option<&[String]>)`** に拡張し、
  `TerminalWindow::with_viewport_command(..., cwd, command)` を使う。
  `Action::NewTab` の呼び出しは `open_tab_in(None, None)` にする。
- `handle_launcher_outcome` の `OpenIn` を新シグネチャに合わせて分解し、
  `open_tab_in(Some(&dir), command.as_deref())` を呼ぶ。
  **起動時ランチャーで最初の空タブを畳む既存ロジック（`startup_launcher`）はそのまま維持**すること。

---

## テスト（省略禁止）

- 機能1: 絞り込みの純関数（大小無視・部分一致・空クエリ全件・`..`除外）。
- 機能2:
  - `Browse` で `Enter` → `Mode::Agent` に入り `chosen_dir` が対象フォルダになる。
  - `Agent` 先頭（そのまま作業）を `Enter` → `OpenIn{ dir, command: None }`。
  - `Agent` で1つ下（Claude Code）を選び `Enter` → `OpenIn{ dir, command: Some(vec!["claude"]) }`。
  - `Agent` で `Esc` → `Browse` に戻る。
- 既存の launcher テスト（read_entries 並び・descend/ascend・Enter で開く・recent）は
  **新シグネチャに直しつつ全て緑のまま**にする。
- `cargo test` と `cargo build` を通し、緑を確認して報告する。

## 制約（厳守）

- 既存のブラウザ挙動・Tokyo Night 配色・キー体系を壊さない。
- **依存を増やさない**。
- 新しい config フィールドは `#[serde(default)]`（または `default =`）必須。`Config::default()` の
  リテラルにも対応フィールドを足す。
- **`cargo fmt` は自分が触ったファイルだけ**にかける（リポジトリ全体にかけない）。
- 検収で**起動スモークテスト**（`timeout 2 gototerm` で panic なし）を通す前提で実装する。
- 完了したら、変更/追加ファイル一覧・追加テスト名・`cargo test` 結果を報告する。
