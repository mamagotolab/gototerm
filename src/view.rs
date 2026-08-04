use crate::Display;
use glium::{index, texture, uniform, uniforms};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::cache::GlyphCache;
use crate::font::{Font, FontSet, FontStyle};
use crate::terminal::{CellSize, Color, Cursor, CursorStyle, Line, PositionedImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Viewport {
    #[allow(unused)]
    pub fn contains(&self, p: PhysicalPosition<f64>) -> bool {
        let l = self.x as f64;
        let r = (self.x + self.w) as f64;
        let t = self.y as f64;
        let b = (self.y + self.h) as f64;
        l <= p.x && p.x < r && t <= p.y && p.y < b
    }

    fn to_glium_rect(self, inner_size: PhysicalSize<u32>) -> glium::Rect {
        let bottom = inner_size.height as i64 - (self.y + self.h) as i64;
        glium::Rect {
            left: self.x,
            bottom: max(bottom, 0) as u32,
            width: self.w,
            height: self.h,
        }
    }
}

/// テキスト選択。Linear=行方向の連続範囲（通常選択）、
/// Block=矩形選択（行範囲 × 列範囲）。いずれも閉区間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Linear {
        left: usize,
        right: usize,
    },
    Block {
        top: usize,
        bottom: usize,
        left: usize,
        right: usize,
    },
}

fn normalized_scale_factor(scale_factor: f64) -> f64 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

pub(crate) fn physical_font_size(logical: u32, scale_factor: f64) -> u32 {
    ((logical.max(1) as f64 * normalized_scale_factor(scale_factor)).round() as u32).max(1)
}

/// PowerLine の区切り記号か。セルの端から端まで塗りつぶして隣のセルと連結する
/// 前提でデザインされている文字（Nerd Font の私用領域）。
///
/// これらは「グリフ本来の大きさ」で置くと継ぎ目が汚くなる。セルの高さは ASCII の
/// 縦bboxから決めており（calculate_cell_size）、フォントの行の高さより小さいため、
/// 実測で font_size=24 のとき セル26px に対しグリフ30px と上下2pxずつはみ出した。
/// はみ出した分は隣の行の背景に塗り潰されるので、段差や隙間として見える。
///
/// U+E0A0〜E0AF（ブランチ・鍵などのアイコン）は通常の文字なので含めない。
/// 罫線素片やブロック要素も含めない（線の太さや高さが設計値なので、セルへ
/// 伸縮すると逆に崩れる）。
fn is_cell_filling_separator(ch: char) -> bool {
    matches!(ch, '\u{E0B0}'..='\u{E0BF}')
}

/// セル背景とまったく同じ矩形。区切り記号をここへ描けば帯と端が一致する。
fn cell_rect(row: usize, leftline: u32, cell_size: CellSize, cell_width_px: u32) -> PixelRect {
    PixelRect {
        x: leftline as i32,
        y: (row as u32 * cell_size.h) as i32,
        w: cell_width_px,
        h: cell_size.h,
    }
}

pub(crate) fn scale_change_requires_rebuild(old_scale: f64, new_scale: f64, logical: u32) -> bool {
    physical_font_size(logical, old_scale) != physical_font_size(logical, new_scale)
}

pub(crate) struct LazySlot<T> {
    value: Option<T>,
}

impl<T> LazySlot<T> {
    pub(crate) fn new() -> Self {
        Self { value: None }
    }

    pub(crate) fn ensure_with(&mut self, factory: impl FnOnce() -> T) -> &mut T {
        self.value.get_or_insert_with(factory)
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.value.is_some()
    }

    fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    fn get_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TerminalViewSpec {
    viewport: Viewport,
    logical_font_size: u32,
    scale_factor: f64,
    scroll_bar: Option<(u32, u32)>,
}

struct LazyTerminalState<T> {
    viewport: Viewport,
    base_font_size: u32,
    font_diff: i32,
    scale_factor: f64,
    scroll_bar: Option<(u32, u32)>,
    view: LazySlot<T>,
}

impl<T> LazyTerminalState<T> {
    fn new(
        viewport: Viewport,
        logical_font_size: u32,
        scale_factor: f64,
        scroll_bar: Option<(u32, u32)>,
    ) -> Self {
        Self {
            viewport,
            base_font_size: logical_font_size,
            font_diff: 0,
            scale_factor: normalized_scale_factor(scale_factor),
            scroll_bar,
            view: LazySlot::new(),
        }
    }

    fn logical_font_size(&self) -> u32 {
        (self.base_font_size as i64 + self.font_diff as i64).max(1) as u32
    }

    fn spec(&self) -> TerminalViewSpec {
        TerminalViewSpec {
            viewport: self.viewport,
            logical_font_size: self.logical_font_size(),
            scale_factor: self.scale_factor,
            scroll_bar: self.scroll_bar,
        }
    }

    fn ensure_with(&mut self, factory: impl FnOnce(TerminalViewSpec) -> T) -> bool {
        if self.view.is_initialized() {
            return false;
        }
        let spec = self.spec();
        self.view.ensure_with(|| factory(spec));
        true
    }

    fn is_initialized(&self) -> bool {
        self.view.is_initialized()
    }

    fn viewport(&self) -> Viewport {
        self.viewport
    }

    fn set_viewport(&mut self, viewport: Viewport, apply: impl FnOnce(&mut T, Viewport)) {
        self.viewport = viewport;
        if let Some(view) = self.view.get_mut() {
            apply(view, viewport);
        }
    }

    fn increase_font_size(
        &mut self,
        size_diff: i32,
        apply: impl FnOnce(&mut T, i32) -> bool,
    ) -> bool {
        let old_size = self.logical_font_size();
        self.font_diff = self.font_diff.saturating_add(size_diff);
        let new_size = self.logical_font_size();
        let Some(view) = self.view.get_mut() else {
            return false;
        };
        apply(view, new_size as i32 - old_size as i32)
    }

    fn set_scale_factor(
        &mut self,
        scale_factor: f64,
        apply: impl FnOnce(&mut T, f64) -> bool,
    ) -> bool {
        self.scale_factor = normalized_scale_factor(scale_factor);
        let Some(view) = self.view.get_mut() else {
            return false;
        };
        apply(view, self.scale_factor)
    }

    fn get(&self) -> Option<&T> {
        self.view.get()
    }

    fn get_mut(&mut self) -> Option<&mut T> {
        self.view.get_mut()
    }
}

/// OpenGL-backed terminal rendering resources which are created only when the
/// workbench is first shown. Geometry and font state remain current while the
/// resource is absent, so the first construction uses the latest window state.
pub(crate) struct LazyTerminalView {
    display: Display,
    state: LazyTerminalState<TerminalView>,
}

impl LazyTerminalView {
    pub(crate) fn new(
        display: Display,
        viewport: Viewport,
        logical_font_size: u32,
        scale_factor: f64,
        scroll_bar: Option<(u32, u32)>,
    ) -> Self {
        Self {
            display,
            state: LazyTerminalState::new(viewport, logical_font_size, scale_factor, scroll_bar),
        }
    }

    pub(crate) fn ensure_initialized(&mut self) -> bool {
        let display = self.display.clone();
        self.state.ensure_with(|spec| {
            log::debug!("initializing lazy workbench terminal view");
            TerminalView::with_viewport(
                display,
                spec.viewport,
                spec.logical_font_size,
                spec.scale_factor,
                spec.scroll_bar,
            )
        })
    }

    pub(crate) fn is_initialized(&self) -> bool {
        self.state.is_initialized()
    }

    pub(crate) fn viewport(&self) -> Viewport {
        self.state.viewport()
    }

    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        self.state
            .set_viewport(viewport, |view, viewport| view.set_viewport(viewport));
    }

    pub(crate) fn increase_font_size(&mut self, size_diff: i32) -> bool {
        self.state.increase_font_size(size_diff, |view, size_diff| {
            view.increase_font_size(size_diff)
        })
    }

    pub(crate) fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        self.state
            .set_scale_factor(scale_factor, |view, scale_factor| {
                view.set_scale_factor(scale_factor)
            })
    }

    pub(crate) fn get(&self) -> Option<&TerminalView> {
        self.state.get()
    }

    pub(crate) fn get_mut(&mut self) -> Option<&mut TerminalView> {
        self.state.get_mut()
    }
}

#[cfg(test)]
mod lazy_tests {
    use super::{LazySlot, LazyTerminalState, TerminalViewSpec, Viewport};
    use std::cell::Cell;

    #[derive(Debug)]
    struct FakeView {
        created_with: TerminalViewSpec,
        viewport_updates: Vec<Viewport>,
        font_updates: Vec<i32>,
        scale_updates: Vec<f64>,
    }

    #[test]
    fn lazy_resource_constructs_once_on_first_access() {
        let calls = Cell::new(0);
        let mut lazy = LazySlot::new();

        assert!(!lazy.is_initialized());
        assert_eq!(
            *lazy.ensure_with(|| {
                calls.set(calls.get() + 1);
                42
            }),
            42
        );
        assert_eq!(
            *lazy.ensure_with(|| {
                calls.set(calls.get() + 1);
                99
            }),
            42
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn lazy_resource_uses_state_from_the_first_access() {
        let mut lazy = LazySlot::new();
        let mut latest_scale = 1.0;
        let mut latest_font_size = 18;

        assert_eq!((latest_scale, latest_font_size), (1.0, 18));
        latest_scale = 1.5;
        latest_font_size += 2;
        let resource = lazy.ensure_with(|| (latest_scale, latest_font_size));

        assert_eq!(*resource, (1.5, 20));
    }

    #[test]
    fn lazy_terminal_state_uses_latest_spec_once_and_forwards_later_updates() {
        let initial = Viewport {
            x: 0,
            y: 0,
            w: 800,
            h: 600,
        };
        let latest = Viewport {
            x: 20,
            y: 30,
            w: 1200,
            h: 700,
        };
        let after_init = Viewport {
            x: 40,
            y: 50,
            w: 1000,
            h: 650,
        };
        let calls = Cell::new(0);
        let mut state = LazyTerminalState::<FakeView>::new(initial, 18, 1.0, None);

        state.set_viewport(latest, |view, viewport| {
            view.viewport_updates.push(viewport)
        });
        assert!(!state.increase_font_size(3, |view, diff| {
            view.font_updates.push(diff);
            true
        }));
        assert!(!state.increase_font_size(-1, |view, diff| {
            view.font_updates.push(diff);
            true
        }));
        assert!(!state.set_scale_factor(1.5, |view, scale| {
            view.scale_updates.push(scale);
            true
        }));

        assert!(state.ensure_with(|spec| {
            calls.set(calls.get() + 1);
            FakeView {
                created_with: spec,
                viewport_updates: Vec::new(),
                font_updates: Vec::new(),
                scale_updates: Vec::new(),
            }
        }));
        assert!(!state.ensure_with(|_| panic!("factory called twice")));
        assert_eq!(calls.get(), 1);
        assert_eq!(
            state.get().unwrap().created_with,
            TerminalViewSpec {
                viewport: latest,
                logical_font_size: 20,
                scale_factor: 1.5,
                scroll_bar: None,
            }
        );

        state.set_viewport(after_init, |view, viewport| {
            view.viewport_updates.push(viewport)
        });
        assert!(state.increase_font_size(-2, |view, diff| {
            view.font_updates.push(diff);
            true
        }));
        assert!(state.set_scale_factor(2.0, |view, scale| {
            view.scale_updates.push(scale);
            true
        }));
        assert!(!state.ensure_with(|_| panic!("factory called after updates")));

        let view = state.get().unwrap();
        assert_eq!(view.viewport_updates, vec![after_init]);
        assert_eq!(view.font_updates, vec![-2]);
        assert_eq!(view.scale_updates, vec![2.0]);
        assert_eq!(calls.get(), 1);
    }
}

pub struct TerminalView {
    fonts: FontSet,
    cache: GlyphCache,
    logical_font_size: u32,
    scale_factor: f64,
    viewport: Viewport,
    cell_size: CellSize,
    cell_max_over: i32,

    pub lines: Vec<Line>,
    pub images: Vec<PositionedImage>,
    pub cursor: Option<Cursor>,
    // IME 変換中（未確定）の文字列。確定すると空になる。
    pub preedit: String,
    pub selection_range: Option<Selection>,
    pub scroll_bar: Option<(u32, u32)>,
    pub bg_color: Color,
    /// 既定背景色のセルで背景クアッドを描かず、下地の clear に任せるか。
    /// パネル（サイドバー/プレビュー）で true にすると、不透明な下地に半透明の
    /// セル背景が重なってアルファが落ち壁紙が透ける不具合を避けられる。
    /// ターミナルは透過がこのクアッド由来なので false のまま。
    pub skip_default_bg: bool,
    pub view_focused: bool,
    // カーソル点滅の表示フェーズ。false の間はカーソルを描かない。
    cursor_blink_on: bool,
    updated: bool,

    display: Display,
    draw_params: glium::DrawParameters<'static>,
    program_cell: glium::Program,
    program_img: glium::Program,
    vertices_fg: Vec<CellVertex>,
    vertices_bg: Vec<CellVertex>,
    // PowerLine の区切り記号だけを分けて積む。セルに合わせて縮めるので、
    // Nearest だと斜辺の刻みが不均一になる（実測で 1px ずつでなく飛ぶ）。
    // ここだけ Linear で描くため描画バッチを分ける。
    vertices_scaled: Vec<CellVertex>,
    draw_queries_fg: Vec<DrawQuery<CellVertex>>,
    draw_queries_bg: Vec<DrawQuery<CellVertex>>,
    draw_queries_scaled: Vec<DrawQuery<CellVertex>>,
    draw_queries_img: Vec<DrawQuery<ImageVertex>>,
    clock: std::time::Instant,
}

struct DrawQuery<V: glium::vertex::Vertex> {
    vertices: glium::VertexBuffer<V>,
    texture: Rc<texture::Texture2d>,
}

impl TerminalView {
    pub fn with_viewport(
        display: Display,
        viewport: Viewport,
        logical_font_size: u32,
        scale_factor: f64,
        scroll_bar: Option<(u32, u32)>,
    ) -> Self {
        let scale_factor = normalized_scale_factor(scale_factor);
        let fonts = build_font_set(physical_font_size(logical_font_size, scale_factor));

        let (cell_size, cell_max_over) = calculate_cell_size(&fonts);

        // Rasterize ASCII characters and cache them as a texture
        let cache = GlyphCache::build_ascii_visible(&display, &fonts, cell_size);

        let draw_params = glium::DrawParameters {
            blend: glium::Blend::alpha_blending(),
            viewport: {
                let (w, h) = display.get_framebuffer_dimensions();
                Some(viewport.to_glium_rect(PhysicalSize::new(w, h)))
            },
            ..glium::DrawParameters::default()
        };

        fn new_program(display: &Display, vert: &str, frag: &str) -> glium::Program {
            use glium::program::{Program, ProgramCreationInput};
            Program::new(
                display,
                ProgramCreationInput::SourceCode {
                    vertex_shader: vert,
                    fragment_shader: frag,
                    geometry_shader: None,
                    tessellation_control_shader: None,
                    tessellation_evaluation_shader: None,
                    transform_feedback_varyings: None,
                    outputs_srgb: true,
                    uses_point_size: false,
                },
            )
            .unwrap()
        }

        let program_cell = new_program(
            &display,
            include_str!("shaders/cell.vert"),
            include_str!("shaders/cell.frag"),
        );

        let program_img = new_program(
            &display,
            include_str!("shaders/image.vert"),
            include_str!("shaders/image.frag"),
        );

        TerminalView {
            fonts,
            cache,
            logical_font_size,
            scale_factor,

            viewport,
            cell_size,
            cell_max_over,

            lines: Vec::new(),
            images: Vec::new(),
            cursor: None,
            preedit: String::new(),
            selection_range: None,
            scroll_bar,
            bg_color: Color::Black,
            skip_default_bg: false,
            view_focused: false,
            cursor_blink_on: true,
            updated: false,

            display,
            draw_params,
            program_cell,
            program_img,
            vertices_fg: Vec::new(),
            vertices_bg: Vec::new(),
            vertices_scaled: Vec::new(),
            draw_queries_fg: Vec::new(),
            draw_queries_bg: Vec::new(),
            draw_queries_scaled: Vec::new(),
            draw_queries_img: Vec::new(),
            clock: std::time::Instant::now(),
        }
    }

    pub fn update_contents<F>(&mut self, callback: F)
    where
        F: FnOnce(&mut Self),
    {
        callback(self);
        self.updated = true;
    }

    /// 再描画が必要か（前回の draw 以降に内容が変わったか）。
    /// 変化が無いフレームでスワップしないことで、ウィンドウが隠れたときの
    /// 「コンポジタ待ちでスワップがブロック→無応答」を防ぐ。
    pub fn needs_redraw(&self) -> bool {
        self.updated
    }

    /// カーソル点滅の表示フェーズを切り替える。変化したときだけ再描画を促す。
    pub fn set_cursor_blink(&mut self, on: bool) {
        if self.cursor_blink_on != on {
            self.cursor_blink_on = on;
            self.updated = true;
        }
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn set_viewport(&mut self, new_viewport: Viewport) {
        log::debug!("viewport changed: {:?}", new_viewport);
        self.viewport = new_viewport;

        let (w, h) = self.display.get_framebuffer_dimensions();
        self.draw_params.viewport = Some(self.viewport.to_glium_rect(PhysicalSize::new(w, h)));

        self.updated = true;
    }

    pub fn cell_size(&self) -> CellSize {
        self.cell_size
    }

    pub fn increase_font_size(&mut self, size_diff: i32) -> bool {
        log::debug!("increase font size: {} (diff)", size_diff);

        let new_logical_size = (self.logical_font_size as i32 + size_diff).max(1) as u32;
        let old_physical_size = physical_font_size(self.logical_font_size, self.scale_factor);
        let new_physical_size = physical_font_size(new_logical_size, self.scale_factor);
        self.logical_font_size = new_logical_size;
        if old_physical_size == new_physical_size {
            return false;
        }

        self.rebuild_font(new_physical_size);
        true
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        let scale_factor = normalized_scale_factor(scale_factor);
        let rebuild =
            scale_change_requires_rebuild(self.scale_factor, scale_factor, self.logical_font_size);
        self.scale_factor = scale_factor;
        if rebuild {
            self.rebuild_font(physical_font_size(self.logical_font_size, scale_factor));
        }
        rebuild
    }

    fn rebuild_font(&mut self, physical_font_size: u32) {
        self.fonts.set_fontsize(physical_font_size);
        let (new_cell_size, new_cell_max_over) = calculate_cell_size(&self.fonts);
        self.cell_size = new_cell_size;
        self.cell_max_over = new_cell_max_over;

        self.cache = GlyphCache::build_ascii_visible(&self.display, &self.fonts, self.cell_size);

        self.updated = true;
    }

    fn rebuild_draw_queries(&mut self) {
        let viewport = self.viewport;
        let cell_size = self.cell_size;
        let timestamp = self.clock.elapsed().as_millis() as u64;

        self.draw_queries_img.clear();
        for img in self.images.iter() {
            let col = img.col;
            let row = img.row;

            let image_rect = PixelRect {
                x: col as i32 * cell_size.w as i32,
                y: row as i32 * cell_size.h as i32,
                w: img.width as u32,
                h: img.height as u32,
            };
            let vs = image_vertices(image_rect.to_gl(viewport));

            let vertices = glium::VertexBuffer::new(&self.display, &vs).unwrap();

            // データ長が寸法と合わない・寸法が0・GLの上限超過などで失敗し得る。
            // 1枚の不正な画像でアプリ全体を落とさないよう、失敗したらスキップする。
            let expected = (img.width * img.height * 3) as usize;
            if img.width == 0 || img.height == 0 || img.data.len() < expected {
                continue;
            }
            let texture = match texture::Texture2d::with_mipmaps(
                &self.display,
                glium::texture::RawImage2d {
                    data: img.data.clone().into(),
                    width: img.width as u32,
                    height: img.height as u32,
                    format: glium::texture::ClientFormat::U8U8U8,
                },
                texture::MipmapsOption::NoMipmap,
            ) {
                Ok(texture) => texture,
                Err(_) => continue,
            };

            self.draw_queries_img.push(DrawQuery {
                vertices,
                texture: Rc::new(texture),
            });
        }

        self.vertices_fg.clear();
        self.vertices_bg.clear();
        self.vertices_scaled.clear();
        self.draw_queries_fg.clear();
        self.draw_queries_bg.clear();
        self.draw_queries_scaled.clear();

        // clear entire screen
        {
            let rect = GlRect {
                x: -1.0,
                y: 1.0,
                w: 2.0,
                h: 2.0,
            };
            let fg = Color::White;
            let bg = self.bg_color;
            let vs = rect_vertices(rect, fg, bg);
            self.vertices_bg.extend_from_slice(&vs);
        }

        // scroll bar
        if let Some((sb_origin, sb_length)) = self.scroll_bar {
            let config = &crate::TOYTERM_CONFIG;
            if config.scroll_bar_width > 0 {
                let sb_width = config.scroll_bar_width;

                let mut rect = PixelRect {
                    x: viewport.w.saturating_sub(sb_width) as i32,
                    y: 0,
                    w: sb_width,
                    h: viewport.h,
                };
                let fg = Color::White;
                let bg = Color::Rgb {
                    rgba: config.scroll_bar_bg_color,
                };
                let vs = rect_vertices(rect.to_gl(viewport), fg, bg);
                self.vertices_bg.extend_from_slice(&vs);

                rect.y = sb_origin as i32;
                rect.h = sb_length;
                let fg = Color::White;
                let bg = Color::Rgb {
                    rgba: config.scroll_bar_fg_color,
                };
                let vs = rect_vertices(rect.to_gl(viewport), fg, bg);
                self.vertices_bg.extend_from_slice(&vs);
            }
        }

        let texture = self.cache.texture();

        let mut baseline: u32 = self.cell_max_over as u32;
        for (i, row) in self.lines.iter().enumerate() {
            let cols = row.columns();
            let mut leftline: u32 = 0;
            for (j, cell) in row.iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }

                let cell_width_px = cell_size.w * cell.width as u32;

                let style = if cell.attr.bold == -1 {
                    FontStyle::Faint
                } else if cell.attr.bold == 0 {
                    FontStyle::Regular
                } else {
                    FontStyle::Bold
                };

                let (fg, bg) = {
                    let mut fg = cell.attr.fg;
                    let mut bg = cell.attr.bg;

                    if cell.attr.inversed {
                        std::mem::swap(&mut fg, &mut bg);
                    }

                    let on_cursor = if let Some(cursor) = self.cursor {
                        self.view_focused
                            && self.cursor_blink_on
                            && cursor.style == CursorStyle::Block
                            && i == cursor.row
                            && j == cursor.col
                    } else {
                        false
                    };

                    let is_selected = match self.selection_range {
                        Some(Selection::Linear { left, right }) => {
                            let offset = i * cols + j;
                            let center = offset + (cell.width / 2) as usize;
                            left <= center && center <= right
                        }
                        Some(Selection::Block {
                            top,
                            bottom,
                            left,
                            right,
                        }) => top <= i && i <= bottom && left <= j && j <= right,
                        None => false,
                    };

                    // マウス選択範囲はセレクション色で塗る。
                    if is_selected {
                        bg = Color::Selection;
                    }

                    // ブロックカーソルは「いまのセルの色」を反転して描く（reverse video）。
                    // 固定色だと nvim の CursorLine / Visual 選択と色が被って、
                    // どこにカーソルがあるか分からなくなる。反転なら下地が
                    // 何色でも必ずコントラストが出る。
                    if on_cursor {
                        std::mem::swap(&mut fg, &mut bg);
                    }

                    if cell.attr.concealed {
                        fg = bg;
                    }

                    (fg, bg)
                };

                let blinking = cell.attr.blinking;

                // Background
                // パネル（skip_default_bg=true）では、セル背景が既定の背景色の
                // ときに背景クアッドを描かず下地の clear に任せる。半透明の背景色を
                // 不透明な下地に重ねると二重合成でアルファが下がり、そのセルだけ
                // 壁紙が透けて灰色ブロックになるのを避けるため。反転・選択・カーソルで
                // 具体色になったセルは bg != Background なので従来どおり描く。
                // ターミナルは透過がこのクアッド由来なので描く（skip_default_bg=false）。
                if !(self.skip_default_bg && matches!(bg, Color::Background)) {
                    let rect = PixelRect {
                        x: (j as u32 * cell_size.w) as i32,
                        y: (i as u32 * cell_size.h) as i32,
                        w: cell_width_px,
                        h: cell_size.h,
                    };

                    let vs = rect_vertices(rect.to_gl(viewport), fg, bg);
                    self.vertices_bg.extend_from_slice(&vs);
                }

                match self
                    .cache
                    .get_or_insert(cell.ch, style, &self.fonts, timestamp)
                {
                    Ok(Some((region, metrics))) => {
                        if !region.is_empty() {
                            let rect = if is_cell_filling_separator(cell.ch) {
                                cell_rect(i, leftline, cell_size, cell_width_px)
                            } else {
                                let bearing_x = (metrics.horiBearingX >> 6) as u32;
                                let bearing_y = (metrics.horiBearingY >> 6) as u32;

                                PixelRect {
                                    x: leftline as i32 + bearing_x as i32,
                                    y: baseline as i32 - bearing_y as i32,
                                    w: region.w,
                                    h: region.h,
                                }
                            };
                            let gl_rect = rect.to_gl(viewport);
                            let uv_rect = region.to_uv(texture.width(), texture.height());

                            let vs = glyph_vertices(gl_rect, uv_rect, fg, bg, blinking);
                            if is_cell_filling_separator(cell.ch) {
                                self.vertices_scaled.extend_from_slice(&vs);
                            } else {
                                self.vertices_fg.extend_from_slice(&vs);
                            }
                        }
                    }
                    Ok(None) => {
                        log::trace!("undefined glyph: {:?}", cell.ch);
                    }
                    Err(_) => {
                        if let Some((glyph_image, metrics)) = self.fonts.render(cell.ch, style) {
                            if glyph_image.width > 0 {
                                log::info!("draw separetely");
                                let rect = if is_cell_filling_separator(cell.ch) {
                                    cell_rect(i, leftline, cell_size, cell_width_px)
                                } else {
                                    let bearing_x = (metrics.horiBearingX >> 6) as u32;
                                    let bearing_y = (metrics.horiBearingY >> 6) as u32;

                                    PixelRect {
                                        x: leftline as i32 + bearing_x as i32,
                                        y: baseline as i32 - bearing_y as i32,
                                        w: glyph_image.width,
                                        h: glyph_image.height,
                                    }
                                };
                                let gl_rect = rect.to_gl(viewport);
                                let uv_rect = UvRect {
                                    x: 0.0,
                                    y: 0.0,
                                    w: 1.0,
                                    h: 1.0,
                                };

                                let vs = glyph_vertices(gl_rect, uv_rect, fg, bg, blinking);

                                let vertex_buffer =
                                    glium::VertexBuffer::new(&self.display, &vs).unwrap();

                                let single_glyph_texture = texture::Texture2d::with_mipmaps(
                                    &self.display,
                                    glyph_image,
                                    texture::MipmapsOption::NoMipmap,
                                )
                                .expect("Failed to create texture");

                                self.draw_queries_fg.push(DrawQuery {
                                    vertices: vertex_buffer,
                                    texture: Rc::new(single_glyph_texture),
                                });
                            }
                        } else {
                            log::trace!("undefined glyph: {:?}", cell.ch);
                        }
                    }
                }

                leftline += cell_width_px;
            }
            baseline += cell_size.h;
        }

        if let Some(cursor) = self.cursor {
            if self.view_focused
                && self.cursor_blink_on
                && matches!(cursor.style, CursorStyle::Underline | CursorStyle::Bar)
            {
                // バー/下線の太さ(px)。セル幅/高さを超えないよう収める。
                let thickness = crate::TOYTERM_CONFIG.cursor_thickness;
                let rect = if cursor.style == CursorStyle::Underline {
                    let t = thickness.min(cell_size.h);
                    PixelRect {
                        x: cursor.col as i32 * cell_size.w as i32,
                        y: (cursor.row + 1) as i32 * cell_size.h as i32 - t as i32,
                        w: cell_size.w,
                        h: t,
                    }
                } else {
                    PixelRect {
                        x: cursor.col as i32 * cell_size.w as i32,
                        y: cursor.row as i32 * cell_size.h as i32,
                        w: thickness.min(cell_size.w),
                        h: cell_size.h,
                    }
                };

                // バー/下線カーソルも、カーソル下のセルの前景色で塗る。
                // 固定色（旧 Color::Selection）だと下地と被って見えないため、
                // ブロックカーソルのリバースビデオと同じく必ずコントラストを出す。
                let cursor_color = self
                    .lines
                    .get(cursor.row)
                    .and_then(|line| {
                        let mut col = 0usize;
                        for cell in line.iter() {
                            let w = cell.width as usize;
                            if w == 0 {
                                continue;
                            }
                            if cursor.col < col + w {
                                return Some(cell.attr.fg);
                            }
                            col += w;
                        }
                        None
                    })
                    .unwrap_or(Color::White);

                let fg = Color::Black;
                let bg = cursor_color;
                let vs = rect_vertices(rect.to_gl(viewport), fg, bg);
                self.vertices_fg.extend_from_slice(&vs);
            }
        }

        // IME 変換中の文字列（preedit）をカーソル位置にインライン描画する。
        // 確定前であることが分かるよう、各文字を下線つきで重ねて描く。
        if !self.preedit.is_empty() {
            if let Some(cursor) = self.cursor {
                use unicode_width::UnicodeWidthChar;

                let style = FontStyle::Regular;
                let row = cursor.row as u32;
                let baseline = self.cell_max_over as u32 + row * cell_size.h;
                let mut leftline = cursor.col as u32 * cell_size.w;

                let fg = Color::White;
                let bg = self.bg_color;
                // preedit の下にある既存文字を確実に隠すため、塗りつぶしは
                // 不透明な背景色にする（透過のままだと下の文字が透けて重なる）。
                // さらに前景バッファに積み、セル本体のグリフより後に描く。
                let opaque_bg = Color::Rgb {
                    rgba: color_to_rgba(self.bg_color) | 0x0000_00FF,
                };

                for ch in self.preedit.chars() {
                    let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if ch_width == 0 {
                        continue;
                    }
                    let cell_width_px = cell_size.w * ch_width as u32;

                    // 背景（下の既存文字・カーソルブロックを不透明に塗りつぶす）
                    let rect = PixelRect {
                        x: leftline as i32,
                        y: (row * cell_size.h) as i32,
                        w: cell_width_px,
                        h: cell_size.h,
                    };
                    let vs = rect_vertices(rect.to_gl(viewport), fg, opaque_bg);
                    self.vertices_fg.extend_from_slice(&vs);

                    // 下線（変換中の目印）
                    let underline = PixelRect {
                        x: leftline as i32,
                        y: ((row + 1) * cell_size.h) as i32 - 2,
                        w: cell_width_px,
                        h: 2,
                    };
                    let vs = rect_vertices(underline.to_gl(viewport), fg, fg);
                    self.vertices_fg.extend_from_slice(&vs);

                    // グリフ
                    if let Ok(Some((region, metrics))) =
                        self.cache.get_or_insert(ch, style, &self.fonts, timestamp)
                    {
                        if !region.is_empty() {
                            let bearing_x = (metrics.horiBearingX >> 6) as u32;
                            let bearing_y = (metrics.horiBearingY >> 6) as u32;
                            let rect = PixelRect {
                                x: leftline as i32 + bearing_x as i32,
                                y: baseline as i32 - bearing_y as i32,
                                w: region.w,
                                h: region.h,
                            };
                            let gl_rect = rect.to_gl(viewport);
                            let uv_rect = region.to_uv(texture.width(), texture.height());
                            let vs = glyph_vertices(gl_rect, uv_rect, fg, bg, 0);
                            self.vertices_fg.extend_from_slice(&vs);
                        }
                    }

                    leftline += cell_width_px;
                }
            }
        }

        // glium 0.34 では空スライスから VertexBuffer を作るとエラーになり、
        // unwrap で落ちる。頂点が無いフレーム（起動直後など）は積まない。
        if !self.vertices_fg.is_empty() {
            let vb_fg = glium::VertexBuffer::new(&self.display, &self.vertices_fg).unwrap();
            self.draw_queries_fg.push(DrawQuery {
                vertices: vb_fg,
                texture: texture.clone(),
            });
        }

        if !self.vertices_scaled.is_empty() {
            let vb = glium::VertexBuffer::new(&self.display, &self.vertices_scaled).unwrap();
            self.draw_queries_scaled.push(DrawQuery {
                vertices: vb,
                texture: texture.clone(),
            });
        }

        if !self.vertices_bg.is_empty() {
            let vb_bg = glium::VertexBuffer::new(&self.display, &self.vertices_bg).unwrap();
            self.draw_queries_bg.push(DrawQuery {
                vertices: vb_bg,
                texture,
            });
        }

        self.updated = false;
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        if self.updated {
            self.rebuild_draw_queries();
        }

        let elapsed = self.clock.elapsed().as_millis() as f32;

        const TRIANGLES: index::NoIndices = index::NoIndices(index::PrimitiveType::TrianglesList);

        let iter_fg = self.draw_queries_fg.iter();
        let iter_bg = self.draw_queries_bg.iter();
        let iter_img = self.draw_queries_img.iter();

        use glium::Surface as _;

        // フレームバッファ全体を背景色でクリアする。ビューポート矩形と
        // フレームバッファ実サイズがズレても、右端・下端がちらつかない。
        {
            let bg = color_to_rgba(self.bg_color);
            let r = ((bg >> 24) & 0xff) as f32 / 255.0;
            let g = ((bg >> 16) & 0xff) as f32 / 255.0;
            let b = ((bg >> 8) & 0xff) as f32 / 255.0;
            let a = (bg & 0xff) as f32 / 255.0;
            // 自分のビューポート矩形だけをクリアする。複数ペインが1つの
            // フレームバッファを分け合うため全体クリアは使えない（他ペインを
            // 消す）。枠の隙間はマネージャ側がフレーム全体クリアで塗る。
            let (fw, fh) = self.display.get_framebuffer_dimensions();
            let rect = self.viewport.to_glium_rect(PhysicalSize::new(fw, fh));
            surface.clear(Some(&rect), Some((r, g, b, a)), true, None, None);
        }

        // 通常の文字は等倍なので Nearest（にじみを抑えて輪郭をくっきりさせる）。
        // PowerLine の区切り記号はセルに合わせて縮めるので Linear。Nearest で縮めると
        // 行が間引かれて斜辺の刻みが不均一になり、継ぎ目が汚く見える。
        for (query, smooth) in iter_bg
            .chain(iter_fg)
            .map(|q| (q, false))
            .chain(self.draw_queries_scaled.iter().map(|q| (q, true)))
        {
            let sampler = if smooth {
                query
                    .texture
                    .sampled()
                    .magnify_filter(uniforms::MagnifySamplerFilter::Linear)
                    .minify_filter(uniforms::MinifySamplerFilter::Linear)
            } else {
                query
                    .texture
                    .sampled()
                    .magnify_filter(uniforms::MagnifySamplerFilter::Nearest)
                    .minify_filter(uniforms::MinifySamplerFilter::Nearest)
            };
            let uniforms = uniform! { tex: sampler, timestamp: elapsed };

            surface
                .draw(
                    &query.vertices,
                    TRIANGLES,
                    &self.program_cell,
                    &uniforms,
                    &self.draw_params,
                )
                .expect("draw cells");
        }

        for query in iter_img {
            let sampler = query
                .texture
                .sampled()
                .magnify_filter(uniforms::MagnifySamplerFilter::Nearest)
                .minify_filter(uniforms::MinifySamplerFilter::Nearest);
            let uniforms = uniform! { tex: sampler };

            surface
                .draw(
                    &query.vertices,
                    TRIANGLES,
                    &self.program_img,
                    &uniforms,
                    &self.draw_params,
                )
                .expect("draw image");
        }
    }
}

// 埋め込みフォント（include_bytes）のバイト列を、プロセス内で1つだけ持つキャッシュ。
// ビュー（端末ペイン・ステータスバー・サイドバー・ビューア・ランチャー）ごとに
// FontSet を組み直すが、埋め込みフォントの to_vec コピーは全ビューで1回に抑える。
// ディスク上のフォント（NotoCJK 等）は Font::from_file で FreeType に直接ストリーム
// させ、こちらのヒープには載せない（必要なグリフだけ遅延読み）。
// 全ビューは main スレッドで作られるので thread_local で足りる。
thread_local! {
    static EMBEDDED_FONT_CACHE: std::cell::RefCell<std::collections::HashMap<PathBuf, Rc<Vec<u8>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn embedded_font_data(bytes: &'static [u8]) -> Rc<Vec<u8>> {
    EMBEDDED_FONT_CACHE.with(|cache| {
        // include_bytes のアドレスをキーにできないので、埋め込み用の擬似パスで引く。
        let key = PathBuf::from(format!("<embedded:{:p}>", bytes.as_ptr()));
        let mut cache = cache.borrow_mut();
        if let Some(data) = cache.get(&key) {
            return data.clone();
        }
        let data = Rc::new(bytes.to_vec());
        cache.insert(key, data.clone());
        data
    })
}

fn build_font_set(font_size: u32) -> FontSet {
    let config = &crate::TOYTERM_CONFIG;

    let mut fonts = FontSet::new(font_size);

    use std::iter::repeat;
    let regular_iter = repeat(FontStyle::Regular).zip(config.fonts_regular.iter());
    let bold_iter = repeat(FontStyle::Bold).zip(config.fonts_bold.iter());
    let faint_iter = repeat(FontStyle::Faint).zip(config.fonts_faint.iter());

    for (style, path) in regular_iter.chain(bold_iter).chain(faint_iter) {
        // FIXME
        if path.as_os_str().is_empty() {
            continue;
        }

        log::debug!("add {:?} font: {:?}", style, path.display());

        // TODO: add config for face index
        match Font::from_file(path, 0) {
            Ok(font) => fonts.add(style, font),
            Err(e) => log::warn!("ignore {:?} (reason: {})", path.display(), e),
        }
    }

    // Add embedded fonts
    {
        let regular_font = Font::from_memory(
            embedded_font_data(include_bytes!("fonts/Mplus1Code-Regular.ttf")),
            0,
        );
        fonts.add(FontStyle::Regular, regular_font);

        let bold_font = Font::from_memory(
            embedded_font_data(include_bytes!("fonts/Mplus1Code-SemiBold.ttf")),
            0,
        );
        fonts.add(FontStyle::Bold, bold_font);

        let faint_font = Font::from_memory(
            embedded_font_data(include_bytes!("fonts/Mplus1Code-Thin.ttf")),
            0,
        );
        fonts.add(FontStyle::Faint, faint_font);
    }

    // 最後の砦：OS に入っている日本語フォント。
    //
    // 内蔵の M PLUS 1 Code はコーディング用フォントで、全角記号の一部（？！～％＆＠＃）
    // を持っていない。設定フォントが Nerd Font だけだと、これらを持つフォントがどこにも
    // 無く空白になる（実機で「？が出ない」として報告された）。
    //
    // 内蔵フォントより後ろに足すのが重要。前に足すとセル幅の基準が日本語フォントに
    // なってしまい、罫線や PowerLine の幅がズレる。
    for (style, candidates) in [
        (FontStyle::Regular, SYSTEM_FALLBACK_REGULAR),
        (FontStyle::Bold, SYSTEM_FALLBACK_BOLD),
        (FontStyle::Faint, SYSTEM_FALLBACK_REGULAR),
    ] {
        for path in pick_existing(candidates, |p| p.is_file()) {
            match Font::from_file(&path, 0) {
                Ok(font) => {
                    log::debug!("OS のフォールバックフォント: {:?}", path.display());
                    fonts.add(style, font);
                }
                Err(e) => log::debug!("フォールバック候補を使えません {:?}: {}", path.display(), e),
            }
        }
    }

    fonts
}

/// OS の日本語フォントの候補（標準の位置）。存在するものだけを使う。
#[cfg(windows)]
const SYSTEM_FALLBACK_REGULAR: &[&str] = &[
    "C:/Windows/Fonts/YuGothM.ttc",
    "C:/Windows/Fonts/meiryo.ttc",
    "C:/Windows/Fonts/msgothic.ttc",
];
#[cfg(windows)]
const SYSTEM_FALLBACK_BOLD: &[&str] = &[
    "C:/Windows/Fonts/YuGothB.ttc",
    "C:/Windows/Fonts/meiryob.ttc",
    "C:/Windows/Fonts/msgothic.ttc",
];

#[cfg(not(windows))]
const SYSTEM_FALLBACK_REGULAR: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
];
#[cfg(not(windows))]
const SYSTEM_FALLBACK_BOLD: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/truetype/fonts-japanese-gothic.ttf",
];

/// 候補のうち、実在するものだけを順番どおりに返す。
fn pick_existing(candidates: &[&str], exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .filter(|p| exists(p))
        .collect()
}

fn calculate_cell_size(fonts: &FontSet) -> (CellSize, i32) {
    let mut max_advance_x: i32 = 0;
    let mut max_over: i32 = 0;
    let mut max_under: i32 = 0;

    let ascii_visible = ' '..='~';
    for ch in ascii_visible {
        for style in FontStyle::all() {
            let metrics = fonts.metrics(ch, style).expect("undefined glyph");

            let advance_x = (metrics.horiAdvance >> 6) as i32;
            max_advance_x = max(max_advance_x, advance_x);

            let over = (metrics.horiBearingY >> 6) as i32;
            max_over = max(max_over, over);

            let under = ((metrics.height - metrics.horiBearingY) >> 6) as i32;
            max_under = max(max_under, under);
        }
    }

    let cell_w = max_advance_x as u32;
    let cell_h = (max_over + max_under) as u32;

    log::debug!("cell size: {}x{} (px)", cell_w, cell_h);

    (
        CellSize {
            w: cell_w,
            h: cell_h,
        },
        max_over,
    )
}

fn color_to_rgba(color: Color) -> u32 {
    let config = &crate::TOYTERM_CONFIG;

    match color {
        Color::Rgb { rgba } => rgba,
        Color::Special => 0xFFFFFF00,

        Color::Black => config.color_black,
        Color::Red => config.color_red,
        Color::Green => config.color_green,
        Color::Yellow => config.color_yellow,
        Color::Blue => config.color_blue,
        Color::Magenta => config.color_magenta,
        Color::Cyan => config.color_cyan,
        Color::White => config.color_white,

        Color::BrightBlack => config.color_bright_black,
        Color::BrightRed => config.color_bright_red,
        Color::BrightGreen => config.color_bright_green,
        Color::BrightYellow => config.color_bright_yellow,
        Color::BrightBlue => config.color_bright_blue,
        Color::BrightMagenta => config.color_bright_magenta,
        Color::BrightCyan => config.color_bright_cyan,
        Color::BrightWhite => config.color_bright_white,

        Color::Foreground => config.color_foreground,
        Color::Background => config.color_background,
        Color::Selection => config.color_selection,
    }
}

/// ワークベンチのサイドバー/プレビュー用の背景色。
/// 背景色（Tokyo Night の紺）を**不透明**で塗る。半透明にすると壁紙が透けて
/// 色がくすみ、罫線・文字も読みにくくなるため。ターミナルはユーザ設定どおり
/// 透過のままなので、「パネルはソリッド／ターミナルは透過」で境界も締まる。
pub(crate) fn panel_bg_color() -> Color {
    let bg = crate::TOYTERM_CONFIG.color_background;
    Color::Rgb {
        rgba: (bg & 0xFFFF_FF00) | 0xFF,
    }
}

#[derive(Clone, Copy)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy)]
pub struct GlRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Copy)]
pub struct UvRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PixelRect {
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn to_gl(self, vp: Viewport) -> GlRect {
        GlRect {
            x: (self.x as f32 / vp.w as f32) * 2.0 - 1.0,
            y: -(self.y as f32 / vp.h as f32) * 2.0 + 1.0,
            w: (self.w as f32 / vp.w as f32) * 2.0,
            h: (self.h as f32 / vp.h as f32) * 2.0,
        }
    }

    pub fn to_uv(self, w: u32, h: u32) -> UvRect {
        UvRect {
            x: self.x as f32 / w as f32,
            y: self.y as f32 / h as f32,
            w: self.w as f32 / w as f32,
            h: self.h as f32 / h as f32,
        }
    }
}

#[derive(Copy, Clone)]
struct CellVertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
    color: [u32; 2],
    is_bg: u32,
    blinking: u32,
}
glium::implement_vertex!(CellVertex, position, tex_coords, color, is_bg, blinking);

/// Generate vertices for a single glyph image
fn glyph_vertices(
    gl_rect: GlRect,
    uv_rect: UvRect,
    fg_color: Color,
    bg_color: Color,
    blinking: u8,
) -> [CellVertex; 6] {
    // top-left, bottom-left, bottom-right, top-right
    let gl_ps = [
        [gl_rect.x, gl_rect.y],
        [gl_rect.x, gl_rect.y - gl_rect.h],
        [gl_rect.x + gl_rect.w, gl_rect.y - gl_rect.h],
        [gl_rect.x + gl_rect.w, gl_rect.y],
    ];
    let uv_ps = [
        [uv_rect.x, uv_rect.y],
        [uv_rect.x, uv_rect.y + uv_rect.h],
        [uv_rect.x + uv_rect.w, uv_rect.y + uv_rect.h],
        [uv_rect.x + uv_rect.w, uv_rect.y],
    ];

    let v = |idx| CellVertex {
        position: gl_ps[idx],
        tex_coords: uv_ps[idx],
        color: [color_to_rgba(bg_color), color_to_rgba(fg_color)],
        is_bg: 0,
        blinking: blinking as u32,
    };

    // 0    3
    // *----*
    // |\  B|
    // | \  |
    // |  \ |
    // |A  \|
    // *----*
    // 1    2

    [/* A */ v(0), v(1), v(2), /* B */ v(2), v(3), v(0)]
}

/// Generate vertices for a rectangle
fn rect_vertices(gl_rect: GlRect, fg_color: Color, bg_color: Color) -> [CellVertex; 6] {
    let GlRect { x, y, w, h } = gl_rect;

    // top-left, bottom-left, bottom-right, top-right
    let gl_ps = [[x, y], [x, y - h], [x + w, y - h], [x + w, y]];

    let v = |idx| CellVertex {
        position: gl_ps[idx],
        tex_coords: [0.0, 0.0],
        color: [color_to_rgba(bg_color), color_to_rgba(fg_color)],
        is_bg: 1,
        blinking: 0,
    };

    [v(0), v(1), v(2), v(2), v(3), v(0)]
}

#[derive(Clone, Copy)]
struct ImageVertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
}
glium::implement_vertex!(ImageVertex, position, tex_coords);

/// Generate vertices for a single sixel image
fn image_vertices(gl_rect: GlRect) -> [ImageVertex; 6] {
    let GlRect { x, y, w, h } = gl_rect;

    // top-left, bottom-left, bottom-right, top-right
    let gl_ps = [[x, y], [x, y - h], [x + w, y - h], [x + w, y]];
    let tx_ps = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

    let v = |idx| ImageVertex {
        position: gl_ps[idx],
        tex_coords: tx_ps[idx],
    };

    [v(0), v(1), v(2), v(2), v(3), v(0)]
}

#[cfg(test)]
mod tests {
    use super::{
        cell_rect, embedded_font_data, is_cell_filling_separator, physical_font_size,
        pick_existing, scale_change_requires_rebuild, CellSize, SYSTEM_FALLBACK_BOLD,
        SYSTEM_FALLBACK_REGULAR,
    };
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[test]
    fn only_powerline_separators_are_drawn_cell_filling() {
        // セル端まで塗る区切り記号
        for ch in ['\u{E0B0}', '\u{E0B1}', '\u{E0B2}', '\u{E0B3}', '\u{E0BF}'] {
            assert!(is_cell_filling_separator(ch), "{ch:?} は対象のはず");
        }
        // 通常のアイコン（ブランチ・鍵など）と、設計値どおりに描くべき文字
        for ch in [
            '\u{E0A0}', // branch
            '\u{E0AF}', // 私用領域だが区切りではない
            '\u{E0C0}', // 区切り範囲の直後
            '─', '│', '┼', // 罫線素片は伸縮すると線幅が崩れる
            '█', '▌', '▁', // ブロック要素は高さ・幅が設計値
            'A', '日',
        ] {
            assert!(!is_cell_filling_separator(ch), "{ch:?} は対象外のはず");
        }
    }

    #[test]
    fn system_fallback_uses_only_existing_paths_in_order() {
        let candidates = ["/a/first.ttc", "/b/missing.ttc", "/c/third.ttf"];
        let picked = pick_existing(&candidates, |p| p != Path::new("/b/missing.ttc"));
        assert_eq!(
            picked,
            vec![PathBuf::from("/a/first.ttc"), PathBuf::from("/c/third.ttf")],
            "実在するものだけを、書いた順で使う"
        );

        // 1つも無い環境では空。内蔵フォントだけで動く（従来と同じ）。
        assert!(pick_existing(&candidates, |_| false).is_empty());
    }

    #[test]
    fn system_fallback_candidates_are_absolute_and_nonempty() {
        // 相対パスだと exe の起動場所で結果が変わってしまう。
        for list in [SYSTEM_FALLBACK_REGULAR, SYSTEM_FALLBACK_BOLD] {
            assert!(!list.is_empty());
            for p in list {
                assert!(Path::new(p).is_absolute(), "{p} は絶対パスであるべき");
            }
        }
    }

    #[test]
    fn cell_rect_matches_the_cell_background_box() {
        let cell_size = CellSize { w: 13, h: 26 };
        // 3行目・左端が39px・全角(2セル)幅26px のセル
        let rect = cell_rect(3, 39, cell_size, 26);
        assert_eq!(rect.x, 39);
        assert_eq!(rect.y, 3 * 26);
        assert_eq!(rect.w, 26);
        assert_eq!(rect.h, 26, "高さはセル高さと一致し、はみ出さない");
    }

    #[test]
    fn physical_font_size_preserves_one_x_and_rounds_scaled_sizes() {
        assert_eq!(physical_font_size(18, 1.0), 18);
        assert_eq!(physical_font_size(18, 1.25), 23);
        assert_eq!(physical_font_size(18, 1.5), 27);
    }

    #[test]
    fn repeated_scale_factor_does_not_request_rebuild() {
        assert!(!scale_change_requires_rebuild(1.0, 1.0, 18));
        assert!(scale_change_requires_rebuild(1.0, 1.5, 18));
    }

    #[test]
    fn embedded_font_data_is_shared() {
        // 埋め込みフォントの to_vec コピーが1回だけになること（全ビューで同じ Rc）。
        static BYTES: &[u8] = b"embedded dummy";
        let a = embedded_font_data(BYTES);
        let b = embedded_font_data(BYTES);
        assert!(Rc::ptr_eq(&a, &b));
        assert_eq!(a.as_slice(), BYTES);
    }
}
