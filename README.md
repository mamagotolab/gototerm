# gototerm

**日本語入力のストレスが少ない、軽量ターミナルエミュレータ。**

gototerm（ゴートターム）は、日本語を打つ人のために作られた Linux 向けターミナルです。
変換中の文字がカーソル位置にそのまま表示され、変換候補も入力位置に出る ——
「いつもの端末は日本語入力がもたつく・ズレる」という小さなストレスを減らすことを第一に設計しています。

> [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)（MIT License）をベースに、
> モダンな Wayland 環境への対応と日本語入力まわりを大きく作り直したフォークです。

---

## なぜ gototerm か

- **日本語入力が素直** — 変換中の文字（preedit）を端末内のカーソル位置にインライン表示。変換候補もカーソルに追従（fcitx5 等の Wayland text-input-v3 に対応）。
- **軽い** — GPU 必須の重量級端末に比べてメモリが小さい（実測でメモリ約 1/3、バイナリも約半分）。
- **半透明＋ぼかし対応** — 背景の不透明度を細かく指定でき、Wayland コンポジタのブラーと相性良し。
- **全角幅に配慮** — East Asian Width（曖昧幅）を設定で切り替え可能。日本語の表組みが崩れにくい。

---

## インストール

### 必要なもの

- Rust ツールチェイン（`cargo`）
- FreeType / fontconfig（フォント描画）
- Wayland 環境（X11 でも XWayland 経由で動作）

### ビルドと導入

```sh
git clone <このリポジトリ>
cd gototerm

# ① terminfo を必ず入れる（重要）
tic -x toyterm.info

# ② ビルド＆インストール
cargo install --path .
```

> ⚠️ **terminfo（`tic -x toyterm.info`）は必須です。**
> これを入れないと nvim 等のアプリが端末の能力を認識できず、
> 画面に `B` のような文字が大量に漏れて崩れます。最初に必ず実行してください。
> （`TERM=toyterm-256color` として動作します。グローバル導入は `sudo tic -x toyterm.info`）

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
color_background = 0x000000B0   # B0 = 176/255 ≈ 0.69（約 30% 透過）
# 目安: FF=不透明 / CC≈0.80 / B0≈0.69 / A0≈0.63 / 80=半分
```

### 配色（前景・選択・16 色）

| 設定キー | 役割 | 既定値 |
|---|---|---|
| `color_foreground` | 文字色 | `0xFFFFFFFF` |
| `color_background` | 背景色（＋透過） | `0x000000FF` |
| `color_selection` | 選択範囲の背景 | `0x505050FF` |
| `color_black` 〜 `color_white` | 通常の 8 色 | ANSI 標準 |
| `color_bright_black` 〜 `color_bright_white` | 明るい 8 色 | ANSI 標準 |
| `scroll_bar_fg_color` / `scroll_bar_bg_color` | スクロールバー | 灰系 |

一部だけ上書きする例（Tokyo Night 風）:

```toml
color_foreground = 0xC0CAF5FF
color_green      = 0x9ECE6AFF
color_blue       = 0x7AA2F7FF
color_magenta    = 0xBB9AF7FF
```

---

## キー操作

| キー | 動作 |
|---|---|
| `Ctrl + =` / `Ctrl + -` | フォント拡大 / 縮小 |
| `Ctrl + Shift + C` / `Ctrl + Shift + V` | コピー / ペースト |
| `Ctrl + Shift + L` | スクロールバックの履歴を消去 |
| `Shift + マウスホイール` | 履歴スクロール |

---

## 対応・非対応

- ✅ 日本語入力（IME・インライン変換）、UTF-8、SIXEL 画像、マウスレポート、ハードウェア描画
- ✅ ECMA-48 準拠の主要な制御機能、SGR（RGB / 256 色）、Alternate Screen、Bracketed Paste 等
- ⚠️ VT 対応は実用十分だが完全ではない。重い装飾系 TUI プラグインでは表示が崩れることがある
- ⚠️ 現状 Linux 専用（Windows 対応は作業中）

---

## ライセンス・謝辞

MIT License。本ソフトウェアは [algon-320 氏の toyterm](https://github.com/algon-320/toyterm)
（Copyright 2022 algon-320, MIT License）をベースにしています。元の著作権表示は `LICENSE` に保持しています。

内蔵フォント（M PLUS 1 Code）は Open Font License (OFL) で再配布しています。
詳細は `src/font/OFL.txt` を参照してください。
