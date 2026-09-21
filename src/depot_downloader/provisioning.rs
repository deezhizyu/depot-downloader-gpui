use std::path::{Path, PathBuf};

/// Our fork of SteamRE/DepotDownloader, which adds the `-json` output mode
/// this app's progress numbers come from.
const RELEASE_BASE_URL: &str =
    "https://github.com/deezhizyu/depot-downloader/releases/latest/download";

/// Returns the path to a working DepotDownloader executable, downloading and
/// extracting the current GitHub release for this platform the first time.
///
/// Runs blocking I/O (network + zip extraction) and should be called from a
/// background thread, e.g. via `cx.background_spawn`.
pub fn ensure_binary() -> anyhow::Result<PathBuf> {
    let install_dir = install_dir()?;
    let binary_path = install_dir.join(binary_file_name());
    if binary_path.is_file() {
        eprintln!(
            "[depot-downloader-gpui] using previously provisioned DepotDownloader at {}",
            binary_path.display()
        );
        return Ok(binary_path);
    }

    std::fs::create_dir_all(&install_dir)?;

    let archive_path = install_dir.join(asset_file_name());
    let url = format!("{RELEASE_BASE_URL}/{}", asset_file_name());
    eprintln!("[depot-downloader-gpui] downloading {url}");
    download_release_asset(&url, &archive_path)?;
    eprintln!(
        "[depot-downloader-gpui] extracting {}",
        archive_path.display()
    );
    extract_binary(&archive_path, &binary_path)?;
    make_executable(&binary_path)?;
    let _ = std::fs::remove_file(&archive_path);
    eprintln!(
        "[depot-downloader-gpui] DepotDownloader ready at {}",
        binary_path.display()
    );
    Ok(binary_path)
}

fn install_dir() -> anyhow::Result<PathBuf> {
    let dirs = crate::config::project_dirs()
        .ok_or_else(|| anyhow::anyhow!("could not determine a data directory for this platform"))?;
    Ok(dirs.data_dir().join("depot-downloader-fork"))
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

fn download_release_asset(url: &str, destination: &Path) -> anyhow::Result<()> {
    let mut response = ureq::get(url).call()?;
    let mut file = std::fs::File::create(destination)?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut file)?;
    Ok(())
}

fn extract_binary(archive_path: &Path, binary_path: &Path) -> anyhow::Result<()> {
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
        let mut out_file = std::fs::File::create(binary_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        return Ok(());
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
