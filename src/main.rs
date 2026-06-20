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

    #[cfg(not(feature = "multiplex"))]
    let mut term = gototerm::window::TerminalWindow::new(window, display, None);

    #[cfg(feature = "multiplex")]
    let mut term = gototerm::multiplexer::Multiplexer::new(window, display);

    event_loop
        .run(move |event, elwt| {
            term.on_event(&event, elwt);
        })
        .expect("run");
}

/// 透過（半透明背景）と vsync を有効にしてウィンドウと glium Display を作る。
/// glium の SimpleWindowBuilder はどちらも無効なため、glutin を手書きする。
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
        .with_transparent(true);
    #[cfg(target_os = "linux")]
    let window_builder = window_builder.with_name("gototerm", "gototerm");

    let template = glutin::config::ConfigTemplateBuilder::new()
        .with_alpha_size(8)
        .with_transparency(true);

    let (window, gl_config) = glutin_winit::DisplayBuilder::new()
        .with_window_builder(Some(window_builder))
        .build(event_loop, template, |configs| {
            // 透過に対応する config を優先して選ぶ。
            configs
                .reduce(|acc, c| {
                    let better = c.supports_transparency().unwrap_or(false)
                        && !acc.supports_transparency().unwrap_or(false);
                    if better {
                        c
                    } else {
                        acc
                    }
                })
                .unwrap()
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

    // vsync（垂直同期）。これで描画がリフレッシュレートに同期し、
    // ティアリングが消え、Poll でも CPU が無駄に回らない。
    let _ = surface.set_swap_interval(
        &context,
        glutin::surface::SwapInterval::Wait(NonZeroU32::new(1).unwrap()),
    );

    let display = glium::Display::from_context_surface(context, surface).unwrap();
    (window, display)
}
