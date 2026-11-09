//! Captures from the input whose name contains ARG, optionally in RAW mode
//! (bypassing Windows audio effects), and prints the peak level each second.
//! cargo run -p relay-audio --example rawcap -- "Relay" raw

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::time::{Duration, Instant};
        let needle = std::env::args().nth(1).unwrap_or_else(|| "Relay".into());
        let raw = std::env::args().nth(2).as_deref() == Some("raw");
        let d = relay_audio::list_devices();
        let dev = d.inputs.iter().find(|x| x.name.contains(&needle)).expect("input not found");
        let id = relay_audio::windows::endpoint_id_from_key(&dev.key).to_string();
        let _ = wasapi::initialize_mta();
        let enumerator = wasapi::DeviceEnumerator::new().map_err(|e| anyhow::anyhow!("{e}"))?;
        let device = enumerator.get_device(&id).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut client = device.get_iaudioclient().map_err(|e| anyhow::anyhow!("{e}"))?;
        if raw {
            client
                .set_properties(wasapi::AudioClientProperties::new().set_option(wasapi::StreamOption::Raw))
                .map_err(|e| anyhow::anyhow!("raw: {e}"))?;
        }
        let format = wasapi::WaveFormat::new(32, 32, wasapi::SampleType::Float, 48000, 2, None);
        client
            .initialize_client(
                &format,
                wasapi::Direction::Capture,
                wasapi::StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 },
                None,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let ev = client.set_get_eventhandle().map_err(|e| anyhow::anyhow!("{e}"))?;
        let cap = client.get_audiocaptureclient().map_err(|e| anyhow::anyhow!("{e}"))?;
        client.start_stream().map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("{} raw={raw}", dev.name);
        let mut q = std::collections::VecDeque::new();
        let start = Instant::now();
        let mut last = Instant::now();
        let mut peak = 0f32;
        while start.elapsed() < Duration::from_secs(6) {
            let _ = ev.wait_for_event(200);
            cap.read_from_device_to_deque(&mut q).map_err(|e| anyhow::anyhow!("{e}"))?;
            while q.len() >= 4 {
                let b =
                    [q.pop_front().unwrap(), q.pop_front().unwrap(), q.pop_front().unwrap(), q.pop_front().unwrap()];
                peak = peak.max(f32::from_le_bytes(b).abs());
            }
            if last.elapsed() >= Duration::from_secs(1) {
                println!("peak {peak:.3}");
                peak = 0.0;
                last = Instant::now();
            }
        }
    }
    Ok(())
}
