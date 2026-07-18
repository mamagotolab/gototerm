use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use freetype::{
    face::{Face, LoadFlag},
    GlyphMetrics, Library,
};
use glium::texture::RawImage2d;

thread_local! {
    // FreeType の Library はプロセス（メインスレッド）で1つだけ。フォントの数だけ
    // Library::init していた（ビュー×フォント×スタイルで数十個）のを1つに集約する。
    // フォント操作は全てメインスレッドなので thread_local で足りる。
    static FT_LIBRARY: Library = freetype::Library::init().expect("FreeType init");
}

pub struct Font {
    face: Face,
}

impl Font {
    // 埋め込みフォント等、メモリ上のバイト列から作る。データは Rc で共有され、
    // FreeType にもそのまま渡す（FT_New_Memory_Face はバッファをコピーしない）。
    pub fn from_memory(ttf_data: Rc<Vec<u8>>, index: isize) -> Self {
        let face = FT_LIBRARY.with(|lib| lib.new_memory_face(ttf_data, index).unwrap());
        Self { face }
    }

    // ディスク上のフォントファイルから作る。FreeType が必要なテーブル・グリフだけを
    // 遅延読みするので、巨大な CJK フォント（NotoCJK は約 19MB）を丸ごとメモリに
    // 載せずに済む。ASCII 中心のセッションでは CJK グリフはほとんど読まれない。
    pub fn from_file(path: &Path, index: isize) -> Result<Self, String> {
        let face = FT_LIBRARY
            .with(|lib| lib.new_face(path, index))
            .map_err(|e| e.to_string())?;
        Ok(Self { face })
    }

    fn set_fontsize(&mut self, size: u32) {
        self.face.set_pixel_sizes(0, size).unwrap();
    }

    fn metrics(&self, ch: char) -> Option<GlyphMetrics> {
        if let idx @ 1.. = self.face.get_char_index(ch as usize) {
            self.face.load_glyph(idx, LoadFlag::DEFAULT).expect("load");
            Some(self.face.glyph().metrics())
        } else {
            None
        }
    }

    fn render(&self, ch: char) -> Option<(RawImage2d<'_, u8>, GlyphMetrics)> {
        if let idx @ 1.. = self.face.get_char_index(ch as usize) {
            let flags = LoadFlag::RENDER | LoadFlag::TARGET_LIGHT;
            self.face.load_glyph(idx, flags).expect("render");
            let glyph = self.face.glyph();

            let bitmap = glyph.bitmap();
            let metrics = glyph.metrics();

            let width = bitmap.width() as u32;
            let height = bitmap.rows() as u32;

            // 空グリフ（スペース等）では freetype の buffer が null になり、
            // bitmap.buffer() 内の slice::from_raw_parts が新しい rustc の
            // 非null前提に引っかかって panic する。空のときは触らない。
            let data = if width == 0 || height == 0 {
                Vec::new()
            } else {
                bitmap.buffer().to_vec()
            };

            let raw_image = RawImage2d {
                data: data.into(),
                width,
                height,
                format: glium::texture::ClientFormat::U8,
            };

            Some((raw_image, metrics))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
#[repr(u8)]
pub enum FontStyle {
    Regular,
    Bold,
    Faint,
}

impl FontStyle {
    pub const fn all() -> [FontStyle; 3] {
        [FontStyle::Regular, FontStyle::Bold, FontStyle::Faint]
    }
}

pub struct FontSet {
    fonts: HashMap<FontStyle, Vec<Font>>,
    font_size: u32,
}

impl FontSet {
    pub fn new(font_size: u32) -> Self {
        FontSet {
            fonts: HashMap::new(),
            font_size,
        }
    }

    pub fn add(&mut self, style: FontStyle, mut font: Font) {
        font.set_fontsize(self.font_size);
        let list = self.fonts.entry(style).or_insert_with(Vec::new);
        list.push(font);
    }

    pub fn metrics(&self, ch: char, style: FontStyle) -> Option<GlyphMetrics> {
        self.fonts.get(&style)?.iter().find_map(|f| f.metrics(ch))
    }

    pub fn render(&self, ch: char, style: FontStyle) -> Option<(RawImage2d<'_, u8>, GlyphMetrics)> {
        self.fonts.get(&style)?.iter().find_map(|f| f.render(ch))
    }

    pub fn fontsize(&self) -> u32 {
        self.font_size
    }

    pub fn set_fontsize(&mut self, new_size: u32) {
        self.font_size = new_size;
        for list in self.fonts.values_mut() {
            for f in list.iter_mut() {
                f.set_fontsize(new_size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ディスク上の実フォントを FreeType にストリームさせても（from_file）、
    // ASCII と CJK の両方のグリフが引けることを確認する。file-based にしても
    // グリフ解決のパスは new_memory_face と同一なので、これが通れば描画は不変。
    // フォントが無い環境（CI 等）ではスキップする。
    #[test]
    fn from_file_resolves_ascii_and_cjk_glyphs() {
        let candidates = [
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ];
        let Some(path) = candidates.iter().map(Path::new).find(|p| p.exists()) else {
            eprintln!("skip: NotoSansCJK が見つからないためスキップ");
            return;
        };

        let mut font = Font::from_file(path, 0).expect("load CJK font from file");
        font.set_fontsize(16);

        assert!(font.metrics('A').is_some(), "ASCII 'A' が引けること");
        assert!(font.metrics('日').is_some(), "漢字 '日' が引けること");
        assert!(
            font.render('語').is_some(),
            "漢字 '語' がラスタライズできること"
        );
    }
}
