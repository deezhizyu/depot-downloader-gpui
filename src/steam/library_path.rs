use std::path::PathBuf;

/// The user's default Steam library's `steamapps/common` folder, if one of
/// the well-known per-OS install locations exists on disk.
pub fn default_steam_common_dir() -> Option<PathBuf> {
    candidate_dirs().into_iter().find(|path| path.is_dir())
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
