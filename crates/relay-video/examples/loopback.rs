//! Captures a source and shows it in a local viewer window: the full
//! capture -> encode -> Annex B -> decode -> display path without a network.
//!
//!   cargo run -p relay-video --example loopback -- [seconds] [--source KEY]
//!
//! Close the window (or wait) to exit. Double-click toggles full screen.

#[cfg(any(windows, target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    tracing_subscriber::fmt().with_env_filter("info").init();
    // The viewer needs the main thread on macOS; everything else runs here.
    relay_video::run_app(|| -> anyhow::Result<()> {
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        let source = args.iter().position(|a| a == "--source").map(|i| {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            args.drain(i..(i + 2).min(args.len()));
            v
        });
        let key = source
            .or_else(|| relay_video::sources().first().map(|s| s.key.clone()))
            .ok_or_else(|| anyhow::anyhow!("no video sources"))?;
        let secs: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(8.0);
        let viewer = relay_video::viewer::open(&format!("Relay loopback · {key}"))?;
        let (tx, rx) = crossbeam_channel::bounded(8);
        let cap = relay_video::start(
            &key,
            relay_video::Settings::default(),
            Box::new(move |f| {
                let _ = tx.try_send(f);
            }),
        )?;
        let start = Instant::now();
        let mut last_print = Instant::now();
        let mut shown = 0u32;
        while start.elapsed() < Duration::from_secs_f64(secs) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(f) => {
                    if !viewer.push(f) {
                        break;
                    }
                    shown += 1;
                }
                Err(_) if !viewer.is_open() => break,
                Err(_) => {}
            }
            if viewer.take_keyframe_request() {
                cap.request_keyframe();
            }
            if last_print.elapsed() >= Duration::from_secs(1) {
                let st = cap.stats();
                println!("{}x{} {:.1} fps {:.0} kbps, {shown} frames shown", st.width, st.height, st.fps, st.kbps);
                last_print = Instant::now();
            }
        }
        println!("done after {:.1} s", start.elapsed().as_secs_f64());
        Ok(())
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("no video viewer on this platform");
}
