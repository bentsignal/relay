fn main() {
    #[cfg(windows)]
    {
        let needle = std::env::args().nth(1).unwrap_or_else(|| "Relay".into());
        let d = relay_audio::list_devices();
        for x in d.inputs.iter().chain(d.outputs.iter()).filter(|x| x.name.contains(&needle)) {
            let id = relay_audio::windows::endpoint_id_from_key(&x.key);
            println!(
                "{} -> {:?} {:?}",
                x.name,
                relay_audio::windows::disable_enhancements(id),
                relay_audio::windows::clear_effects(id)
            );
        }
    }
}
