//! Captures a source (the first screen by default) for N seconds, writes the
//! raw H.264 (Annex B) to a file and checks the stream's structure.
//!
//!   cargo run -p relay-video --example capture -- [seconds] [out.h264] [--source KEY]

use relay_proto::EncodedVideo;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let source = args.iter().position(|a| a == "--source").map(|i| {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        v
    });
    let sources = relay_video::sources();
    println!("sources:");
    for s in &sources {
        println!("  {:<40} {:?}  {}", s.key, s.kind, s.name);
    }
    let key = source.or_else(|| sources.first().map(|s| s.key.clone()));
    let Some(key) = key else { anyhow::bail!("no video sources") };
    let secs: f64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let out_path = args.get(1).cloned().unwrap_or_else(|| "capture.h264".into());

    let frames: Arc<Mutex<Vec<(Instant, EncodedVideo)>>> = Arc::default();
    let f2 = frames.clone();
    println!("capturing {key} for {secs} s…");
    let cap = relay_video::start(
        &key,
        relay_video::Settings::default(),
        Box::new(move |f| f2.lock().unwrap().push((Instant::now(), f))),
    )?;
    let start = Instant::now();
    let mut asked = false;
    while start.elapsed() < Duration::from_secs_f64(secs) {
        std::thread::sleep(Duration::from_millis(250));
        if !asked && start.elapsed() > Duration::from_secs_f64(secs / 2.0) {
            cap.request_keyframe();
            asked = true;
        }
        let st = cap.stats();
        println!("  stats: {}x{} {:.1} fps {:.0} kbps hw={}", st.width, st.height, st.fps, st.kbps, st.encoder_hw);
    }
    let elapsed = start.elapsed().as_secs_f64();
    drop(cap);

    let frames = frames.lock().unwrap();
    let bytes: usize = frames.iter().map(|(_, f)| f.data.len()).sum();
    let keys = frames.iter().filter(|(_, f)| f.key).count();
    let annexb_ok = frames.iter().all(|(_, f)| f.data.starts_with(&[0, 0, 0, 1]));
    let keys_with_params = frames.iter().filter(|(_, f)| f.key).all(|(_, f)| {
        let nals = relay_video::h264::split_annexb(&f.data);
        let t: Vec<u8> = nals.iter().map(|n| relay_video::h264::nal_type(n)).collect();
        t.contains(&relay_video::h264::NAL_SPS)
            && t.contains(&relay_video::h264::NAL_PPS)
            && t.contains(&relay_video::h264::NAL_IDR)
    });
    let seq_ok = frames.windows(2).all(|w| w[1].1.seq == w[0].1.seq.wrapping_add(1));
    let pts_ok = frames.windows(2).all(|w| w[1].1.pts_us > w[0].1.pts_us);
    // Capture-to-output latency is not observable here; report the pts span instead.
    let span = frames.last().zip(frames.first()).map(|(l, f)| (l.1.pts_us - f.1.pts_us) as f64 / 1e6).unwrap_or(0.0);
    println!(
        "frames: {} in {:.2} s ({:.1} fps), {:.0} kbps",
        frames.len(),
        elapsed,
        frames.len() as f64 / elapsed,
        bytes as f64 * 8.0 / 1000.0 / elapsed
    );
    if let Some((_, f)) = frames.first() {
        println!("size: {}x{}, first frame key={} ({} bytes)", f.width, f.height, f.key, f.data.len());
    }
    println!("keyframes: {keys}; every keyframe has SPS+PPS+IDR: {keys_with_params}");
    println!(
        "annex B start codes: {annexb_ok}; seq contiguous: {seq_ok}; pts increasing: {pts_ok}; pts span {span:.2} s"
    );
    let mut all = Vec::with_capacity(bytes);
    for (_, f) in frames.iter() {
        all.extend_from_slice(&f.data);
    }
    std::fs::write(&out_path, all)?;
    println!("wrote {out_path}");
    Ok(())
}
