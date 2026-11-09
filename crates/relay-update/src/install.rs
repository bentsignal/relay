//! Where Relay is installed, which version is active, and switching between
//! versions.
//!
//! Windows: `%LOCALAPPDATA%\Relay\app-<version>\` holds each version and
//! `%LOCALAPPDATA%\Relay\current` is a directory junction to the active one.
//! Shortcuts and the login item point through `current`.
//!
//! macOS: the active app bundle stays where the user put it (normally
//! /Applications/Relay.app). Other versions live in
//! `~/Library/Application Support/Relay/versions/<version>/Relay.app` and are
//! swapped in by renaming.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    if let Ok(r) = std::env::var("RELAY_INSTALL_ROOT") {
        return PathBuf::from(r);
    }
    let base = directories::BaseDirs::new()
        .map(|b| if cfg!(windows) { b.data_local_dir().to_path_buf() } else { b.data_dir().to_path_buf() });
    base.unwrap_or_else(std::env::temp_dir).join("Relay")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub active: Option<String>,
    pub previous: Option<String>,
    /// Set while a freshly applied version hasn't proven itself yet.
    pub pending: Option<String>,
    pub rolled_back_from: Option<String>,
    /// macOS: where the active bundle lives.
    pub app_path: Option<PathBuf>,
}

fn state_path() -> PathBuf {
    root().join("update.json")
}

pub fn load_state() -> State {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_state(s: &State) -> Result<()> {
    std::fs::create_dir_all(root())?;
    let tmp = state_path().with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(tmp, state_path())?;
    Ok(())
}

/// True for builds run straight out of a cargo `target` directory.
pub fn is_dev_build() -> bool {
    std::env::current_exe().map(|p| p.components().any(|c| c.as_os_str() == "target")).unwrap_or(true)
}

/// Where versions are staged.
pub fn version_dir(version: &str) -> PathBuf {
    if cfg!(windows) { root().join(format!("app-{version}")) } else { root().join("versions").join(version) }
}

#[cfg(windows)]
pub fn current_link() -> PathBuf {
    root().join("current")
}

/// True when this process runs from the managed install.
pub fn running_installed() -> bool {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else { return false };
        let root = root();
        exe.starts_with(&root)
            || std::fs::canonicalize(&exe)
                .ok()
                .zip(std::fs::canonicalize(&root).ok())
                .map(|(e, r)| e.starts_with(r))
                .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        app_bundle().is_some() && !is_dev_build()
    }
}

/// macOS: the .app bundle containing this executable.
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find(|p| p.extension().map(|e| e == "app").unwrap_or(false)).map(Path::to_path_buf)
}

#[cfg(windows)]
fn make_junction(link: &Path, target: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_dir(link).with_context(|| format!("remove {}", link.display()))?;
    }
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(0x0800_0000)
        .output()?;
    if !out.status.success() {
        bail!("mklink failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

/// Makes `version` (already staged) the active one. The caller restarts.
pub fn activate(version: &str) -> Result<()> {
    let mut st = load_state();
    let dir = version_dir(version);
    if !dir.exists() {
        bail!("version {version} isn't staged");
    }
    #[cfg(windows)]
    make_junction(&current_link(), &dir)?;
    #[cfg(target_os = "macos")]
    {
        let bundle = st.app_path.clone().or_else(app_bundle).context("can't find Relay.app")?;
        let staged = dir.join("Relay.app");
        if let Some(active) = &st.active {
            let keep = version_dir(active);
            std::fs::create_dir_all(&keep)?;
            let _ = std::fs::remove_dir_all(keep.join("Relay.app"));
            std::fs::rename(&bundle, keep.join("Relay.app")).context("move current app aside")?;
        } else {
            let _ = std::fs::remove_dir_all(&bundle);
        }
        std::fs::rename(&staged, &bundle).context("move new app into place")?;
        st.app_path = Some(bundle);
    }
    if st.active.as_deref() != Some(version) {
        st.previous = st.active.take();
    }
    st.active = Some(version.to_string());
    st.pending = Some(version.to_string());
    st.rolled_back_from = None;
    save_state(&st)
}

/// Marks the pending version healthy.
pub fn confirm() -> Result<()> {
    let mut st = load_state();
    if st.pending.take().is_some() {
        save_state(&st)?;
    }
    Ok(())
}

/// Switches back to the previous version. Returns the version rolled back to.
pub fn rollback() -> Result<Option<String>> {
    let mut st = load_state();
    let Some(prev) = st.previous.clone() else { return Ok(None) };
    let failed = st.active.clone();
    #[cfg(windows)]
    make_junction(&current_link(), &version_dir(&prev))?;
    #[cfg(target_os = "macos")]
    {
        let bundle = st.app_path.clone().context("unknown app location")?;
        if let Some(bad) = &failed {
            let bad_dir = version_dir(bad);
            std::fs::create_dir_all(&bad_dir)?;
            let _ = std::fs::remove_dir_all(bad_dir.join("Relay.app"));
            std::fs::rename(&bundle, bad_dir.join("Relay.app"))?;
        }
        std::fs::rename(version_dir(&prev).join("Relay.app"), &bundle)?;
    }
    st.active = Some(prev.clone());
    st.previous = None;
    st.pending = None;
    st.rolled_back_from = failed;
    save_state(&st)?;
    Ok(Some(prev))
}

/// Path of an executable in the active version.
pub fn active_exe(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let p = current_link().join(format!("{name}.exe"));
        p.exists().then_some(p)
    }
    #[cfg(target_os = "macos")]
    {
        let st = load_state();
        let bundle = st.app_path.or_else(app_bundle)?;
        let p = bundle.join("Contents").join("MacOS").join(name);
        p.exists().then_some(p)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = name;
        None
    }
}

/// Removes staged versions other than the active and previous ones.
pub fn prune() {
    let st = load_state();
    let keep: Vec<PathBuf> = [st.active.as_deref(), st.previous.as_deref(), st.pending.as_deref()]
        .into_iter()
        .flatten()
        .map(version_dir)
        .collect();
    let dir = if cfg!(windows) { root() } else { root().join("versions") };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let is_version = if cfg!(windows) { name.starts_with("app-") } else { p.is_dir() };
        if is_version && !keep.contains(&p) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// Test installs (RELAY_INSTALL_ROOT set) leave login items and shortcuts alone.
fn sandboxed() -> bool {
    std::env::var_os("RELAY_INSTALL_ROOT").is_some()
}

/// Starts the engine at login.
pub fn set_autostart(on: bool) -> Result<()> {
    if sandboxed() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut cmd = std::process::Command::new("reg");
        if on {
            let exe = current_link().join("relay-engine.exe");
            cmd.args(["add", key, "/v", "Relay", "/t", "REG_SZ", "/f", "/d"]).arg(format!("\"{}\"", exe.display()));
        } else {
            cmd.args(["delete", key, "/v", "Relay", "/f"]);
        }
        cmd.creation_flags(0x0800_0000).output()?;
    }
    #[cfg(target_os = "macos")]
    {
        let agents = directories::BaseDirs::new().context("home")?.home_dir().join("Library/LaunchAgents");
        let plist = agents.join("dev.relay.engine.plist");
        if on {
            let exe = active_exe("relay-engine").context("engine not found")?;
            std::fs::create_dir_all(&agents)?;
            std::fs::write(
                &plist,
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>dev.relay.engine</string>
<key>ProgramArguments</key><array><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#,
                    exe.display()
                ),
            )?;
        } else {
            let _ = std::fs::remove_file(&plist);
        }
    }
    let _ = on;
    Ok(())
}

/// Windows: Start menu shortcut to the app.
pub fn create_shortcut() -> Result<()> {
    if sandboxed() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let programs = directories::BaseDirs::new()
            .context("dirs")?
            .data_dir()
            .join(r"Microsoft\Windows\Start Menu\Programs\Relay.lnk");
        let target = current_link().join("relay.exe");
        let ps = format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.Save()",
            programs.display(),
            target.display(),
            current_link().display()
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .creation_flags(0x0800_0000)
            .output()?;
    }
    Ok(())
}

/// First run of a downloaded build: copy it into the managed location,
/// register login start and a shortcut. Returns the installed app executable.
pub fn first_install(version: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let src = exe.parent().context("exe dir")?.to_path_buf();
    #[cfg(windows)]
    {
        let dest = version_dir(version);
        if dest.exists() {
            std::fs::remove_dir_all(&dest).ok();
        }
        copy_dir(&src, &dest)?;
        let mut st = load_state();
        st.previous = None;
        save_state(&st)?;
        activate(version)?;
        confirm()?;
        set_autostart(true)?;
        let _ = create_shortcut();
        Ok(current_link().join("relay.exe"))
    }
    #[cfg(not(windows))]
    {
        let mut st = load_state();
        st.active = Some(version.to_string());
        st.app_path = app_bundle();
        save_state(&st)?;
        set_autostart(true)?;
        let _ = src;
        Ok(exe)
    }
}

#[cfg(windows)]
fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}
