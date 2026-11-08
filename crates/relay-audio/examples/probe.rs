//! Manual test: plays a 1 kHz tone to the output device whose name contains
//! ARG and measures what arrives from the input device whose name contains ARG.

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let needle = std::env::args().nth(1).unwrap_or_else(|| "Relay".into());
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let devices = relay_audio::list_devices();
    let out = devices.outputs.iter().find(|d| d.name.contains(&needle));
    let inp = devices.inputs.iter().find(|d| d.name.contains(&needle));
    println!("output: {:?}\ninput: {:?}", out.map(|d| &d.name), inp.map(|d| &d.name));
    let mut play = out.map(|d| relay_audio::open_output(&d.key)).transpose()?;
    let mut rec = inp.map(|d| relay_audio::open_input(&d.key)).transpose()?;
    let start = Instant::now();
    let mut phase = 0f32;
    let mut written = 0usize;
    let mut buf = vec![0f32; 960];
    let mut l = vec![0f32; 480];
    let mut r = vec![0f32; 480];
    let mut peak = 0f32;
    let mut crossings = 0usize;
    let mut samples = 0usize;
    let mut prev = 0f32;
    let mut last = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if let Some((s, w)) = play.as_mut() {
            let due = (start.elapsed().as_secs_f64() * s.rate as f64) as usize + s.rate as usize / 50;
            while written + 480 <= due {
                for f in 0..480 {
                    let v = (phase * std::f32::consts::TAU).sin() * 0.5;
                    phase = (phase + 1000.0 / s.rate as f32).fract();
                    buf[f * 2] = v;
                    buf[f * 2 + 1] = v;
                }
                w.write_interleaved(&buf);
                written += 480;
            }
        }
        if let Some((_, rd)) = rec.as_mut() {
            while rd.fill_frames() >= 480 {
                rd.read_planar(&mut l, &mut r);
                for &v in &l {
                    peak = peak.max(v.abs());
                    if prev <= 0.0 && v > 0.0 {
                        crossings += 1;
                    }
                    prev = v;
                    samples += 1;
                }
            }
        }
        if last.elapsed() >= Duration::from_secs(1) {
            let hz = if samples > 0 { crossings as f64 * 48000.0 / samples as f64 } else { 0.0 };
            let (pu, po) = play.as_ref().map(|(s, _)| (s.pipe.underruns(), s.pipe.overruns())).unwrap_or_default();
            let (ru, ro) = rec.as_ref().map(|(s, _)| (s.pipe.underruns(), s.pipe.overruns())).unwrap_or_default();
            println!(
                "mic peak {peak:.3} ~{hz:.0} Hz | play underruns {pu} overruns {po} | rec underruns {ru} overruns {ro}"
            );
            peak = 0.0;
            crossings = 0;
            samples = 0;
            last = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
