# gototerm

**日本語入力のストレスが少ない、軽量ターミナルエミュレータ。**

gototerm（ゴトターム）は、日本語を打つ人のために作られた Linux 向けターミナルです。
変換中の文字がカーソル位置にそのまま表示され、変換候補も入力位置に出る ——
「いつもの端末は日本語入力がもたつく・ズレる」という小さなストレスを減らすことを第一に設計しています。

> 名前は、プログラミングの `goto` と、開発元 [mamagotolab](https://github.com/mamagotolab) に由来します。

> [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)（MIT License）をベースに、
> モダンな Wayland 環境への対応と日本語入力まわりを大きく作り直したフォークです。

---

## なぜ gototerm か

- **日本語入力が素直** — 変換中の文字（preedit）を端末内のカーソル位置にインライン表示。変換候補もカーソルに追従（fcitx5 等の Wayland text-input-v3 に対応）。
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
> を採用し、`TERM=xterm-256color` として動作します。xterm-256color の terminfo は
> ほぼ全ての環境に標準で入っているため、`tic` での導入は要りません。

---

## 設定マニュアル

設定ファイルは **`~/.config/gototerm/config.toml`** です。自動生成されないので自分で作成します。
同梱の [`config.example.toml`](./config.example.toml) をコピーするのが簡単です。

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

### 文字サイズ・その他

```toml
font_size = 17                     # ピクセル。スケールに応じて調整。Ctrl+= / Ctrl+- でライブ変更も可
shell = ["/usr/bin/fish"]          # 起動するシェル（省略時は $SHELL）
east_asian_width_ambiguous = 1     # 曖昧幅文字を全角(2マス)扱いなら 1、半角扱いなら 0
scroll_bar_width = 5               # スクロールバーの幅(px)。0 で非表示
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

別のテーマにしたいときは、必要なキーだけ上書きします（例：背景を透過させる）:

```toml
color_background = 0x1A1B26B0   # Tokyo Night の背景＋透過
```

---

## SSH・cwd 追従（OSC 7）

gototerm は OSC 7 (`file://host/path`) を受け取ると、ペインの現在ディレクトリを追従します。
`host` がローカルホストと異なる場合はリモート接続中として扱い、サイドバーはローカルファイルの監視・一覧を止めます。

bash:

```sh
__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"
}
PROMPT_COMMAND="__gototerm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```

zsh:

```sh
__gototerm_osc7() {
    printf '\033]7;file://%s%s\033\\' "$HOST" "$PWD"
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __gototerm_osc7
```

fish は多くの環境で OSC 7 を標準発行します。発行されない場合は次を読み込んでください。

```fish
function __gototerm_osc7 --on-event fish_prompt
    printf '\033]7;file://%s%s\033\\' (hostname) "$PWD"
end
```

同じ内容のスニペットを `assets/shell-integration/osc7.bash`、
`assets/shell-integration/osc7.zsh`、`assets/shell-integration/osc7.fish` に同梱しています。

---

## キー操作

| キー | 動作 |
|---|---|
| `Ctrl + =` / `Ctrl + -` | フォント拡大 / 縮小 |
| `Ctrl + Shift + C` / `Ctrl + Shift + V` | コピー / ペースト |
| `Ctrl + Shift + F` | サイドバー表示 / 非表示 |
| `Ctrl + Shift + L` | スクロールバックの履歴を消去 |
| `Shift + マウスホイール` | 履歴スクロール |

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

---

## 対応・非対応

- ✅ 日本語入力（IME・インライン変換）、UTF-8、マウスレポート、ハードウェア描画
- ✅ **完全な VT 互換**（alacritty_terminal エンジン採用）。nvim・Claude Code 等の
  高機能 TUI も正しく描画できる。SGR（RGB / 256 色）・Alternate Screen・
  Bracketed Paste・スクロールバック対応
- ✅ タブ・画面分割（二分割を入れ子に。上のキー操作を参照）
- ✅ **Sixel 画像表示**（yazi のプレビュー・アルバムアートなど。`?62;4c` で対応申告）
- ⚠️ kitty graphics protocol は未対応（画像は Sixel のみ）
- ⚠️ 画像はスクロールに追従しない（全画面 TUI での絶対配置は問題なし）
- ⚠️ Linux で動作（Windows 対応は作業中）

---

## ライセンス・謝辞

MIT License。本ソフトウェアは [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)
（Copyright 2022 algon-320, MIT License）をベースにしています。元の著作権表示は `LICENSE` に保持しています。

内蔵フォント（M PLUS 1 Code）は Open Font License (OFL) で再配布しています。
詳細は `src/font/OFL.txt` を参照してください。
