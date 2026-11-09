//! Finds and launches the engine next to the app binary.

use std::path::PathBuf;
use std::process::Command;

fn engine_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "relay-engine.exe" } else { "relay-engine" };
    let p = dir.join(name);
    p.is_file().then_some(p)
}

/// Starts the engine if it isn't running. Returns false if it couldn't be found.
pub fn ensure_running() -> bool {
    if relay_ipc::engine_running() {
        return true;
    }
    launch()
}

pub fn launch() -> bool {
    let Some(path) = engine_path() else {
        tracing::error!("relay-engine not found next to the app");
        return false;
    };
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().is_ok()
}

/// First run of a downloaded build: install it and return the installed app
/// to launch instead. Returns None when already running from the install (or
/// from a development build).
pub fn install_if_needed() -> Option<PathBuf> {
    use relay_update::install;
    if install::is_dev_build() || install::running_installed() {
        return None;
    }
    if cfg!(target_os = "macos") {
        // The bundle stays where the user put it; just register login start once.
        if install::load_state().active.is_none() {
            let _ = install::first_install(crate::VERSION);
        }
        return None;
    }
    let st = install::load_state();
    if st.active.as_deref() == Some(crate::VERSION)
        && let Some(exe) = install::active_exe("relay")
    {
        return Some(exe);
    }
    stop_engine();
    match install::first_install(crate::VERSION) {
        Ok(exe) => Some(exe),
        Err(e) => {
            tracing::error!("install failed: {e:#}");
            None
        }
    }
}

/// Asks a running engine to quit and waits for it to go away.
pub fn stop_engine() {
    if let Ok((client, _rx)) = relay_ipc::Client::connect("relay-installer") {
        let _ = client.send(&relay_proto::ClientMsg::Command(relay_proto::Command::Quit));
    }
    let t = std::time::Instant::now();
    while relay_ipc::engine_running() && t.elapsed() < std::time::Duration::from_secs(8) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// After the engine updated itself, restart this UI from the new version.
pub fn relaunch_updated(engine_version: &str) -> bool {
    use relay_update::install;
    // Only when the active install matches the engine, so this can't loop.
    if !install::running_installed() || install::load_state().active.as_deref() != Some(engine_version) {
        return false;
    }
    match relay_update::install::active_exe("relay") {
        Some(exe) => Command::new(exe).spawn().is_ok(),
        None => false,
    }
}
