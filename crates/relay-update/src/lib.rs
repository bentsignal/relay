//! Relay's updater.
//!
//! Every build publishes a manifest per channel (`stable.json`, `dev.json`) to
//! a fixed GitHub release, signed with Relay's Ed25519 update key. The engine
//! downloads the build for its platform, verifies it, stages it next to the
//! current one, and switches over; a watchdog rolls back if the new version
//! doesn't come up healthy.

pub mod install;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const REPO: &str = "bentsignal/relay";

/// Public half of the update signing key. The private half lives only in the
/// repository's CI secrets.
pub const PUBLIC_KEY: &str = include_str!("update-key.pub");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub channel: String,
    pub version: String,
    pub commit: String,
    pub date: String,
    pub assets: HashMap<String, Asset>,
}

/// Name of this build's platform in manifests.
pub fn platform() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x64"
    } else {
        "unsupported"
    }
}

pub fn base_url() -> String {
    std::env::var("RELAY_UPDATE_BASE")
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/channels"))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .user_agent(concat!("relay-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut resp = agent().get(url).call().with_context(|| format!("GET {url}"))?;
    let mut buf = Vec::new();
    resp.body_mut().as_reader().take(limit).read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn verifying_key() -> Result<VerifyingKey> {
    let raw = base64::engine::general_purpose::STANDARD.decode(PUBLIC_KEY.trim())?;
    let bytes: [u8; 32] = raw.try_into().map_err(|_| anyhow!("bad public key"))?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

pub fn verify(data: &[u8], sig_b64: &str) -> Result<()> {
    let raw = base64::engine::general_purpose::STANDARD.decode(sig_b64.trim())?;
    let sig: [u8; 64] = raw.try_into().map_err(|_| anyhow!("bad signature length"))?;
    verifying_key()?.verify(data, &Signature::from_bytes(&sig)).map_err(|_| anyhow!("signature check failed"))
}

/// Signs with a base64 32-byte seed (used by the release tooling).
pub fn sign(seed_b64: &str, data: &[u8]) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD.decode(seed_b64.trim())?;
    let seed: [u8; 32] = raw.try_into().map_err(|_| anyhow!("bad seed"))?;
    let key = SigningKey::from_bytes(&seed);
    Ok(base64::engine::general_purpose::STANDARD.encode(key.sign(data).to_bytes()))
}

pub fn public_from_seed(seed_b64: &str) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD.decode(seed_b64.trim())?;
    let seed: [u8; 32] = raw.try_into().map_err(|_| anyhow!("bad seed"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()))
}

/// Fetches and verifies the manifest for `channel`.
pub fn fetch_manifest(channel: &str) -> Result<Manifest> {
    let base = base_url();
    let json = get_bytes(&format!("{base}/{channel}.json"), 1 << 20)?;
    let sig = get_bytes(&format!("{base}/{channel}.json.sig"), 4096)?;
    verify(&json, &String::from_utf8_lossy(&sig))?;
    let m: Manifest = serde_json::from_slice(&json)?;
    if m.channel != channel {
        bail!("manifest is for {}", m.channel);
    }
    Ok(m)
}

/// Downloads the asset to `dest`, verifying size and hash.
pub fn download(asset: &Asset, dest: &Path, mut progress: impl FnMut(f32)) -> Result<()> {
    let mut resp = agent().get(&asset.url).call().with_context(|| format!("GET {}", asset.url))?;
    let mut reader = resp.body_mut().with_config().limit(asset.size + 1).reader();
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n])?;
        done += n as u64;
        progress(done as f32 / asset.size.max(1) as f32);
    }
    drop(file);
    if done != asset.size {
        bail!("download incomplete ({done} of {} bytes)", asset.size);
    }
    let digest = hex::encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&tmp);
        bail!("download corrupted (hash mismatch)");
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Extracts a zip into `dir` (which must not exist yet).
pub fn extract(zip_path: &Path, dir: &Path) -> Result<()> {
    let tmp = dir.with_extension("extracting");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = tmp.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut f, &mut w)?;
        #[cfg(unix)]
        if let Some(mode) = f.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))?;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&tmp, dir)?;
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

pub fn downloads_dir() -> PathBuf {
    install::root().join("downloads")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let seed = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let sig = sign(&seed, b"hello").unwrap();
        let pk = public_from_seed(&seed).unwrap();
        let raw: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(pk).unwrap().try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&raw).unwrap();
        let sraw: [u8; 64] = base64::engine::general_purpose::STANDARD.decode(sig).unwrap().try_into().unwrap();
        assert!(vk.verify(b"hello", &Signature::from_bytes(&sraw)).is_ok());
        assert!(vk.verify(b"hellp", &Signature::from_bytes(&sraw)).is_err());
    }
}
