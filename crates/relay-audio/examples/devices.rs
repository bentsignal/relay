fn main() {
    let d = relay_audio::list_devices();
    for i in &d.inputs {
        println!(
            "IN  {} | {} | {} Hz {} ch{}",
            i.name,
            i.key,
            i.sample_rate,
            i.channels,
            if i.is_default { " (default)" } else { "" }
        );
    }
    for o in &d.outputs {
        println!(
            "OUT {} | {} | {} Hz {} ch{}",
            o.name,
            o.key,
            o.sample_rate,
            o.channels,
            if o.is_default { " (default)" } else { "" }
        );
    }
    #[cfg(windows)]
    println!(
        "default render endpoint: {:?}",
        relay_audio::windows::default_endpoint(false)
    );
}
