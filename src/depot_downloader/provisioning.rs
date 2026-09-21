use std::path::{Path, PathBuf};

use crate::config::Config;

const RELEASE_BASE_URL: &str =
    "https://github.com/SteamRE/DepotDownloader/releases/latest/download";

/// Returns the path to a working DepotDownloader executable, downloading and
/// extracting the current GitHub release for this platform the first time.
///
/// Runs blocking I/O (network + zip extraction) and should be called from a
/// background thread, e.g. via `cx.background_spawn`.
pub fn ensure_binary(config: &mut Config) -> anyhow::Result<PathBuf> {
    if let Some(path) = &config.depot_downloader_binary
        && path.is_file()
    {
        return Ok(path.clone());
    }

    let install_dir = install_dir()?;
    std::fs::create_dir_all(&install_dir)?;

    let archive_path = install_dir.join(asset_file_name());
    download_release_asset(&archive_path)?;
    let binary_path = extract_binary(&archive_path, &install_dir)?;
    make_executable(&binary_path)?;
    let _ = std::fs::remove_file(&archive_path);

    config.depot_downloader_binary = Some(binary_path.clone());
    config.save();
    Ok(binary_path)
}

fn install_dir() -> anyhow::Result<PathBuf> {
    let dirs = crate::config::project_dirs()
        .ok_or_else(|| anyhow::anyhow!("could not determine a data directory for this platform"))?;
    Ok(dirs.data_dir().join("depot-downloader"))
}

fn asset_file_name() -> String {
    format!(
        "DepotDownloader-{}-{}.zip",
        target_os_name(),
        target_arch_name()
    )
}

fn target_os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn target_arch_name() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "x64"
    }
}

fn binary_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "DepotDownloader.exe"
    } else {
        "DepotDownloader"
    }
}

fn download_release_asset(destination: &Path) -> anyhow::Result<()> {
    let url = format!("{RELEASE_BASE_URL}/{}", asset_file_name());
    let mut response = ureq::get(&url).call()?;
    let mut file = std::fs::File::create(destination)?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut file)?;
    Ok(())
}

fn extract_binary(archive_path: &Path, destination_dir: &Path) -> anyhow::Result<PathBuf> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let binary_name = binary_file_name();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(entry_path) = entry.enclosed_name() else {
            continue;
        };
        if entry_path.file_name().and_then(|name| name.to_str()) != Some(binary_name) {
            continue;
        }
        let binary_path = destination_dir.join(binary_name);
        let mut out_file = std::fs::File::create(&binary_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        return Ok(binary_path);
    }

    anyhow::bail!("{binary_name} was not found inside the downloaded DepotDownloader archive")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
