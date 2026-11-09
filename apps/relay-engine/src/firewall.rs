//! Windows Firewall check: Relay needs inbound UDP for discovery and links.

use std::process::Command;

#[cfg(windows)]
fn quiet(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

/// True if our firewall rule is missing (Windows only).
pub fn blocked() -> bool {
    #[cfg(windows)]
    {
        let out =
            quiet(Command::new("netsh").args(["advfirewall", "firewall", "show", "rule", "name=Relay (LAN)"])).output();
        !matches!(out, Ok(o) if o.status.success())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Runs the elevated setup script (one admin prompt). Returns the child to wait on.
pub fn fix() -> anyhow::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let script = [dir.join("usbip").join("install-driver.ps1"), dir.join("install-driver.ps1")]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow::anyhow!("setup script missing"))?;
    Ok(Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!(
                "$p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"'; exit $p.ExitCode",
                script.display()
            ),
        ])
        .spawn()?)
}
