use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Settings persisted between launches: what the user typed last time, and
/// where the auto-provisioned DepotDownloader binary ended up so we do not
/// download it again on every start.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub last_app_id: String,
    pub last_download_location: Option<PathBuf>,
    pub depot_downloader_binary: Option<PathBuf>,
    /// The Steam account name of a remembered login (see
    /// `depot_downloader::process::LoginMethod::RememberedUsername`), or `None`
    /// if the user hasn't logged in yet or has logged out.
    pub logged_in_username: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        config_file_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_file_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(contents) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, contents);
        }
    }
}

pub fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("dev", "DepotDownloaderGpui", "DepotDownloader GUI")
}

fn config_file_path() -> Option<PathBuf> {
    project_dirs().map(|dirs| dirs.config_dir().join("config.json"))
}
