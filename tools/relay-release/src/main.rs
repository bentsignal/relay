//! relay-release manifest <channel> <version> <commit> <tag> <asset-dir> <out-dir>
//!   Writes <out-dir>/<channel>.json and .sig for the zips in <asset-dir>.
//!   The signing seed comes from the RELAY_UPDATE_KEY environment variable.
//! relay-release pubkey   Prints the public key for RELAY_UPDATE_KEY.

use anyhow::{Context, Result, bail};
use relay_update::{Asset, Manifest, REPO, public_from_seed, sha256_file, sign};
use std::collections::HashMap;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed = std::env::var("RELAY_UPDATE_KEY").context("RELAY_UPDATE_KEY not set")?;
    match args.first().map(String::as_str) {
        Some("pubkey") => println!("{}", public_from_seed(&seed)?),
        Some("manifest") if args.len() == 7 => {
            let (channel, version, commit, tag) = (&args[1], &args[2], &args[3], &args[4]);
            let assets_dir = PathBuf::from(&args[5]);
            let out = PathBuf::from(&args[6]);
            let mut assets = HashMap::new();
            for e in std::fs::read_dir(&assets_dir)? {
                let p = e?.path();
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let Some(platform) = name.strip_prefix("relay-").and_then(|n| n.strip_suffix(".zip")) else { continue };
                assets.insert(
                    platform.to_string(),
                    Asset {
                        url: format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
                        sha256: sha256_file(&p)?,
                        size: std::fs::metadata(&p)?.len(),
                    },
                );
            }
            if assets.is_empty() {
                bail!("no relay-*.zip assets in {}", assets_dir.display());
            }
            let m = Manifest {
                channel: channel.clone(),
                version: version.clone(),
                commit: commit.clone(),
                date: std::env::var("RELAY_BUILD_DATE").unwrap_or_default(),
                assets,
            };
            let json = serde_json::to_vec_pretty(&m)?;
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join(format!("{channel}.json")), &json)?;
            std::fs::write(out.join(format!("{channel}.json.sig")), sign(&seed, &json)?)?;
            println!("{}", String::from_utf8_lossy(&json));
        }
        _ => bail!("usage: relay-release manifest <channel> <version> <commit> <tag> <asset-dir> <out-dir> | pubkey"),
    }
    Ok(())
}
