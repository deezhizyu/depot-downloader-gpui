use std::path::PathBuf;

/// The user's default Steam library's `steamapps/common` folder, if one of
/// the well-known per-OS install locations exists on disk.
pub fn default_steam_common_dir() -> Option<PathBuf> {
    candidate_dirs().into_iter().find(|path| path.is_dir())
}

/// Where a game should land by default: a subfolder named after its app id
/// inside the Steam library, mirroring how Steam itself keeps one folder per
/// game under `steamapps/common`. DepotDownloader writes directly into
/// whatever directory it is given, so without this it would mix a game's
/// files straight into the shared `common` folder.
pub fn default_download_dir_for_app(app_id: &str) -> Option<PathBuf> {
    default_steam_common_dir().map(|common| common.join(app_id))
}

#[cfg(target_os = "windows")]
fn candidate_dirs() -> Vec<PathBuf> {
    let program_files_x86 = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
    vec![
        program_files_x86
            .join("Steam")
            .join("steamapps")
            .join("common"),
    ]
}

#[cfg(target_os = "macos")]
fn candidate_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    vec![home.join("Library/Application Support/Steam/steamapps/common")]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidate_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".local/share/Steam/steamapps/common"),
        home.join(".steam/steam/steamapps/common"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps/common"),
    ]
}

#[cfg(unix)]
fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_dir_is_a_subfolder_of_the_common_dir() {
        if let Some(common) = default_steam_common_dir() {
            let app_dir = default_download_dir_for_app("440").unwrap();
            assert_eq!(app_dir, common.join("440"));
        }
    }
}
