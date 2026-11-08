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
