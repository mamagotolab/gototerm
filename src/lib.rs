mod cache;
mod config;
mod font;
mod sixel;
mod terminal;
mod vt;
mod utils;
mod view;
pub mod window;

pub mod multiplexer;

/// glium 0.34 から Display は Surface 型のジェネリック引数が必須になった。
/// 全体で使う「ウィンドウ表面つき Display」を別名にまとめておく。
pub type Display = glium::Display<glutin::surface::WindowSurface>;

lazy_static::lazy_static! {
    pub static ref TOYTERM_CONFIG: crate::config::Config = crate::config::build();
}
