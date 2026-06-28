// Windows のリリースビルドではコンソール窓を出さない（GUI subsystem）。
// デバッグビルドはログを見られるようコンソールを残す。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    // Make sure that configuration errors are detected earlier
    lazy_static::initialize(&gototerm::TOYTERM_CONFIG);

    // Setup env_logger
    let our_logs = concat!(module_path!(), "=debug");
    let env = env_logger::Env::default().default_filter_or(our_logs);
    env_logger::Builder::from_env(env)
        .format_timestamp(None)
        .init();

    let event_loop = winit::event_loop::EventLoopBuilder::new()
        .build()
        .expect("event loop");

    // 実際の制御は on_event の AboutToWait で 16ms ごとの WaitUntil に
    // 切り替える（60fps ポーリング）。初期値は Wait にしておく。
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let (window, display) = build_window(&event_loop);

    let mut mux = gototerm::multiplexer::Multiplexer::new(window, display);

    event_loop
        .run(move |event, elwt| {
            mux.on_event(&event, elwt);
        })
        .expect("run");
}

/// 透過（半透明背景）と vsync を有効にしてウィンドウと glium Display を作る。
/// glium の SimpleWindowBuilder はどちらも無効なため、glutin を手書きする。
/// ウィンドウ（タイトルバー・タスクバー）のアイコン。exe 埋め込みの
/// フラスコアイコンと同じものを、生 RGBA から読み込んで設定する。
fn load_window_icon() -> Option<winit::window::Icon> {
    const RGBA: &[u8] = include_bytes!("../assets/icon128.rgba");
    winit::window::Icon::from_rgba(RGBA.to_vec(), 128, 128).ok()
}

fn build_window<T>(
    event_loop: &winit::event_loop::EventLoop<T>,
) -> (winit::window::Window, gototerm::Display) {
    use glutin::prelude::*;
    use glutin::display::GetGlDisplay;
    use raw_window_handle::HasRawWindowHandle;
    use std::num::NonZeroU32;

    // Wayland の app_id（＝コンポジタが見るウィンドウクラス）を設定する。
    // これが無いと Hyprland のウィンドウルール（ブラー・透過）で狙えない。
    #[cfg(target_os = "linux")]
    use winit::platform::wayland::WindowBuilderExtWayland as _;
    let window_builder = winit::window::WindowBuilder::new()
        .with_title("gototerm")
        .with_transparent(true)
        .with_window_icon(load_window_icon());
    #[cfg(target_os = "linux")]
    let window_builder = window_builder.with_name("gototerm", "gototerm");

    let template = glutin::config::ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .with_transparency(true);

    let (window, gl_config) = glutin_winit::DisplayBuilder::new()
        .with_window_builder(Some(window_builder))
        .build(event_loop, template, |configs| {
            // 標準の 8bit RGBA かつ透過対応の config を最優先で選ぶ。
            // （16bit float 等の特殊 config だと透過が正しく出ないことがある）
            let score = |cfg: &glutin::config::Config| -> i32 {
                let t = cfg.supports_transparency().unwrap_or(false);
                match (t, cfg.alpha_size()) {
                    (true, 8) => 3,
                    (true, _) => 2,
                    (false, 8) => 1,
                    _ => 0,
                }
            };
            let cfg = configs.reduce(|acc, c| if score(&c) > score(&acc) { c } else { acc }).unwrap();
            // 透過対応 config が無い環境(Windows など)では transparency=false に
            // なるが、不透明で正常に動くだけなので警告ではなく debug ログにする。
            log::debug!(
                "selected GL config: alpha_size={} transparency={:?}",
                cfg.alpha_size(),
                cfg.supports_transparency()
            );
            cfg
        })
        .expect("failed to build display");
    let window = window.unwrap();
    let raw_handle = window.raw_window_handle();

    let (w, h): (u32, u32) = window.inner_size().into();
    let attrs = glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
        .build(
            raw_handle,
            NonZeroU32::new(w.max(1)).unwrap(),
            NonZeroU32::new(h.max(1)).unwrap(),
        );
    let surface = unsafe {
        gl_config
            .display()
            .create_window_surface(&gl_config, &attrs)
            .unwrap()
    };

    let context_attrs = glutin::context::ContextAttributesBuilder::new().build(Some(raw_handle));
    let context = unsafe {
        gl_config
            .display()
            .create_context(&gl_config, &context_attrs)
            .expect("failed to create context")
    }
    .make_current(&surface)
    .unwrap();

    // Wayland(Linux) では swap interval を待たない（DontWait）。Wait(1)＝vsync だと、
    // ワークスペース切替などでコンポジタが frame callback を止めた瞬間に swap が
    // ブロックし、イベントループが固まって「応答なし（強制終了/待機）」ダイアログが
    // 出る。描画ペースは AboutToWait の WaitUntil(16ms) で既に約60fpsに保っている
    // ので、vsync の待ちは無くても CPU は回らず、固着だけが消える。
    // Windows ではこの問題は起きないため、ティアリングを避けて従来どおり vsync。
    #[cfg(not(windows))]
    let interval = glutin::surface::SwapInterval::DontWait;
    #[cfg(windows)]
    let interval = glutin::surface::SwapInterval::Wait(NonZeroU32::new(1).unwrap());
    let _ = surface.set_swap_interval(&context, interval);

    let display = glium::Display::from_context_surface(context, surface).unwrap();
    (window, display)
}
