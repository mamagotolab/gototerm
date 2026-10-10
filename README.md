# gototerm

**日本語入力のストレスが少ない、AI開発ワークベンチ付き軽量ターミナル。**

gototerm（ゴトターム）は、日本語を打つ人のために作られたターミナルです。
変換中の文字がカーソル位置にそのまま表示され、変換候補も入力位置に出る ——
「いつもの端末は日本語入力がもたつく・ズレる」という小さなストレスを減らすことを第一に設計しています。

さらに `Ctrl+Shift+F` ひとつで、**ファイル一覧・プレビュー・ターミナルの3分割ワークベンチ**に切り替わります。
Claude Code などの AI コーディングツールが「いま・どのファイルを・どう変えているか」を、
隣のペインでリアルタイムに眺めながら作業できます。

> 名前は、プログラミングの `goto` と、開発元 [mamagotolab](https://github.com/mamagotolab) に由来します。

> [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)（MIT License）をベースに、
> モダンな Wayland 環境への対応と日本語入力まわりを大きく作り直したフォークです。

---

## なぜ gototerm か

- **日本語入力が素直** — 変換中の文字（preedit）を端末内のカーソル位置にインライン表示。変換候補もカーソルに追従（fcitx5 等の Wayland text-input-v3 に対応）。
- **AIの作業が見える** — Claude Code がファイルを書くそばから、中身が隣のペインに流れる。ファイル名の出力はクリックで即プレビュー。
- **描画資源を必要時に生成** — 非表示のワークベンチ描画資源は、初回表示まで遅延初期化。Windowsのメモリは[下記の手順](#メモリの測定)で実測します。
- **半透明＋ぼかし対応** — 背景の不透明度を細かく指定でき、Wayland コンポジタのブラーと相性良し。
- **全角幅に配慮** — East Asian Width（曖昧幅）を設定で切り替え可能。日本語の表組みが崩れにくい。
- **完全な VT 互換** — VT エンジンに [alacritty_terminal](https://crates.io/crates/alacritty_terminal) を採用。nvim・Claude Code 等の高機能 TUI も正しく描画。
- **タブ・画面分割・Sixel 画像** — 端末内で画像表示（yazi のプレビューなど）も可能。

---

## インストール

### 🪟 Windows（ビルド不要・おすすめ）

[**Releases ページ**](https://github.com/mamagotolab/gototerm/releases/latest) から
`gototerm-windows-x64.zip` をダウンロードし、**すべて展開**してから、中の `gototerm.exe` を起動してください。
更新・移動するときも、`conpty.dll` と `OpenConsole.exe` を含むフォルダ全体を使ってください。
これらはWindows上でnvimなどのスクロール情報を保つための同梱ファイルです。
単体の `gototerm-windows-x64.exe` も残していますが、同梱ファイルなしではOSによってコピー継続が停止します。

- 設定ファイル（任意）は **`%APPDATA%\gototerm\config.toml`**。無くても内蔵フォントで動きます。
- 設定例は [`config.windows.example.toml`](./config.windows.example.toml) を参照（フォント・サイズ・配色・透過）。
- フォントは**ファイルの絶対パス**で指定します。Nerd Font 等を使う場合、実ファイルのパスは
  PowerShell で確認できます:

  ```powershell
  Get-ChildItem -Path C:\Windows\Fonts, "$env:LOCALAPPDATA\Microsoft\Windows\Fonts" -Filter "JetBrainsMono*" | % { $_.FullName -replace '\\','/' }
  ```

  個人インストールしたフォントは `%LOCALAPPDATA%\Microsoft\Windows\Fonts\` 配下にあります。

> 起動しない場合は、[Microsoft Visual C++ 再頒布可能パッケージ](https://aka.ms/vs/17/release/vc_redist.x64.exe)
> を入れてください（多くの PC には既に入っています）。

### Linux（ソースからビルド）

```sh
git clone https://github.com/mamagotolab/gototerm.git
cd gototerm
cargo install --path .
```

必要なシステムライブラリ（Arch の例）:
`sudo pacman -S freetype2 fontconfig wayland libxkbcommon cmake`
（Debian/Ubuntu 系は `libfreetype6-dev libfontconfig1-dev libwayland-dev libxkbcommon-dev cmake`）

### Windows でソースからビルドする場合

Rust（MSVC ツールチェイン）・Visual Studio の C++ ビルドツール・CMake が必要です。
**PowerShell** で（`set` ではなく `$env:` で環境変数を渡す点に注意）:

```powershell
git clone https://github.com/mamagotolab/gototerm.git
cd gototerm
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"   # 新しいCMakeが同梱FreeTypeの古いポリシーを拒否するため
cargo build --release
& ./.github/scripts/install-conpty.ps1 -Destination target/release
# 起動: target\release\gototerm.exe（同じフォルダのDLL・OpenConsole.exeも必要）
```

> もし「couldn't determine visual studio generator」で止まる場合は、
> `winget install Ninja-build.Ninja` で Ninja を入れ、`$env:CMAKE_GENERATOR = "Ninja"` も足してください。

> ℹ️ **terminfo の導入は不要です。**
> 内部の VT エンジンに [alacritty_terminal](https://crates.io/crates/alacritty_terminal)
> を採用し、`TERM=xterm-256color` として動作します。

---

## プロジェクトランチャー

`Ctrl+Shift+N` で、開く場所とツールを選ぶランチャー（yazi 風のファイルブラウザ）が開きます。
`cd` を打たずに、目的のフォルダへ移動してターミナルやAIツールを起動できます。

| キー | 動作 |
|---|---|
| `j`/`k`・`↑`/`↓` | 移動 |
| `l`/`→` | フォルダの中へ |
| `h`/`←` | 上のフォルダへ |
| `/` | 絞り込み検索（大文字小文字を無視・部分一致） |
| `.` | 隠しファイル（ドットファイル）の表示切替 |
| `r` | 最近使ったプロジェクト一覧 |
| `m` | ブックマークの登録／解除（★が付きます） |
| `b` | ブックマーク一覧（よく使うフォルダへ一発で移動） |
| `Enter` | フォルダ＝開く（下記の選択ポップアップへ）／ファイル＝エディタや既定アプリで開く |
| `o` | 選択中のファイルを OS の既定アプリで開く |
| `Esc` | 閉じる |

`Enter` の動作は選んでいるものによって変わります。

- **フォルダ**（`../` 含む）— そのフォルダを開く（ツール選択ポップアップへ）
- **テキストファイル**（Markdown・ソースコードなど）— エディタで開く（新しいタブ・そのフォルダを作業ディレクトリに）。
  エディタは `config.toml` の `editor` → 環境変数 `$EDITOR` → `nvim`（Windows は `notepad`）の順で決まります
- **画像・PDF・圧縮ファイルなど** — OS の既定アプリで開く（ランチャーは開いたまま）

テキストでも `o` を押せば既定アプリで開けます（Markdown をビューアで見たいときなど）。

### ブックマーク（よく使うフォルダ）

よく開くプロジェクトは `m` で登録しておくと、`b` の一覧から一発で飛べます。一覧では `Enter` でそこを開き、
`l` でそこへ移動して中を見られます。`m`／`d` で外せます。登録先は `~/.config/gototerm/bookmarks.txt`
（Windows は `%APPDATA%\gototerm\bookmarks.txt`）で、テキストなので直接編集もできます。

フォルダを選ぶと「そのまま作業（シェル）／ Claude Code ／ Codex …」を選ぶポップアップが出ます。
AIツールを選ぶと、そのフォルダで起動し、**抜けるとそのフォルダのシェルに戻ります**（タブは残ります）。

- 起動時にランチャーを出す挙動は**既定で ON**（`config.toml` で `show_launcher_on_start = false` にすると、従来どおり起動直後にシェルが出ます）。
- 選べるツールは `config.toml` の `launcher_agents` で追加・変更できます（既定は Claude Code と Codex）。
- `gototerm <フォルダ>` のようにパスを引数で渡すと、そのフォルダを作業ディレクトリにして起動します。

---

## 作業状態の一覧

`Ctrl+Shift+A` で、同じウィンドウ内にある全タブの通常端末ペインを一覧できます。
1行が1ペインで、通知されたツール名と「最後に受け取った状態」を表示します。
通知のないシェルは「状態通知なし」と表示し、実行中か終了済みかを推測しません。
「応答終了」はツールから最後の応答終了通知を受け取ったという意味で、作業の成功や完了を保証しません。

`↑` / `↓`、`PageUp` / `PageDown`、`Home` / `End` で選び、`Enter` でそのペインへ移動します。
`Esc` またはもう一度 `Ctrl+Shift+A` で、移動せず元の領域へ戻ります。

状態は一覧を閉じている間もペインごとに保持します。ただし、ワークベンチの変更履歴は通常ペインを
切り替えた時点で仕切り直し、背景ペインで届いた履歴を後から再生しません。
プレビュー枠で一時的に開いたエディタは一覧対象外です。

WindowsネイティブのClaude Code/Codexは、[helperの設定](#windowsのclaude-code--codex状態通知)で状態通知を使えます。
WSL内では、[下記の `gt` 導入手順](#claude-code-と連携する)に従ってください。

---

## ワークベンチ（3分割モード）

### クイックスタート — 3分割を試す

1. gototerm を開いて `Ctrl+Shift+F` を押す
2. 画面が3つに分かれます

```
┌─────────────┬───────────────────────────────┐
│ ファイル一覧 │ プレビュー                     │
│ (files /    │ 選んだファイル、または          │
│  changes)   │ AIがいま書いているファイルの中身 │
│             ├───────────────────────────────┤
│ ↑↓ と ←→   │ ターミナル                     │
│ で移動      │ （ここで Claude Code などを実行）│
└─────────────┴───────────────────────────────┘
```

3. 矢印キーでファイルを選び、`Enter`（または `→`）で右上に中身が表示されます
4. もう一度 `Ctrl+Shift+F` を2回押すと、元の全画面ターミナルに戻ります

`Ctrl+Shift+F` は押すたびに「開いて一覧を操作 → ターミナルに居るときは一覧へフォーカス → 閉じる」と巡回します。
`Esc` でいつでもターミナルに戻れます。

### ファイル一覧の操作

キーボードとマウス、どちらでも同じことができます。

| したいこと | キーボード | マウス |
|---|---|---|
| 選択を移動 | `j` `k`（`↑` `↓` `PageUp/Down` `Home/End` も可） | — |
| 一覧をスクロール | `j` `k`（自動追従） | ホイール |
| フォルダに入る / ファイルを開く | `l`（`→` `Enter` も可） | クリック |
| 親フォルダへ戻る | `h`（`←` `Backspace` も可） | `../` をクリック |
| 頭文字で探す（多い一覧向け） | `/` で検索→文字入力→`Enter`/`Esc` | — |
| files → log → changes 切り替え | `Tab` | 切替行をクリック |
| プレビューをスクロール | `PageUp` / `PageDown` | プレビュー上でホイール |
| 開いたファイルを編集 | `e` | `[編集: nvim]` をクリック |
| OS の既定アプリで開く | `o` | `[OSの既定アプリで開く]` をクリック |
| ターミナルへ戻る | `Esc` | ターミナルをクリック |

- 一覧のフォーカス中は選択行が反転表示され、最下行に操作ヒントが出ます。
- フォーカス中に打った文字がシェルに流れることはありません。

### プレビュー（右上）の操作

長いファイルはプレビュー自体にフォーカスを移して読めます。ターミナルから `Ctrl+Shift+K`（上へフォーカス）で移動します。

| したいこと | キーボード | マウス |
|---|---|---|
| プレビューへフォーカス | `Ctrl+Shift+K`（ターミナルから上へ） | プレビューをクリック |
| スクロール | `j` `k`（`↑` `↓` `PageUp/Down` `Home/End` も可） | ホイール |
| 編集 / 既定アプリで開く | `e` / `o` | 見出しのボタンをクリック |
| ターミナルへ戻る | `Esc`（`Ctrl+Shift+J` も可） | ターミナルをクリック |
| ファイル一覧へ移る | `Ctrl+Shift+H` | 一覧をクリック |

- **プログラムファイルは色つき＋行番号**で表示します（Tokyo Night 配色）。文法が分からない拡張子は素のテキストになります。
  末尾だけ読んだ大きいファイル（64KB超）は、先頭が1行目とは限らないので行番号を出しません。
- **Markdown は整形して表示**します。見出しは `■`（大）`◆`（中）`▸`（小）で色分け、表は列を揃えて罫線を引き、
  ` ```rust ` のようなコードブロックは言語ごとに色を付けます。
- フォーカス中は最下行に操作ヒントが出ます。一覧と同じく、打った文字はシェルに流れません。
- AI の書き込みを追いかけている最中にスクロールすると、その位置で固定されます（`tail -f` を上へ辿るのと同じ）。
  固定中は見出しに「クリックで追従に戻る」と出るので、そこをクリックすれば追従に戻ります。

### 3つのモード：files・changes・log

- **files** … いま居るフォルダのファイル一覧。フォルダを潜って探せます（シェルで `cd` すると一覧も付いてきます）。
- **changes** … 作成・変更・削除されたファイルが新しい順に流れます。`NEW`（緑）/ `MOD`（黄）/ `DEL`（赤）のバッジ付き。
  AI にコードや記事を書かせているとき、何が起きているかを一覧で把握できます。
- **log** … **AI 作業タイムライン**。「何分前に・どのファイルが・どう変わったか」を時系列で記録します。

### log モード（AI 作業タイムライン）

AI に任せた作業を、あとから時系列で追えるモードです。

```
 作業ログ  ▲ 要確認 2件
 いま   NEW  src/timeline.rs (Write)
 ─ セッション開始 ─
 2分    MOD  src/main.rs (Edit)
 5分    MOD  Cargo.toml ▲依存
 8分    DEL  old_module.rs ▲削除
```

- 行を選ぶと右上プレビューでそのファイルの中身を確認できます。
- `gt init-hooks` を設定しておくと、Claude Code のツール名（`Edit` / `Write` など）付きで記録されます。
  未設定でもファイル監視ベースで記録されます（ワークベンチ表示中の変更が対象）。
- **セッション開始の区切り線** — `gt init-hooks` 済みなら、Claude Code を起動する
  （SessionStart）たびに区切り線が入ります。「今回のセッションで何が変わったか」は
  区切り線より上を見るだけで分かります。
- **要確認ラベル（▲）** — 人間が目を通すべき変更をルールで自動抽出します。LLM は使いません。
  - `▲削除` … ファイルの削除すべて
  - `▲秘密` … `.env` 系（鍵・接続情報が入りがち）
  - `▲依存` … `Cargo.toml` `package.json` などの依存関係マニフェスト・ロックファイル
  - `▲CI` … `.github/workflows/` `Dockerfile` など
  - `▲設定` … `.toml` `.yml` などの設定ファイル
- 別のフォルダに `cd` するとログは仕切り直しになります（パスはプロジェクト相対のため）。

### プレビュー（右上）

- 画像ファイル（`.png` `.jpg` `.jpeg` `.gif` `.webp` `.bmp`）はプレビュー領域に収まるよう縮小して表示します。
- Markdown（`.md`）は見出し・箇条書き・コードブロックを**整形表示**します。それ以外はテキスト表示。
- 長い行は折り返し、`PageUp/Down` やホイールでスクロールできます。
- **追従モード**（既定）では、AI やコマンドが最後に書き込んだファイルへ自動で切り替わり、追記が末尾に流れます。
- ファイルを自分で選ぶと**ピン留め**（📌）され、勝手に切り替わらなくなります。
  そのファイル自身への追記は反映され続けます。ヘッダの 📌 をクリックすると追従に戻ります。
- **ターミナルに表示されたファイルパスはクリックできます**。
  Claude Code の「`src/main.rs` を編集しました」のようなパスにマウスを載せると
  手のカーソルに変わり、クリックでプレビューが開きます（URL は従来どおりブラウザ）。

### プレビューから編集する

`e`（または `[編集: nvim]` をクリック）で、**プレビュー枠がそのままエディタに変わります**。
`:wq` で閉じると閲覧に戻り、編集後の内容が反映されています。

- 使うエディタは `config.toml` の `editor` → 環境変数 `$EDITOR` → `nvim`（Windows は `notepad`）の順で決まります。
- VSCode などの GUI エディタ派は `o`（OS の既定アプリで開く）が便利です。

---

## Claude Code と連携する

ワークベンチは何もしなくてもファイルの変化を監視して changes に流しますが、
`gt` コマンドを導入すると **Claude Code 自身から「どのツールで・どのファイルを触ったか」の正確な通知**を受け取れます。

> `gt` は gototerm に同梱の小さなシェルスクリプトです（`assets/bin/gt`）。パッケージマネージャからは
> 入りません。リポジトリが手元に無い環境（Windows の exe だけ使っている場合など）では、GitHub から直接取得できます:
>
> ```sh
> mkdir -p ~/.local/bin
> curl -fsSL https://raw.githubusercontent.com/mamagotolab/gototerm/main/assets/bin/gt -o ~/.local/bin/gt
> chmod +x ~/.local/bin/gt
> ```

### 1. gt を置く（1回だけ）

`gt` は **Claude Code が動いているマシン**に置きます（シェルスクリプトなので Linux / WSL / SSH 先用です）。

| Claude Code をどこで動かしているか | gt を置く場所 |
|---|---|
| Linux ローカル | `install -m 755 assets/bin/gt ~/.local/bin/gt` |
| Windows の **WSL 内** | WSL の中で同上 |
| Windows ネイティブ（Claude Code / Codex） | 状態通知は `gototerm-hook.exe` で対応。`gt` のファイル変更フックはPOSIX用。ローカルのファイル監視はhelperなしでも動きます |

（`~/.local/bin` が PATH に入っていることを確認してください）

### 2. プロジェクトで hooks を設定する（プロジェクトごとに1回）

Claude Code を使うプロジェクトのルートで:

```sh
gt init-hooks
```

`.claude/settings.local.json` にフック設定が書き込まれます。
既にこのファイルがある場合は上書きせず、手動マージ用のスニペットを表示します。

**ランチャーからも設定できます。** `claude` コマンドが入っている環境で、
`.claude/settings.local.json` が無いフォルダをランチャーで開こうとすると、
「Claude Code 連携を設定しますか？」と一度だけ聞かれます。`gt` が未導入でも
（`gt init-hooks` を打てなくても）ここから設定できます。既に設定済みのフォルダや
`claude` が無い環境では出ません。

### 3. 使う

そのプロジェクトで Claude Code が動くと:

- サイドバーのヘッダに作業中インジケータが出ます。ファイルを触っている間は
  **`● claude MOD src/main.rs (Edit)`**、許可待ちなら **`⏸ claude 許可待ち …`**、
  応答が終わると **`✓ claude 完了`** に変わります（画面の見た目を判定するのではなく、
  Claude Code 自身の hooks から届く合図をそのまま表示しています）。
- changes 一覧に正確なイベントが流れます
- **log モード（AI 作業タイムライン）にツール名・セッション区切り付きで記録されます**
- プレビュー（追従モード）に、書き込み中のファイルの中身が流れます
- **AI の応答が終わったとき（Stop）、ウィンドウが非フォーカスなら OS 通知**が出ます
  （画面を見ているときは通知しません）。Linux は `notify-send`（無ければ通知は出ません）。
  Windows は PowerShell 経由のトースト通知（実機未検証）。
- **セッションレビューのポップアップ**が出ます。変更ファイル数・追加/削除行数
  （`git diff HEAD` ベース・git リポジトリでなければ省略）・要確認ラベル一覧を
  一目で確認できます。`Enter` / `Esc` で閉じてターミナルへ戻ります。
  ```
  ╭─────────────────────────────────╮
  │ セッションレビュー                │
  │                                   │
  │ 変更ファイル  3件                 │
  │ +42 / -8 行                       │
  │                                   │
  │ 要確認                            │
  │  ▲依存  Cargo.toml                │
  │  ▲削除  old_api.rs                │
  │                                   │
  │ Enter / Esc: ターミナルへ戻る      │
  ╰─────────────────────────────────╯
  ```
  テスト結果は表示しません（ターミナル出力の読み取りになり、上記の
  screen-scraping 禁止の原則に反するため）。

> 仕組み: フックはファイル内容をエスケープシーケンスに包んで端末に送ります（`docs/gt-protocol.md`）。
> 受信内容は**表示専用**です。gototerm がディスクに書いたりコマンドを実行したりすることはありません。
> 許可待ち・完了・セッション区切りも同じ仕組み（Notification/Stop/SessionStart/SessionEnd フック）で、
> 画面のテキストを読み取る方式（screen scraping）は使いません——UI の文言が変わると壊れるため。

---

## cwd 追従（OSC 7）

シェルに現在地（cwd）を通知させると、`cd` に合わせてサイドバーのファイル一覧が付いてきます。
`assets/shell-integration/` のスニペットを、お使いのシェルの起動ファイルから読み込むだけです。

bash（`~/.bashrc` に追記）:

```sh
__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"
}
PROMPT_COMMAND="__gototerm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```

zsh は `precmd`、fish は多くの環境で標準発行されます（`assets/shell-integration/osc7.zsh` / `osc7.fish` 参照）。
Windows ローカルの PowerShell は `osc7.ps1` を `$PROFILE` から読み込みます（Windows は `/proc` が無いため、cwd 追従にはこの設定が必要です）。

> エスケープシーケンスは SSH を素通りするので、上のスニペットや `gt` を接続先（Linux サーバ側）に置けば、
> リモートの作業も手元のワークベンチに流せます（ポート転送などの追加設定は不要）。

---

## カスタマイズ

フォント・色・ワークベンチの比率などは設定ファイルで変更できます
（配色は [色の設定](#色の設定)、キー操作は[キー操作](#キー操作)を参照）。

設定ファイルは **`~/.config/gototerm/config.toml`**（Windows は `%APPDATA%\gototerm\config.toml`）です。
自動生成されないので、同梱の [`config.example.toml`](./config.example.toml) をコピーして作ります。

```sh
mkdir -p ~/.config/gototerm
cp config.example.toml ~/.config/gototerm/config.toml
```

書いた項目だけがデフォルト値を上書きします。

### フォント

#### とりあえず何を入れればいいか（迷ったらこれ）

**設定しなくても動きます。** 内蔵フォントと、OS に入っている日本語フォント（Windows なら遊ゴシック、
Linux なら Noto Sans CJK）を自動で使うので、日本語も記号もそのまま表示されます。

こだわるなら、**日本語と Nerd Font 記号が 1 本に入ったフォント**を選ぶのが一番楽です。
1 本で済むので、後述の「主フォントの落とし穴」を踏みません。

| フォント | 特徴 |
|---|---|
| [UDEV Gothic NF](https://github.com/yuru7/udev-gothic) | JetBrains Mono + BIZ UDゴシック。半角:全角が 1:2 |
| [HackGen Console NF](https://github.com/yuru7/HackGen) | Hack + 源柔ゴシック。全角スペースが見える |

どちらも **`NF` が付いた版**を選びます（NF なしには Nerd Font の記号が入っていません）。
zip を展開して ttf を右クリック →「インストール」。あとは下記のとおりパスを書くだけです。

#### 書き方

`fonts_*` には**フォントファイルの絶対パス**を配列で指定します（フォント名ではありません）。
先頭が主フォント、2 番目以降がフォールバック。これらに無いグリフは内蔵フォント（M PLUS 1 Code）→
OS の日本語フォント の順でフォールバックします。

> **主フォントの落とし穴**
> **先頭には必ず Nerd Font（または上記の NF 版）を置いてください。**
> gototerm はセル幅を主フォントの半角幅から決めます。ここを和文フォントにすると幅の基準がズレて、
> 罫線・PowerLine・アイコンの継ぎ目が崩れます。日本語は 2 番目以降で補えば十分です。

```toml
fonts_regular = [
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",  # 英数字・アイコン
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",       # 日本語
]
fonts_bold  = [ "...Bold.ttf",   "...Bold.ttc" ]
fonts_faint = [ "...Regular.ttf", "...Regular.ttc" ]
```

パスは環境で異なります。実体は次で確認できます:

```sh
fc-match -f '%{file}\n' 'JetBrainsMono Nerd Font'
fc-match -f '%{file}\n' 'Noto Sans CJK JP'
```

### 基本設定

```toml
font_size = 17                     # ピクセル。Ctrl+= / Ctrl+- でライブ変更も可
shell = ["/usr/bin/fish"]          # 起動するシェル（省略時は $SHELL）
east_asian_width_ambiguous = 1     # 曖昧幅文字を全角(2マス)扱いなら 1、半角扱いなら 0
scroll_bar_width = 5               # スクロールバーの幅(px)。0 で非表示
status_bar_font_size = 16          # タブバー（複数タブのときだけ表示）の文字サイズ
cursor_blink = true                # カーソルを点滅させるか
cursor_thickness = 8               # バー/下線カーソルの太さ(px)。ブロックカーソルには影響しない
```

### ワークベンチの設定

```toml
# [編集] で使うエディタ。空なら $EDITOR → nvim（Windows は notepad）の順で決まる。
# 例: editor = ["nvim"] / ["vim"] / ["micro"] / ["helix"]
# VSCode 等の GUI エディタ派は editor を設定せず [OSの既定アプリで開く] が簡単。
editor = []

sidebar_ratio = 0.25    # 左サイドバーの幅比率
preview_ratio = 0.5     # 右側の上下分割比（上=プレビュー）

# スクロールバックの保持行数（ペイン毎）。大きいほどメモリを使う。
# 既定 3000 行（参考: alacritty の既定は 10000 行）。
scrollback_lines = 3000

# ファイル監視で無視するフォルダ名
watch_ignore = [".git", "node_modules", "target", "dist", "__pycache__"]
```

---

### タスクファイル連携（任意）

Markdown のチェックボックス（`- [ ] タスク`）で書いたタスクファイルを指定すると、
タブバーの右端に未完了の件数「残8 優先2」を出します（先頭が `! ` のタスクを優先として数えます）。
`Ctrl+Shift+G` で、今いるフォルダのタスクを `task_url` のページで開きます。

```toml
task_file = "~/notes/tasks.md"           # 空（既定）なら無効
task_url = "http://localhost:8765/"      # ブラウザで開く先。?v=dir:<フォルダ> を付けて開く
open_socket = true                       # Linux のみ。外部から「このファイルをエディタで開く」を受け付ける
```

`open_socket = true` にすると `$XDG_RUNTIME_DIR/gototerm.sock` を待ち受け、
`open<TAB>/絶対パス<TAB>行番号` の1行を受け取ると、そのファイルを新しいタブのエディタで開きます。
受け付けるのは既存ファイルの絶対パスだけで、コマンドは実行しません。ソケットは本人のみ読み書きできます（0600）。

## 色の設定

色はすべて **`0xRRGGBBAA`**（赤・緑・青・**アルファ**）の 32bit 整数で指定します。
末尾 2 桁の **アルファ**が不透明度で、`FF` = 不透明、`00` = 完全透明です。

### 背景の半透明（不透明度）

`color_background` の末尾 2 桁で透け具合を決めます。Wayland コンポジタ側のブラーと併用すると綺麗です。

```toml
color_background = 0x1A1B26B0   # Tokyo Night 背景＋ B0 = 176/255 ≈ 0.69（約 30% 透過）
# 目安: FF=不透明 / CC≈0.80 / B0≈0.69 / A0≈0.63 / 80=半分
```

### 配色（前景・選択・16 色）

既定の配色は **[Tokyo Night](https://github.com/folke/tokyonight.nvim)（Night バリアント）** です。

| 設定キー | 役割 | 既定値（Tokyo Night） |
|---|---|---|
| `color_foreground` | 文字色 | `0xC0CAF5FF` |
| `color_background` | 背景色（＋透過） | `0x1A1B26B0`（既定で約 30% 透過） |
| `color_selection` | 選択範囲の背景 | `0x283457FF` |
| `color_black` 〜 `color_white` | 通常の 8 色 | Tokyo Night |
| `color_bright_black` 〜 `color_bright_white` | 明るい 8 色 | Tokyo Night |
| `scroll_bar_fg_color` / `scroll_bar_bg_color` | スクロールバー | Tokyo Night |

---

## キー操作

> 下表のアプリ側キーバインドは `config.toml` の `[keybindings]` で個別に変更できます。
> 未指定の項目は既定値のまま動作します。

| キー | 動作 |
|---|---|
| `Ctrl + Shift + A` | 作業状態の一覧を開く／閉じる |
| `Ctrl + Shift + F` | ワークベンチ（開いて一覧へ → 一覧へフォーカス → 閉じる、の巡回） |
| `Ctrl + =` / `Ctrl + -` | フォント拡大 / 縮小（分割・ワークベンチ・ランチャーの全パネルに一括で効きます） |
| `Ctrl + Shift + C` / `Ctrl + Shift + V` | コピー / ペースト |
| `Ctrl + Shift + Delete` | スクロールバックの履歴を消去 |
| `Shift + マウスホイール` | 履歴スクロール |

ワークベンチ内の操作は[上の表](#ファイル一覧の操作)を参照してください。

### タブ

| キー | 動作 |
|---|---|
| `Ctrl + Shift + T` | 新しいタブ |
| `Ctrl + Tab` / `Ctrl + Shift + Tab` | 次 / 前のタブ |
| `Ctrl + 1` 〜 `Ctrl + 9` / `Ctrl + 0` | 左から1〜9番目 / 10番目のタブ（存在しない番号は何もしない） |
| `Ctrl + Shift + W` | 現在のペイン（最後の1つならタブ）を閉じる |

> タブが2枚以上のときだけ、番号とフォーカス中ペインの作業フォルダ名を表示します。
> 選択中のタブはアクセントと背景色で示し、狭い画面では名前を省略します（1枚なら全面が端末）。

### 画面分割

| キー | 動作 |
|---|---|
| `Ctrl + Shift + E` | 縦の仕切りで左右に分割 |
| `Ctrl + Shift + O` | 横の仕切りで上下に分割 |
| `Ctrl + Shift + H / J / K / L` | 隣のペインへフォーカス移動（左 / 下 / 上 / 右）。ワークベンチ表示中は端から一覧（左）・プレビュー（上）へも移れます |
| `Ctrl + Shift + ↑ / ↓ / ← / →` | ペインの境界を矢印方向へ動かす（リサイズ） |
| クリック | クリックしたペインにフォーカス |

> ワークベンチ表示中の `Ctrl + Shift + 矢印` は、左右でサイドバー幅・上下でプレビュー高さを調整します。
> ファイルパスは **`Ctrl + クリック`** でプレビューします。
> HTTP/HTTPS のURLも **`Ctrl + クリック`** で既定のブラウザに開けます。
> muttやClaude Codeなど、マウス入力を使うアプリ内でもこの操作が使えます。
> URL上でCtrlを押すと、マウスを動かさなくても手のカーソルに変わります。
> マウス入力を使わない通常のシェル出力では、URLは通常クリックでも開けます。
> 表示名にリンク先が埋め込まれたリンク（OSC 8）と、端末の画面幅による自動折り返しにも対応します。
> メール本文やアプリ自身が改行・省略したURLは、自動では復元しません。

> 新しいペインは、元のペインのシェルが居た場所（cwd）で開きます。

### 長文のコピー（Vim風の操作）

`Ctrl+Shift+Space`でコピー専用モードに入ります。マウス入力を使うTUIでも、
履歴を移動して選択できます。選択の始点はスクロールバック内に固定されます。

| 操作 | キー |
|---|---|
| カーソル移動 | `h/j/k/l`、矢印 |
| 半画面移動 | `Ctrl+U` / `Ctrl+D` |
| 履歴の先頭 / 最下部 | `g` / `G` |
| 文字 / 行 / 矩形を選択 | `v` / `V` / `Ctrl+V`（同じキーでもう一度解除） |
| 選択をコピーして終了 | `y` |
| コピーせず終了 | `Esc` |

コピー中の移動・選択キーとIME入力は端末アプリへ送りません。終了後の通常入力で最新画面に戻ります。
マウスで選択し直した後も`Esc`で終了できます。通常入力やIMEの確定入力を始めると、
マウス選択で保持していた画面を解除し、アプリの最新画面を表示します。

全画面アプリでは、ホイール、`Ctrl+U/D`、コピーカーソルが領域の端を越えたときにアプリへスクロールを要求し、
通過した文字をコピー専用のメモリに保持します。通常のマウス選択も同じ仕組みを使います。
マウス報告中のアプリでは`Shift+ドラッグ`で端末側の選択を開始できます。コピー専用モード中は、
`Shift`なしでもマウスで選択できます。マウス選択のコピーは`Ctrl+Shift+C`、コピー専用モードは`y`です。

mutt / NeoMuttのデフォルトメール表示は下矢印で別メールへ移動するため、Enter / Backspaceの専用スクロール入力を使います。
Linuxのローカル前景プロセスと直接起動したmuttは自動で識別します。SSH / WSL越しなど識別できない場合は、
コピー専用モード中の`M`でmutt用へ切り替えてから選択・スクロールしてください。`M`でもう一度、矢印操作へ戻せます。
この操作はmuttの本文表示用です。キーを独自に変更している場合は、その設定に合わせる必要があります。

本文の照合には選択とは別に前後の文字行を使い、本文領域を固定して選択の始点を保持します。
nvimなどが端末へ送るスクロール領域・行数も使うため、1行だけ選択してからの複数行スクロールや、
空行・同じ記号が続くコードの上下スクロールにも対応します。露出した行がまだ描画されていない間は最大3秒待ちます。
移動量を確認できない同一行の反復、検索ジャンプ、途中の更新、画面・サイズ・フォーカスの切り替えで連続性が判定できない場合は、
保持した選択を残してコピー継続を止め、画面下部に理由を折り返して表示します。保持済みの範囲はコピーできます。
アプリの描画完了を保証する仕組みではありません。SSHなどで行の途中の描画が120ms以上途切れる場合は、
途中までの表示を保持する可能性があるため、ファイル全文の取得にはアプリ側のコピー・保存を使ってください。
一時履歴は設定した履歴行数が上限で、ディスクへ保存しません。画面に表示されていないファイル・メール・会話の全文は取得しません。
履歴の保持行数を超えて消えた内容や、TUI自身が画面を描き直して消した内容はコピーできません。

### リンクヒント

`Ctrl+Shift+R`で表示中のHTTP/HTTPSリンクと既存のローカルファイルに、英字のラベルを表示します。
ラベルの文字を入力するとブラウザまたはファイルプレビューで開き、`Esc`でキャンセルできます。
候補が多いときは2文字です。画面更新・スクロール・フォーカス変更で候補は取り消されます。
作業フォルダを取得できない場合、ファイルの候補は出しません。Windowsでは[OSC 7](#cwd-追従osc-7)を設定してください。

### 作業セット

`Ctrl+Shift+N`でランチャーを開いて`w`を押すと、作業セット一覧へ移ります。
`s`で現在の配置に名前を付けて保存し、一覧の`Enter`で新しいタブとして復元、`d`で削除します。
既存のタブは保持します。同名の保存は拒否するので、別名にするか既存セットを削除してください。

保存するのはローカルの作業フォルダ・タブ・分割方向・分割比率だけです。
実行中のプロセス、端末出力、シェル履歴、メールやAI会話は保存せず、復元時には新しいシェルを起動します。
リモート接続を含む配置や、作業フォルダを取得できないペインは保存できません。
Windowsでは[OSC 7](#cwd-追従osc-7)の設定が必要です。復元先のフォルダが無い場合は表示し、残りの配置を開きます。
保存先はWindowsが`%APPDATA%\gototerm\workspace-sets.json`、Linuxが
`$XDG_CONFIG_HOME/gototerm/workspace-sets.json`（未設定なら`~/.config/gototerm/`）です。

### WindowsのClaude Code / Codex状態通知

`gototerm-hook.exe`をgototermの実行ファイルと同じフォルダに置きます。
連携したいプロジェクトのPowerShellで、明示的に次を実行します（パスは配置先に合わせてください）。

```powershell
& 'C:\Tools\gototerm\gototerm-hook.exe' init-hooks . all
```

`all`の代わりに`claude`または`codex`も指定できます。既存のJSON設定とユーザーフックを保持して追加し、
初回の変更前には`.json.gototerm-backup`を残します。Codexは`/hooks`で追加したフックを確認・信頼してください。
再起動したClaude Code/Codexをgototerm内で使うと、開始・承認待ち・応答完了・終了がペイン状態一覧に反映されます。
会話やコマンド本文は通知・保存せず、承認待ちでも許可や拒否を代行しません。

解除は同じ配置のhelperで`remove-hooks . all`を実行します。他のユーザーフックは残ります。
Windowsネイティブ用の操作です。WSL/Linuxでは従来の`gt hook`を使います。

### キーバインド設定

`config.toml` に書いた項目だけが上書きされます。キー文字列は `Ctrl+Shift+T` のように
`+` 区切りで、修飾キー `Ctrl` / `Shift` / `Alt` / `Super` のいずれかが必須です。

```toml
[keybindings]
focus_left = "Ctrl+Alt+H"
toggle_sidebar = "Ctrl+Alt+Space"
new_tab = "Ctrl+Shift+N"
```

| action名 | 既定値 |
|---|---|
| `new_tab` | `Ctrl+Shift+T` |
| `open_task_overview` | `Ctrl+Shift+A` |
| `open_tasks` | `Ctrl+Shift+G`（`task_file` 設定時のみ） |
| `close_pane` | `Ctrl+Shift+W` |
| `next_tab` | `Ctrl+Tab` |
| `prev_tab` | `Ctrl+Shift+Tab` |
| `select_tab_1` 〜 `select_tab_9` / `select_tab_10` | `Ctrl+1` 〜 `Ctrl+9` / `Ctrl+0` |
| `split_vertical` | `Ctrl+Shift+E` |
| `split_horizontal` | `Ctrl+Shift+O` |
| `toggle_sidebar` | `Ctrl+Shift+F` |
| `focus_left` / `focus_down` / `focus_up` / `focus_right` | `Ctrl+Shift+H/J/K/L` |
| `resize_up` / `resize_down` / `resize_left` / `resize_right` | `Ctrl+Shift+↑/↓/←/→` |
| `increase_font` / `decrease_font` | `Ctrl+=` / `Ctrl+-` |
| `copy` / `paste` | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| `clear_history` | `Ctrl+Shift+Delete` |
| `copy_mode` | `Ctrl+Shift+Space` |
| `link_hints` | `Ctrl+Shift+R` |

## Windows 実機での確認手順

以下は開発版をWindows実機へ引き継ぐためのチェックリストです。この文書の時点では、
Windows固有の結果は未確認です。確認した実測値と結果だけを記録してください。

### 表示・タブ・選択

1. Windowsの表示倍率を、接続している各モニターで100％にします。
2. `%APPDATA%\gototerm\config.toml` がある場合は削除せず一時的に退避し、
   設定なしの内蔵フォントでgototermを起動します。
3. 各モニターへウィンドウを順に移動し、元のモニターにも戻します。移動のたびに
   文字サイズ、輪郭の鮮明さ、罫線などのセル位置、IME候補ウィンドウの位置を確認します。
4. タブAで `Ctrl+Shift+F` を押してワークベンチを開き、タブBは通常表示のままにします。
   A→B→Aと切り替え、開閉状態がそれぞれのタブに保たれることを確認します。
   一覧の内容、分割比率、フォント倍率はウィンドウ共通の仕様です。
5. スクロールバック内の複数行を選択し、選択を解除せず `Shift+マウスホイール` で上下に
   スクロールします。ハイライトが選択した文字に残り、画面外の部分が上下端へ貼り付かず、
   画面内との交差部分だけ表示されることを確認します。スクロール後のコピー結果も確認します。

結果は「確認済み／再現あり／未確認」のいずれかと、モニターごとの表示倍率を記録します。
スクリーンショットだけでなく、IME候補位置とコピー結果も文字で残してください。

### SSH先のmutt

1. 通常どおり既存ホストへSSH接続し、muttを起動します。
2. メール一覧を上下左右の矢印キーで移動し、`j` / `k` 以外でも選択が動くか記録します。
3. 過去の送信メールを再利用し、日本語の件名と本文が記号へ変わらないか確認します。
4. 文字化けが残る場合だけ、次節のUTF-8診断を有効にして同じ操作を1回再現します。

診断で異常が出ない場合も、gototerm側でISO-2022-JP、Shift-JIS、EUC-JPなどへの
推測変換は行いません。SSH先のlocaleとmuttの文字コード設定を別途確認します。

### メモリの測定

gototermとWezTermは、同じモニター、ウィンドウ寸法、シェル、作業ディレクトリ、
タブ数で個別に起動します。他の端末を閉じ、タスクマネージャーの同じ「メモリ」列を使い、
次の値をそのまま記録します。単発値だけで優劣や削減量を断定しません。

| 条件 | gototerm | WezTerm | 備考 |
|---|---:|---:|---|
| 起動直後・通常表示・1タブ | 未測定 | 未測定 | 同じシェルのプロンプト表示後 |
| ワークベンチ初回表示後・1タブ | 未測定 | 対象外 | gototermの遅延生成確認用 |
| 通常表示・2タブ | 未測定 | 未測定 | 両方とも同じタブ数 |

---

## トラブルシューティング

### SSH先で日本語表示が崩れる場合

PTY出力のUTF-8診断は、PowerShellで次のように明示的に有効化できます。

```powershell
$env:GOTOTERM_UTF8_DIAGNOSTICS = "1"
$env:RUST_LOG = "gototerm::vt=warn"
Start-Process -FilePath ".\gototerm-windows-x64.exe" -Wait `
  -RedirectStandardError ".\gototerm-utf8-diagnostic.log"
```

SSH先のmuttで過去送信メールの再利用を1回行い、gototermを終了してから
`gototerm-utf8-diagnostic.log` を確認します。UTF-8診断の各行に記録するのは、
不正または未完シーケンスの累計件数、PTY出力内の相対オフセット、
その時点の端末モードだけです。メール本文や復元可能な生バイト列は記録しません。
読み取り単位をまたいで正常に完成した文字は未完として数えず、PTY終了時や
OSC・Sixelとの境界に残った接頭辞だけを未完として記録します。
共有する前に、診断以外の実行時警告が混ざっていないか確認してください。
診断後は次のように環境変数を解除します。

```powershell
Remove-Item Env:GOTOTERM_UTF8_DIAGNOSTICS
Remove-Item Env:RUST_LOG
```

gototermはメール本文の文字コードを判定・変換しません。診断でUTF-8の異常が
見つからない場合は、SSH先のlocaleやmutt側のメール文字コード設定も確認してください。

---

## 対応・非対応

- ✅ 日本語入力（IME・インライン変換）、UTF-8、マウスレポート、ハードウェア描画
- ✅ **完全な VT 互換**（alacritty_terminal エンジン採用）。nvim・Claude Code 等の
  高機能 TUI も正しく描画できる。SGR（RGB / 256 色）・Alternate Screen・
  Bracketed Paste・スクロールバック対応
- ✅ タブ・画面分割・**3分割ワークベンチ**（ファイル一覧・プレビュー・AI作業の見える化）
- ✅ **Sixel 画像表示**（yazi のプレビュー・アルバムアートなど。`?62;4c` で対応申告）
- ✅ Linux（Wayland）/ Windows で動作
- ⚠️ kitty graphics protocol は未対応（画像は Sixel のみ）
- ⚠️ 画像はスクロールに追従しない（全画面 TUI での絶対配置は問題なし）
- ⚠️ Windows は背景の透過に未対応
- ⚠️ Windows ローカルの cwd 追従はシェル統合（OSC 7）の導入が必要（`/proc` が無いため）

---

## ライセンス・謝辞

MIT License。本ソフトウェアは [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)
（Copyright 2022 algon-320, MIT License）をベースにしています。元の著作権表示は `LICENSE` に保持しています。

内蔵フォント（M PLUS 1 Code）は Open Font License (OFL) で再配布しています。
詳細は `src/font/OFL.txt` を参照してください。
