//! Captures the screen and shows it in a viewer window (no network).
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info,relay_video=debug").init();
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    let viewer = Arc::new(relay_video::viewer::open("Relay test viewer")?);
    let v2 = viewer.clone();
    let src = relay_video::sources().first().map(|s| s.key.clone()).expect("no screen");
    let settings = relay_video::Settings { max_width: 1280, max_height: 720, ..Default::default() };
    let cap = relay_video::start(
        &src,
        settings,
        Box::new(move |f| {
            v2.push(f);
        }),
    )?;
    for _ in 0..secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if viewer.take_keyframe_request() {
            cap.request_keyframe();
        }
        let s = cap.stats();
        println!("{}x{} {:.1} fps {:.0} kbps open={}", s.width, s.height, s.fps, s.kbps, viewer.is_open());
    }
    Ok(())
}
