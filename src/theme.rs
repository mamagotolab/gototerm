//! 他の端末の配色テーマを読み込む（`theme = "名前"` / パス）。
//!
//! 読める形式は2つ：
//! - Ghostty のテーマ（`palette = 0=#15161e` / `background = #1a1b26` …）。
//!   iTerm2-Color-Schemes の `ghostty/` にある数百種類もこの形式。
//! - Alacritty の TOML（`[colors.primary]` `[colors.normal]` `[colors.bright]` `[colors.selection]`）。
//!   `alacritty.toml` をそのまま指定してもよい（色以外は無視する）。
//!
//! 結果は `color_*` の設定キーと値（0xRRGGBBAA）の組。背景のアルファは呼び出し側が決める。

use std::path::{Path, PathBuf};

const NORMAL: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// テーマ名かパスを実際のファイルに解決する。見つからなければ None。
pub fn resolve(name: &str, config_dir: Option<&Path>) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let expanded = expand_home(name);
    if expanded.is_file() {
        return Some(expanded);
    }
    // 名前だけなら themes フォルダを探す（区切り文字を含むものはパス扱いで、上で無ければ無し）
    if name.contains(['/', '\\']) {
        return None;
    }
    search_dirs(config_dir)
        .into_iter()
        .flat_map(|dir| ["", ".toml", ".conf"].map(|ext| dir.join(format!("{name}{ext}"))))
        .find(|p| p.is_file())
}

/// テーマを探すフォルダ（先にあるほど優先）。
pub fn search_dirs(config_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = config_dir {
        dirs.push(dir.join("themes"));
    }
    #[cfg(not(windows))]
    {
        // Ghostty を入れていれば、そのテーマ集もそのまま使える。
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        if let Some(xdg) = xdg {
            dirs.push(xdg.join("ghostty").join("themes"));
        }
        dirs.push(PathBuf::from("/usr/share/ghostty/themes"));
        dirs.push(PathBuf::from("/usr/local/share/ghostty/themes"));
    }
    dirs
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// テーマの中身から色を取り出す。形式は中身で判定する（拡張子は見ない）。
pub fn parse(text: &str) -> Vec<(String, u32)> {
    let alacritty = parse_alacritty(text);
    if !alacritty.is_empty() {
        return alacritty;
    }
    parse_ghostty(text)
}

fn parse_ghostty(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "palette" => {
                let Some((index, color)) = value.split_once('=') else {
                    continue;
                };
                let (Ok(index), Some(rgb)) = (index.trim().parse::<usize>(), parse_rgb(color))
                else {
                    continue;
                };
                if let Some(name) = palette_key(index) {
                    out.push((name, rgb));
                }
            }
            "background" | "foreground" | "selection-background" => {
                if let Some(rgb) = parse_rgb(value) {
                    let name = match key {
                        "background" => "color_background",
                        "foreground" => "color_foreground",
                        _ => "color_selection",
                    };
                    out.push((name.to_owned(), rgb));
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_alacritty(text: &str) -> Vec<(String, u32)> {
    let Ok(doc) = ::config::Config::builder()
        .add_source(::config::File::from_str(text, ::config::FileFormat::Toml))
        .build()
    else {
        return Vec::new();
    };
    let get = |path: &str| doc.get_string(path).ok().and_then(|v| parse_rgb(&v));
    let mut out = Vec::new();
    for (path, key) in [
        ("colors.primary.background", "color_background"),
        ("colors.primary.foreground", "color_foreground"),
        ("colors.selection.background", "color_selection"),
    ] {
        if let Some(rgb) = get(path) {
            out.push((key.to_owned(), rgb));
        }
    }
    for (group, offset) in [("normal", 0), ("bright", 8)] {
        for (i, color) in NORMAL.iter().enumerate() {
            if let Some(rgb) = get(&format!("colors.{group}.{color}")) {
                out.push((palette_key(offset + i).unwrap(), rgb));
            }
        }
    }
    out
}

/// 0〜15 の色番号を設定キーにする（0=color_black … 8=color_bright_black …）。
fn palette_key(index: usize) -> Option<String> {
    let name = NORMAL.get(index % 8)?;
    match index {
        0..=7 => Some(format!("color_{name}")),
        8..=15 => Some(format!("color_bright_{name}")),
        _ => None,
    }
}

/// `#rrggbb` / `rrggbb` / `0xrrggbb` / `#rgb` を 0xRRGGBBFF にする。
fn parse_rgb(value: &str) -> Option<u32> {
    let v = value.trim().trim_matches(['"', '\'']);
    let hex = v
        .strip_prefix('#')
        .or_else(|| v.strip_prefix("0x"))
        .or_else(|| v.strip_prefix("0X"))
        .unwrap_or(v);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok()?,
        3 => {
            let n = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((n >> 8) & 0xF, (n >> 4) & 0xF, n & 0xF);
            (r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11)
        }
        _ => return None,
    };
    Some(rgb << 8 | 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(colors: &[(String, u32)], key: &str) -> Option<u32> {
        colors.iter().rev().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    #[test]
    fn reads_ghostty_theme() {
        let text = "# comment\npalette = 0=#15161e\npalette = 15=#c0caf5\n\
                    background = #1a1b26\nforeground = c0caf5\nselection-background = #33467c\n\
                    cursor-color = #c0caf5\n";
        let c = parse(text);
        assert_eq!(get(&c, "color_black"), Some(0x15161EFF));
        assert_eq!(get(&c, "color_bright_white"), Some(0xC0CAF5FF));
        assert_eq!(get(&c, "color_background"), Some(0x1A1B26FF));
        assert_eq!(get(&c, "color_foreground"), Some(0xC0CAF5FF));
        assert_eq!(get(&c, "color_selection"), Some(0x33467CFF));
    }

    #[test]
    fn reads_alacritty_toml() {
        let text = "[colors.primary]\nbackground = \"#10100E\"\nforeground = '0xC6C6C4'\n\
                    [colors.normal]\nred = \"#C40233\"\n[colors.bright]\nblue = \"#07F\"\n\
                    [font]\nsize = 7.0\n";
        let c = parse(text);
        assert_eq!(get(&c, "color_background"), Some(0x10100EFF));
        assert_eq!(get(&c, "color_foreground"), Some(0xC6C6C4FF));
        assert_eq!(get(&c, "color_red"), Some(0xC40233FF));
        assert_eq!(get(&c, "color_bright_blue"), Some(0x0077FFFF));
    }

    #[test]
    fn ignores_unknown_lines_and_colors() {
        let c = parse("palette = 16=#ffffff\nbackground = red\nfont-size = 12\n");
        assert!(c.is_empty());
    }

    #[test]
    fn resolves_name_in_themes_dir() {
        let dir = std::env::temp_dir().join(format!("gototerm-theme-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(dir.join("themes").join("My Theme.toml"), "").unwrap();
        assert_eq!(
            resolve("My Theme", Some(&dir)),
            Some(dir.join("themes").join("My Theme.toml"))
        );
        assert_eq!(resolve("does-not-exist-xyz", Some(&dir)), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
