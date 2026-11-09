//! Measures round-trip latency: plays clicks to the output whose name contains
//! OUT, listens on the input whose name contains IN, and reports the delay
//! between writing a click and hearing it. Includes both apps' own buffers.
//!
//! cargo run -p relay-audio --example latency -- "Speakers (Relay)" "Microphone (Relay)" 10

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let out_name = std::env::args().nth(1).unwrap_or_else(|| "Speakers (Relay)".into());
    let in_name = std::env::args().nth(2).unwrap_or_else(|| "Microphone (Relay)".into());
    let secs: u64 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    let d = relay_audio::list_devices();
    let out = d.outputs.iter().find(|x| x.name.contains(&out_name)).expect("output not found");
    let inp = d.inputs.iter().find(|x| x.name.contains(&in_name)).expect("input not found");
    let (ostream, mut w) = relay_audio::open_output(&out.key)?;
    let (_istream, mut r) = relay_audio::open_input(&inp.key)?;
    r.shared().set_prime_frames(1);
    let rate = ostream.rate as f64;
    let start = Instant::now();
    let mut written = 0u64; // frames written to the output pipe
    let mut clicks: Vec<(u64, Instant)> = Vec::new();
    let mut results = Vec::new();
    let mut buf = vec![0f32; 960];
    let (mut l, mut rr) = (vec![0f32; 480], vec![0f32; 480]);
    let mut armed = true;
    let mut max_in = 0f32;
    let mut burst = 0usize;
    let mut last_hit = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        // Keep ~20 ms queued in our own output pipe.
        let target = (start.elapsed().as_secs_f64() * rate) as u64 + (rate * 0.02) as u64;
        while written < target {
            buf.fill(0.0);
            let block_start = written;
            // A click every 500 ms.
            let period = (rate * 0.5) as u64;
            if block_start / period != (block_start + 480) / period && block_start > (rate as u64) {
                let at = ((block_start / period + 1) * period - block_start) as usize;
                for i in at..480 {
                    let v = ((i - at) as f32 * std::f32::consts::TAU * 1000.0 / rate as f32).sin() * 0.8;
                    buf[i * 2] = v;
                    buf[i * 2 + 1] = v;
                }
                burst = 960;
                // When the click actually leaves our pipe: now + what's queued ahead of it.
                let queued = w.fill_frames() as f64 + at as f64;
                clicks.push((block_start, Instant::now() + Duration::from_secs_f64(queued / rate)));
            }
            if burst > 0 && clicks.last().map(|(b, _)| *b != block_start).unwrap_or(true) {
                for i in 0..480 {
                    let v = ((written + i as u64) as f32 * std::f32::consts::TAU * 1000.0 / rate as f32).sin() * 0.8;
                    buf[i * 2] = v;
                    buf[i * 2 + 1] = v;
                }
                burst = burst.saturating_sub(480);
            }
            w.write_interleaved(&buf);
            written += 480;
        }
        while r.fill_frames() >= 480 {
            r.read_planar(&mut l, &mut rr);
            let pk = l.iter().fold(0f32, |m, s| m.max(s.abs()));
            if pk > max_in {
                max_in = pk;
            }
            if let Some(i) = l.iter().position(|s| s.abs() > 0.3) {
                if armed && last_hit.elapsed() > Duration::from_millis(200) {
                    let heard = Instant::now() - Duration::from_secs_f64((r.fill_frames() + (480 - i)) as f64 / rate);
                    if let Some((_, sent)) = clicks.iter().rev().find(|(_, t)| *t <= heard) {
                        results.push((heard - *sent).as_secs_f64() * 1000.0);
                    }
                    last_hit = Instant::now();
                    armed = false;
                }
            } else {
                armed = true;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "clicks sent {} written frames {} out underruns {} in fill {}",
        clicks.len(),
        written,
        ostream.pipe.underruns(),
        r.fill_frames()
    );
    results.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!("max input level {max_in:.3}");
    if results.is_empty() {
        println!("no clicks detected");
    } else {
        let median = results[results.len() / 2];
        println!(
            "round trip over {} clicks: min {:.1} ms, median {:.1} ms, max {:.1} ms",
            results.len(),
            results[0],
            median,
            results[results.len() - 1]
        );
    }
    Ok(())
}
