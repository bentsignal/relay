//! Shows or sets the default devices (macOS).
//!
//!   default_device                 # prints "output <uid>" and "input <uid>"
//!   default_device output <uid>    # makes <uid> the default output

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use relay_audio::macos::{default_device_uid, set_default_device};
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            println!("output {}", default_device_uid(false).unwrap_or_default());
            println!("input {}", default_device_uid(true).unwrap_or_default());
        }
        [dir, uid] => set_default_device(uid, dir == "input")?,
        _ => anyhow::bail!("usage: default_device [output|input <uid>]"),
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
