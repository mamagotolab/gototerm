mod cache;
mod config;
mod font;
mod gt;
mod keybindings;
mod file_style;
mod launcher;
mod preview;
mod reader;
mod recent;
mod sidebar;
mod sixel;
mod terminal;
mod utils;
mod view;
mod vt;
mod watcher;
pub mod window;
mod workspace;

pub mod multiplexer;

/// glium 0.34 から Display は Surface 型のジェネリック引数が必須になった。
/// 全体で使う「ウィンドウ表面つき Display」を別名にまとめておく。
pub type Display = glium::Display<glutin::surface::WindowSurface>;

lazy_static::lazy_static! {
    pub static ref TOYTERM_CONFIG: crate::config::Config = crate::config::build();
}

pub fn initialize_keybindings() {
    lazy_static::initialize(&crate::keybindings::KEYBINDINGS);
}
