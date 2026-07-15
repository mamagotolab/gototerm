//! プレビューのシンタックスハイライト。
//!
//! 解析（どこがコメントで、どこが文字列か）は syntect に任せ、色は自前で決める。
//! テーマファイルを持ち込まず、scope セレクタ → Tokyo Night の色をこの表で対応づける
//! （ターミナル本体・サイドバーと同じ配色に揃えるため）。

use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::terminal::Color;

/// これより長いファイルはハイライトしない（1行ずつ正規表現を回すので、
/// AI が書き換えるたびに走らせると描画が引っかかる）。
const MAX_LINES: usize = 5000;

/// ハイライト済みの1行（色つきの断片の並び）。
pub type HighlightedLine = Vec<(Color, String)>;

/// 拡張子から文法を選んでハイライトする。未知の拡張子なら None。
pub fn highlight_source(text: &str, path: &Path) -> Option<Vec<HighlightedLine>> {
    let syntaxes = syntaxes();
    let name = path.file_name()?.to_str()?;
    let syntax = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| syntaxes.find_syntax_by_extension(ext))
        // 拡張子なしの Makefile・Dockerfile などはファイル名そのもので引く。
        .or_else(|| syntaxes.find_syntax_by_extension(name))?;
    highlight(syntax, text)
}

/// Markdown のコードフェンス（```rust 等）を言語名でハイライトする。
pub fn highlight_code_block(code: &str, lang: &str) -> Option<Vec<HighlightedLine>> {
    if lang.is_empty() {
        return None;
    }
    let syntax = syntaxes().find_syntax_by_token(lang)?;
    highlight(syntax, code)
}

fn highlight(syntax: &SyntaxReference, text: &str) -> Option<Vec<HighlightedLine>> {
    if text.lines().take(MAX_LINES + 1).count() > MAX_LINES {
        return None;
    }

    let syntaxes = syntaxes();
    let mut state = HighlightLines::new(syntax, theme());
    let mut out = Vec::new();
    for line in LinesWithEndings::from(text) {
        // 壊れた入力で失敗したら、その時点で諦めて素のテキストに戻す。
        let ranges = state.highlight_line(line, syntaxes).ok()?;
        out.push(
            ranges
                .into_iter()
                .map(|(style, text)| {
                    (
                        to_color(style.foreground),
                        text.trim_end_matches(['\n', '\r']).to_owned(),
                    )
                })
                .filter(|(_, text)| !text.is_empty())
                .collect(),
        );
    }
    Some(out)
}

/// 文法定義の読み込みは重いので、最初にプレビューしたときだけ行う（起動は速いまま）。
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(tokyo_night)
}

fn to_color(color: SynColor) -> Color {
    Color::Rgb {
        rgba: u32::from_be_bytes([color.r, color.g, color.b, 0xFF]),
    }
}

/// Tokyo Night（nvim の tokyonight-night 相当）の配色。
fn tokyo_night() -> Theme {
    Theme {
        name: Some("gototerm-tokyo-night".to_owned()),
        settings: ThemeSettings {
            foreground: Some(rgb(0xC0CAF5)),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("comment, punctuation.definition.comment", 0x565F89),
            item(
                "string, punctuation.definition.string, constant.character",
                0x9ECE6A,
            ),
            item("constant.numeric, constant.language", 0xFF9E64),
            item("constant.other, support.constant", 0xFF9E64),
            item("keyword, storage.modifier, keyword.operator.word", 0xBB9AF7),
            item("storage.type, keyword.declaration", 0x9D7CD8),
            item("keyword.operator, punctuation.separator", 0x89DDFF),
            item("entity.name.function, support.function, meta.function-call", 0x7AA2F7),
            item("entity.name.type, entity.other.inherited-class, support.type", 0x2AC3DE),
            item("entity.name.tag, meta.tag", 0xF7768E),
            item("entity.other.attribute-name", 0xBB9AF7),
            item("variable.parameter", 0xE0AF68),
            item("variable.function, variable.annotation", 0x7AA2F7),
            item("meta.preprocessor, meta.annotation", 0x7DCFFF),
            item("invalid", 0xF7768E),
            // Markdown・設定ファイルの見出しやキー。
            item("markup.heading, entity.name.section", 0x7AA2F7),
            item("markup.bold", 0xE0AF68),
            item("support.type.property-name, meta.mapping.key", 0x7AA2F7),
        ],
        ..Theme::default()
    }
}

fn item(selectors: &str, color: u32) -> ThemeItem {
    ThemeItem {
        // セレクタは固定の文字列。壊れていたらテスト（theme_selectors_parse）で落ちる。
        scope: ScopeSelectors::from_str(selectors).expect("scope selector"),
        style: StyleModifier {
            foreground: Some(rgb(color)),
            background: None,
            font_style: None,
        },
    }
}

fn rgb(color: u32) -> SynColor {
    let [_, r, g, b] = color.to_be_bytes();
    SynColor { r, g, b, a: 0xFF }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// terminal::Color は比較できないので、テストでは色の数値を取り出して見る。
    fn rgba_of(color: &Color) -> u32 {
        match color {
            Color::Rgb { rgba } => *rgba,
            other => panic!("ハイライトは Rgb 色を返すはず: {other:?}"),
        }
    }

    const COMMENT: u32 = 0x565F_89FF;
    const STRING: u32 = 0x9ECE_6AFF;

    /// 配色表のセレクタが全部 syntect で解釈できること（expect で落ちないこと）。
    #[test]
    fn theme_selectors_parse() {
        assert_eq!(tokyo_night().scopes.len(), 18);
    }

    #[test]
    fn highlights_rust_comments_and_strings_differently() {
        let lines = highlight_source("// note\nlet s = \"hi\";\n", Path::new("a.rs")).unwrap();

        assert_eq!(rgba_of(&lines[0][0].0), COMMENT);

        // 行の中に文字列の緑がいること。
        let has_string = lines[1]
            .iter()
            .any(|(color, text)| rgba_of(color) == STRING && text.contains("hi"));
        assert!(has_string, "文字列が緑になっていない: {:?}", lines[1]);
    }

    #[test]
    fn keeps_every_source_line() {
        let lines = highlight_source("let a = 1;\n\nlet b = 2;\n", Path::new("a.rs")).unwrap();
        assert_eq!(lines.len(), 3, "空行を落とさない");
        assert!(lines[1].is_empty(), "空行は断片なしの行になる");
    }

    #[test]
    fn unknown_extension_falls_back_to_plain() {
        assert!(highlight_source("hello", Path::new("a.unknownext")).is_none());
    }

    #[test]
    fn code_block_uses_the_fence_language() {
        assert!(highlight_code_block("let x = 1;", "rust").is_some());
        assert!(highlight_code_block("let x = 1;", "").is_none());
        assert!(highlight_code_block("x", "not-a-language").is_none());
    }

    /// 長すぎるファイルは素のテキストに落とす（描画が引っかからないように）。
    #[test]
    fn very_long_files_are_not_highlighted() {
        let long = "let x = 1;\n".repeat(MAX_LINES + 1);
        assert!(highlight_source(&long, Path::new("a.rs")).is_none());
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use std::time::Instant;

    /// 実ファイル（このリポジトリの reader.rs）1本にかかる時間を見る。
    /// AI がファイルを書くたびに走るので、描画が引っかからない範囲か確かめる。
    #[test]
    #[ignore]
    fn measure_real_file() {
        let text = std::fs::read_to_string("src/reader.rs").unwrap();
        // 文法定義の読み込み（初回だけ）を分けて測る。
        let t0 = Instant::now();
        syntaxes();
        let load = t0.elapsed();

        let t1 = Instant::now();
        let lines = highlight_source(&text, Path::new("src/reader.rs")).unwrap();
        let hl = t1.elapsed();
        println!(
            "文法読み込み {load:?} / {} 行のハイライト {hl:?}",
            lines.len()
        );
    }
}
