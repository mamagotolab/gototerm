use std::path::PathBuf;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub shell: Vec<String>,

    // paths to font files which FreeType supports (TTF, OTF, etc.)
    pub fonts_regular: Vec<PathBuf>,
    pub fonts_bold: Vec<PathBuf>,
    pub fonts_faint: Vec<PathBuf>,
    pub font_size: u32,

    // タブバー（複数タブのときだけ表示）の文字サイズ
    pub status_bar_font_size: u32,

    // RRGGBBAA
    pub color_background: u32,
    pub color_foreground: u32,
    pub color_selection: u32,
    pub color_black: u32,
    pub color_red: u32,
    pub color_green: u32,
    pub color_yellow: u32,
    pub color_blue: u32,
    pub color_magenta: u32,
    pub color_cyan: u32,
    pub color_white: u32,
    pub color_bright_black: u32,
    pub color_bright_red: u32,
    pub color_bright_green: u32,
    pub color_bright_yellow: u32,
    pub color_bright_blue: u32,
    pub color_bright_magenta: u32,
    pub color_bright_cyan: u32,
    pub color_bright_white: u32,

    pub scroll_bar_width: u32,
    pub scroll_bar_fg_color: u32,
    pub scroll_bar_bg_color: u32,

    pub east_asian_width_ambiguous: u8,

    // カーソルを点滅させるか。
    pub cursor_blink: bool,
    // バー/下線カーソルの太さ(px)。ブロックカーソルには影響しない。
    pub cursor_thickness: u32,
}

impl Default for Config {
    fn default() -> Self {
        #[cfg(windows)]
        let shell = vec![std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())];
        #[cfg(not(windows))]
        let shell = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())];

        Config {
            shell,

            east_asian_width_ambiguous: 0,

            cursor_blink: true,
            cursor_thickness: 8,

            // FIXME: due to a bug on "config-rs", empty Vecs cannot be serialized properly.
            // https://github.com/mehcode/config-rs/issues/114
            fonts_regular: vec![PathBuf::new()],
            fonts_bold: vec![PathBuf::new()],
            fonts_faint: vec![PathBuf::new()],
            font_size: 18,

            status_bar_font_size: 16,

            scroll_bar_width: 5,
            // 既定の配色は Tokyo Night（Night バリアント）。
            scroll_bar_fg_color: 0x414868FF,
            scroll_bar_bg_color: 0x1A1B26FF,

            color_background: 0x1A1B26FF,
            color_foreground: 0xC0CAF5FF,
            color_selection: 0x283457FF,
            color_black: 0x15161EFF,
            color_red: 0xF7768EFF,
            color_green: 0x9ECE6AFF,
            color_yellow: 0xE0AF68FF,
            color_blue: 0x7AA2F7FF,
            color_magenta: 0xBB9AF7FF,
            color_cyan: 0x7DCFFFFF,
            color_white: 0xA9B1D6FF,

            color_bright_black: 0x414868FF,
            color_bright_red: 0xF7768EFF,
            color_bright_green: 0x9ECE6AFF,
            color_bright_yellow: 0xE0AF68FF,
            color_bright_blue: 0x7AA2F7FF,
            color_bright_magenta: 0xBB9AF7FF,
            color_bright_cyan: 0x7DCFFFFF,
            color_bright_white: 0xC0CAF5FF,
        }
    }
}

pub fn build() -> Config {
    let mut builder = ::config::Config::builder();

    // default config
    let default_config = Config::default();
    let default_source = ::config::Config::try_from(&default_config).unwrap();
    builder = builder.add_source(default_source);

    // user config
    if let Some(config_path) = find_config_file() {
        builder = builder.add_source(config::File::from(config_path).required(false));
    }

    builder
        .build()
        .unwrap()
        .try_deserialize()
        .expect("Failed to build config")
}

fn find_config_file() -> Option<PathBuf> {
    // Windows: %APPDATA%\gototerm\config.toml
    #[cfg(windows)]
    let mut base = PathBuf::from(std::env::var_os("APPDATA")?);

    // Unix: $XDG_CONFIG_HOME か $HOME/.config
    #[cfg(not(windows))]
    let mut base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME")?;
            let mut p = PathBuf::from(home);
            p.push(".config");
            Some(p)
        })?;

    base.push("gototerm");
    base.push("config.toml");
    Some(base)
}
