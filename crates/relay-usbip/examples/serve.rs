//! Manual test: plugs in a "Relay Test" device, plays a tone into its
//! microphone and prints the level of whatever apps play to its speaker.

use relay_usbip::{DEFAULT_PORT, Identity, VirtualAudio};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    println!(
        "driver installed: {}, running: {}",
        relay_usbip::attach::driver_installed(),
        relay_usbip::attach::driver_running()
    );
    let (mut va, mut eps) = VirtualAudio::start(
        DEFAULT_PORT,
        vec![Identity {
            product: "Relay Test".into(),
            serial: "RELAY-TEST-1".into(),
            product_id: 0x0001,
        }],
    )?;
    println!("client found: {}", va.client_found());
    va.attach_all()?;
    let ep = &mut eps[0];
    let start = Instant::now();
    let mut phase = 0f32;
    let mut written = 0usize;
    let mut buf = vec![0f32; 960];
    let mut out_l = vec![0f32; 480];
    let mut out_r = vec![0f32; 480];
    let mut last_print = Instant::now();
    let mut peak = 0f32;
    while start.elapsed() < Duration::from_secs(secs) {
        // Produce mic audio at real time.
        let due = (start.elapsed().as_secs_f64() * 48000.0) as usize;
        while written + 480 <= due + 960 {
            for f in 0..480 {
                let s = (phase * std::f32::consts::TAU).sin() * 0.3;
                phase = (phase + 440.0 / 48000.0).fract();
                buf[f * 2] = s;
                buf[f * 2 + 1] = s;
            }
            ep.capture.write_interleaved(&buf);
            written += 480;
        }
        while ep.playback.fill_frames() >= 480 {
            ep.playback.read_planar(&mut out_l, &mut out_r);
            peak = out_l
                .iter()
                .chain(out_r.iter())
                .fold(peak, |m, s| m.max(s.abs()));
        }
        if last_print.elapsed() > Duration::from_secs(1) {
            let s = &ep.stats;
            use std::sync::atomic::Ordering::Relaxed;
            println!(
                "connected={} play={} cap={} out_pkts={} in_pkts={} playback_peak={:.3} mic_fill={}",
                s.connected.load(Relaxed),
                s.playback_active.load(Relaxed),
                s.capture_active.load(Relaxed),
                s.out_packets.load(Relaxed),
                s.in_packets.load(Relaxed),
                peak,
                ep.capture.free_frames(),
            );
            peak = 0.0;
            last_print = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    va.detach_all();
    Ok(())
}
