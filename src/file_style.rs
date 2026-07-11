//! ファイル一覧の見た目（アイコン・色・選択バー）をランチャーとサイドバーで
//! 共有するためのヘルパー。両者の見た目を揃え、今後もズレないようにする。
//!
//! アイコンは Nerd Font のグリフ。config で Nerd Font を指定している前提
//! （無いフォント環境では豆腐になる）。

use crate::terminal::Color;

/// Tokyo Night のコメント色（淡い表示・ファイル既定色）。
pub const DIM: Color = Color::Rgb { rgba: 0x565F_89FF };
/// フォルダ名の色（Tokyo Night の青）。
pub const DIR_FG: Color = Color::BrightBlue;
/// 選択バーの背景（Tokyo Night の青）。
pub const SEL_BG: Color = Color::Rgb { rgba: 0x3D59_A1FF };
/// 選択バーの文字色。
pub const SEL_FG: Color = Color::BrightWhite;
/// アクセント（`..` やパンくず末尾などに使う TN シアン）。
pub const ACCENT: Color = Color::Rgb { rgba: 0x7DCF_FFFF };

/// エントリ名（と dir かどうか）から、Nerd Font アイコンと文字色を返す。
/// `..` は上矢印、フォルダは青、ファイルは拡張子ごとに色分けする。
pub fn icon_and_color(name: &str, is_dir: bool) -> (char, Color) {
    if name == ".." {
        return ('\u{f062}', ACCENT); // arrow-up
    }
    if is_dir {
        return ('\u{f07b}', DIR_FG); // folder
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => ('\u{e7a8}', Color::Rgb { rgba: 0xE0AF_68FF }), // rust
        "md" | "markdown" => ('\u{f48a}', Color::BrightCyan),
        "toml" | "yaml" | "yml" | "json" | "lock" | "ini" | "conf" => {
            ('\u{e615}', Color::Rgb { rgba: 0xE0AF_68FF })
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" => {
            ('\u{f1c5}', Color::BrightMagenta)
        }
        "sh" | "bash" | "zsh" | "fish" => ('\u{f489}', Color::Rgb { rgba: 0x9ECE_6AFF }),
        "js" | "ts" | "py" | "go" | "c" | "cpp" | "h" | "html" | "css" => {
            ('\u{f121}', Color::BrightCyan)
        }
        "txt" | "log" => ('\u{f0f6}', DIM),
        _ => ('\u{f15b}', DIM), // generic file
    }
}
