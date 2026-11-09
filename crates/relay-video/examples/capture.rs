//! Captures the first screen for N seconds and writes raw H.264 to a file.
use std::io::Write;
use std::sync::{Arc, Mutex};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info,relay_video=debug").init();
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let out = std::env::args().nth(2).unwrap_or_else(|| "capture.h264".into());
    println!("sources: {:?}", relay_video::sources());
    let src = relay_video::sources().first().map(|s| s.key.clone()).expect("no screen");
    let file = Arc::new(Mutex::new(std::fs::File::create(&out)?));
    let counts = Arc::new(Mutex::new((0usize, 0usize, 0usize)));
    let (f2, c2) = (file.clone(), counts.clone());
    let cap = relay_video::start(
        &src,
        relay_video::Settings::default(),
        Box::new(move |frame| {
            f2.lock().unwrap().write_all(&frame.data).unwrap();
            let mut c = c2.lock().unwrap();
            c.0 += 1;
            c.1 += frame.key as usize;
            c.2 += frame.data.len();
        }),
    )?;
    for _ in 0..secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let s = cap.stats();
        println!("{}x{} {:.1} fps {:.0} kbps", s.width, s.height, s.fps, s.kbps);
    }
    drop(cap);
    let c = counts.lock().unwrap();
    println!("frames {} keyframes {} bytes {}", c.0, c.1, c.2);
    Ok(())
}
