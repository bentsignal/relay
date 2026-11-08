#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod engine_proc;
mod fx;
mod theme;
mod widgets;

use gpui::{App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size};

pub const VERSION: &str = match option_env!("RELAY_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    engine_proc::ensure_running();
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1120.0), px(700.0)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Relay".into()),
                appears_transparent: cfg!(target_os = "macos"),
                traffic_light_position: Some(point(px(14.0), px(14.0))),
            }),
            app_id: Some("relay".into()),
            window_min_size: Some(size(px(720.0), px(460.0))),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| cx.new(|cx| app::RelayApp::new(window, cx))).expect("open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
