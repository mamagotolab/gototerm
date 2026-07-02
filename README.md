# gototerm

**日本語入力のストレスが少ない、AI開発ワークベンチ付き軽量ターミナル。**

gototerm（ゴトターム）は、日本語を打つ人のために作られたターミナルです。
変換中の文字がカーソル位置にそのまま表示され、変換候補も入力位置に出る ——
「いつもの端末は日本語入力がもたつく・ズレる」という小さなストレスを減らすことを第一に設計しています。

さらに `Ctrl+Shift+F` ひとつで、**ファイル一覧・プレビュー・ターミナルの3分割ワークベンチ**に切り替わります。
Claude Code などの AI コーディングツールが「いま・どのファイルを・どう変えているか」を、
隣のペインでリアルタイムに眺めながら作業できます。SSH 先の作業にも対応しています。

> 名前は、プログラミングの `goto` と、開発元 [mamagotolab](https://github.com/mamagotolab) に由来します。

> [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)（MIT License）をベースに、
> モダンな Wayland 環境への対応と日本語入力まわりを大きく作り直したフォークです。

---

## なぜ gototerm か

- **日本語入力が素直** — 変換中の文字（preedit）を端末内のカーソル位置にインライン表示。変換候補もカーソルに追従（fcitx5 等の Wayland text-input-v3 に対応）。
- **AIの作業が見える** — Claude Code がファイルを書くそばから、中身が隣のペインに流れる。ファイル名の出力はクリックで即プレビュー。
- **軽い** — GPU 必須の重量級端末に比べてメモリが小さい（実測でメモリ約 1/3、バイナリも約半分）。
- **半透明＋ぼかし対応** — 背景の不透明度を細かく指定でき、Wayland コンポジタのブラーと相性良し。
- **全角幅に配慮** — East Asian Width（曖昧幅）を設定で切り替え可能。日本語の表組みが崩れにくい。
- **完全な VT 互換** — VT エンジンに [alacritty_terminal](https://crates.io/crates/alacritty_terminal) を採用。nvim・Claude Code 等の高機能 TUI も正しく描画。
- **タブ・画面分割・Sixel 画像** — 端末内で画像表示（yazi のプレビューなど）も可能。

---

## インストール

### 🪟 Windows（ビルド不要・おすすめ）

[**Releases ページ**](https://github.com/mamagotolab/gototerm/releases/latest) から
`gototerm-windows-x64.exe` をダウンロードし、ダブルクリックで起動するだけです。

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
# 生成物: target\release\gototerm.exe
```

> もし「couldn't determine visual studio generator」で止まる場合は、
> `winget install Ninja-build.Ninja` で Ninja を入れ、`$env:CMAKE_GENERATOR = "Ninja"` も足してください。

> ℹ️ **terminfo の導入は不要です。**
> 内部の VT エンジンに [alacritty_terminal](https://crates.io/crates/alacritty_terminal)
> を採用し、`TERM=xterm-256color` として動作します。

---

## ワークベンチ（3分割モード）

### 30秒ではじめる

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
| 選択を移動 | `↑` `↓`（`PageUp/Down` `Home/End` も可） | — |
| 一覧をスクロール | `↑` `↓`（自動追従） | ホイール |
| フォルダに入る / ファイルを開く | `→` または `Enter` | クリック |
| 親フォルダへ戻る | `←` または `Backspace` | `../` をクリック |
| files ⇔ changes 切り替え | `Tab` | 切替行をクリック |
| プレビューをスクロール | `PageUp` / `PageDown` | プレビュー上でホイール |
| 開いたファイルを編集 | `e` | `[編集: nvim]` をクリック |
| OS の既定アプリで開く | `o` | `[OSの既定アプリで開く]` をクリック |
| ターミナルへ戻る | `Esc` | ターミナルをクリック |

- 一覧のフォーカス中は選択行が反転表示され、最下行に操作ヒントが出ます。
- フォーカス中に打った文字がシェルに流れることはありません。

### 2つのモード：files と changes

- **files** … いま居るフォルダのファイル一覧。フォルダを潜って探せます（シェルで `cd` すると一覧も付いてきます）。
- **changes** … 作成・変更・削除されたファイルが新しい順に流れます。`NEW`（緑）/ `MOD`（黄）/ `DEL`（赤）のバッジ付き。
  AI にコードや記事を書かせているとき、何が起きているかを一覧で把握できます。

### プレビュー（右上）

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

### 1. gt を置く（1回だけ）

```sh
install -m 755 assets/bin/gt ~/.local/bin/gt
```

（`~/.local/bin` が PATH に入っていることを確認してください）

### 2. プロジェクトで hooks を設定する（プロジェクトごとに1回）

Claude Code を使うプロジェクトのルートで:

```sh
gt init-hooks
```

`.claude/settings.local.json` にフック設定が書き込まれます。
既にこのファイルがある場合は上書きせず、手動マージ用のスニペットを表示します。

### 3. 使う

そのプロジェクトで Claude Code が動くと:

- サイドバーのヘッダに **`● claude MOD src/main.rs (Edit)`** のような作業中インジケータが出ます
- changes 一覧に正確なイベントが流れます
- プレビュー（追従モード）に、書き込み中のファイルの中身が流れます

> 仕組み: フックはファイル内容をエスケープシーケンスに包んで端末に送ります（`docs/gt-protocol.md`）。
> 受信内容は**表示専用**です。gototerm がディスクに書いたりコマンドを実行したりすることはありません。

---

## SSH 先で使う

ワークベンチのファイル監視・一覧は手元のマシンを見るため、SSH 先の作業はそのままでは見えません。
次の2つを SSH 先に置くと、**リモートの作業も手元のワークベンチに流れてきます**。
（エスケープシーケンスは SSH を素通りするので、ポート転送などの設定は不要です）

### cwd 追従（OSC 7）

SSH 先のシェルに現在地を通知させます。`assets/shell-integration/` のスニペットを読み込むだけです。

bash（`~/.bashrc` に追記）:

```sh
__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"
}
PROMPT_COMMAND="__gototerm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```

zsh は `precmd`、fish は多くの環境で標準発行されます（`assets/shell-integration/osc7.zsh` / `osc7.fish` 参照）。

これでリモート接続中はサイドバーが `host:path` 表示になり、手元のファイル一覧を誤って出さなくなります。

### ファイルの中身を送る（gt）

```sh
scp assets/bin/gt remote:~/bin/gt
ssh remote 'chmod +x ~/bin/gt'
```

SSH 先で:

```sh
gt view 記事.md      # このファイルの中身が、手元の右上プレビューに表示される
gt init-hooks       # リモートの Claude Code の作業が、手元にリアルタイムで流れる
```

---

## 設定マニュアル

設定ファイルは **`~/.config/gototerm/config.toml`**（Windows は `%APPDATA%\gototerm\config.toml`）です。
自動生成されないので、同梱の [`config.example.toml`](./config.example.toml) をコピーして作ります。

```sh
mkdir -p ~/.config/gototerm
cp config.example.toml ~/.config/gototerm/config.toml
```

書いた項目だけがデフォルト値を上書きします。

### フォント

`fonts_*` には**フォントファイルの絶対パス**を配列で指定します（フォント名ではありません）。
先頭が主フォント、2 番目以降がフォールバック。これらに無いグリフは内蔵フォント（M PLUS 1 Code）が最終フォールバックになります。

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
```

### ワークベンチの設定

```toml
# [編集] で使うエディタ。空なら $EDITOR → nvim（Windows は notepad）の順で決まる。
# 例: editor = ["nvim"] / ["vim"] / ["micro"] / ["helix"]
# VSCode 等の GUI エディタ派は editor を設定せず [OSの既定アプリで開く] が簡単。
editor = []

sidebar_ratio = 0.25    # 左サイドバーの幅比率
preview_ratio = 0.5     # 右側の上下分割比（上=プレビュー）

# ファイル監視で無視するフォルダ名
watch_ignore = [".git", "node_modules", "target", "dist", "__pycache__"]
```

---

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
| `color_background` | 背景色（＋透過） | `0x1A1B26FF` |
| `color_selection` | 選択範囲の背景 | `0x283457FF` |
| `color_black` 〜 `color_white` | 通常の 8 色 | Tokyo Night |
| `color_bright_black` 〜 `color_bright_white` | 明るい 8 色 | Tokyo Night |
| `scroll_bar_fg_color` / `scroll_bar_bg_color` | スクロールバー | Tokyo Night |

---

## キー操作

| キー | 動作 |
|---|---|
| `Ctrl + Shift + F` | ワークベンチ（開いて一覧へ → 一覧へフォーカス → 閉じる、の巡回） |
| `Ctrl + =` / `Ctrl + -` | フォント拡大 / 縮小 |
| `Ctrl + Shift + C` / `Ctrl + Shift + V` | コピー / ペースト |
| `Ctrl + Shift + L` | スクロールバックの履歴を消去 |
| `Shift + マウスホイール` | 履歴スクロール |

ワークベンチ内の操作は[上の表](#ファイル一覧の操作)を参照してください。

### タブ

| キー | 動作 |
|---|---|
| `Ctrl + Shift + T` | 新しいタブ |
| `Ctrl + Tab` / `Ctrl + Shift + Tab` | 次 / 前のタブ |
| `Ctrl + Shift + W` | 現在のペイン（最後の1つならタブ）を閉じる |

> タブが 2 枚以上のときだけ画面上部にタブバーが出ます（1 枚なら全面が端末）。

### 画面分割

| キー | 動作 |
|---|---|
| `Ctrl + Shift + E` | 縦の仕切りで左右に分割 |
| `Ctrl + Shift + O` | 横の仕切りで上下に分割 |
| `Ctrl + Shift + ↑ / ↓ / ← / →` | 隣のペインへフォーカス移動 |
| `Ctrl + Shift + Q` | 現在のペインを閉じる |
| クリック | クリックしたペインにフォーカス |

> 新しいペインは、元のペインのシェルが居た場所（cwd）で開きます。

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
