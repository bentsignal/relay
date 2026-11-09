fn main() {
    #[cfg(windows)]
    {
        let d = relay_audio::list_devices();
        for x in d.inputs.iter().chain(d.outputs.iter()) {
            let id = relay_audio::windows::endpoint_id_from_key(&x.key);
            println!("{:50} {:?}", x.name, relay_audio::windows::endpoint_volume(id));
        }
    }
}
