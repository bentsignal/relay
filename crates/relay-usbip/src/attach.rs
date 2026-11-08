//! Drives the signed usbip-win2 client to plug our loopback devices into the
//! Windows USB/IP virtual host controller.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Where the usbip-win2 client (usbip.exe + libusbip.dll + resources.dll) lives.
pub fn find_client() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("RELAY_USBIP_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("usbip"));
        candidates.push(dir.to_path_buf());
        // Development builds: target/<profile>/ -> repo/vendor/usbip
        for up in dir.ancestors().take(4) {
            candidates.push(up.join("vendor").join("usbip"));
        }
    }
    candidates.into_iter().map(|d| d.join("usbip.exe")).find(|p| p.is_file())
}

/// True if the usbip-win2 virtual host controller driver is installed and running.
pub fn driver_running() -> bool {
    let Ok(out) = quiet(Command::new("sc").args(["query", "usbip2_ude"])).output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).contains("RUNNING")
}

/// True if the driver service exists (possibly stopped).
pub fn driver_installed() -> bool {
    quiet(Command::new("sc").args(["query", "usbip2_ude"])).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Attaches one device; returns the hub port it was plugged into.
pub fn attach(client: &Path, tcp_port: u16, busid: &str) -> Result<u8> {
    let out = quiet(Command::new(client).args([
        "--tcp-port",
        &tcp_port.to_string(),
        "attach",
        "--remote",
        "127.0.0.1",
        "--bus-id",
        busid,
        "--once",
        "--terse",
    ]))
    .output()
    .context("run usbip attach")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("usbip attach failed: {} {}", stdout.trim(), stderr.trim());
    }
    stdout
        .split_whitespace()
        .filter_map(|t| t.parse::<u8>().ok())
        .next()
        .ok_or_else(|| anyhow!("usbip attach printed no port: {stdout}"))
}

pub fn detach(client: &Path, port: u8) -> Result<()> {
    let out = quiet(Command::new(client).args(["detach", "--port", &port.to_string()])).output()?;
    if !out.status.success() {
        bail!("usbip detach failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Ports currently imported from our server, parsed from `usbip port`.
pub fn our_ports(client: &Path, tcp_port: u16) -> Vec<u8> {
    let Ok(out) = quiet(Command::new(client).arg("port")).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!("127.0.0.1:{tcp_port}");
    let mut ports = Vec::new();
    let mut current: Option<u8> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Port ") {
            current = rest.split(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse().ok());
        }
        if l.contains(&needle)
            && let Some(p) = current
        {
            ports.push(p);
        }
    }
    ports
}
