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

/// グリフのラスタライズ方法。全プラットフォームでライトヒンティングを使う。
///
/// 一度 Windows だけフルヒンティング（TARGET_LIGHT なし）にしたが、実機で
/// PowerLine の区切り記号の継ぎ目が汚くなった。フルヒンティングは輪郭を
/// ピクセル格子に横方向まで吸着させるため、タイル状に連結する前提の記号では
/// 隣のセルとの境界がズレる。ライトヒンティングは縦方向だけ吸着させるので
/// 送り幅が保たれ、記号が隙間なく繋がる。
fn glyph_load_flags() -> LoadFlag {
    LoadFlag::RENDER | LoadFlag::TARGET_LIGHT
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

    /// フォントが宣言している行の情報（ascender, 行の高さ）を px で返す。
    ///
    /// ASCII のインク範囲から高さを決めると、フォントが確保している行の高さより
    /// 小さくなる（JetBrains Mono NF の font_size=24 で実測 26px 対 31.7px＝18%小さい）。
    /// セルが縮むと1文字あたりのピクセルが減って解像度が落ち、セルいっぱいに
    /// 設計された PowerLine の記号も収まらなくなる。
    fn line_metrics(&self) -> Option<(i32, i32)> {
        let m = self.face.size_metrics()?;
        Some(((m.ascender >> 6) as i32, (m.height >> 6) as i32))
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
            let flags = glyph_load_flags();
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

    pub fn set_fontsize(&mut self, new_size: u32) {
        self.font_size = new_size;
        for list in self.fonts.values_mut() {
            for f in list.iter_mut() {
                f.set_fontsize(new_size);
            }
        }
    }

    /// 主フォント（Regular の先頭）が宣言している (ascender, 行の高さ) を px で返す。
    ///
    /// フォールバックではなく主フォントを見るのが重要。和文フォントは行が高い
    /// ものが多く（Noto Sans CJK は ascender が em の 1.16 倍）、最大値を採ると
    /// 行間が不必要に広がってしまう。セル幅を主フォントから決めているのと揃える。
    pub fn primary_line_metrics(&self) -> Option<(i32, i32)> {
        self.fonts.get(&FontStyle::Regular)?.first()?.line_metrics()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PowerLine の記号が隣のセルと隙間なく繋がるかは送り幅が保たれるかで決まる。
    // ライトヒンティングを外すと実機で継ぎ目が崩れたので、外れていないことを見る。
    #[test]
    fn glyphs_keep_light_hinting_on_every_platform() {
        assert!(glyph_load_flags().contains(LoadFlag::TARGET_LIGHT));
        assert!(glyph_load_flags().contains(LoadFlag::RENDER));
    }

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
