use std::path::PathBuf;

fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "Relay")
}

/// Where config and identity live.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RELAY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs().map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

pub fn log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RELAY_DATA_DIR") {
        return PathBuf::from(dir).join("logs");
    }
    dirs().map(|d| d.data_local_dir().join("logs")).unwrap_or_else(|| PathBuf::from("logs"))
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.json")
}
