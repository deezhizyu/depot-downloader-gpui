use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::app_info::AppInfo;

/// True when `path` ends in `steamapps/common` (or `steamapps\common` on
/// Windows), case-insensitively - a real Steam library, as opposed to an
/// arbitrary folder DepotDownloader was pointed at. Only then does Steam
/// itself know to look here for an `appmanifest_*.acf`.
pub fn is_steam_library_common_dir(path: &Path) -> bool {
    let mut components = path.components().rev();
    let Some(common) = components.next() else {
        return false;
    };
    let Some(steamapps) = components.next() else {
        return false;
    };
    component_eq_ignore_ascii_case(common, "common")
        && component_eq_ignore_ascii_case(steamapps, "steamapps")
}

fn component_eq_ignore_ascii_case(component: std::path::Component, name: &str) -> bool {
    component
        .as_os_str()
        .to_str()
        .is_some_and(|value| value.eq_ignore_ascii_case(name))
}

/// Writes `appmanifest_{app_id}.acf` into `steamapps_dir` so Steam lists the
/// game as installed. `buildid` and each depot's manifest gid come from
/// `app_info`'s api.steamcmd.net lookup rather than from DepotDownloader
/// (upstream and our fork report neither): when the app wasn't found there,
/// this still writes a manifest Steam recognizes, just one that shows an
/// update available at next launch instead of an exact build match.
pub fn write_app_manifest(
    steamapps_dir: &Path,
    app_id: &str,
    app_info: &AppInfo,
    total_uncompressed_bytes: u64,
    depot_ids: &[u64],
) -> std::io::Result<()> {
    let contents = render_manifest(app_id, app_info, total_uncompressed_bytes, depot_ids);
    std::fs::write(
        steamapps_dir.join(format!("appmanifest_{app_id}.acf")),
        contents,
    )
}

fn render_manifest(
    app_id: &str,
    app_info: &AppInfo,
    total_uncompressed_bytes: u64,
    depot_ids: &[u64],
) -> String {
    let name = app_info.display_name.as_deref().unwrap_or(app_id);
    let buildid = app_info.buildid.as_deref().unwrap_or("0");
    let last_updated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let mut out = String::new();
    let _ = writeln!(out, "\"AppState\"");
    let _ = writeln!(out, "{{");
    write_field(&mut out, 1, "appid", app_id);
    write_field(&mut out, 1, "Universe", "1");
    write_field(&mut out, 1, "name", name);
    write_field(&mut out, 1, "StateFlags", "4");
    write_field(&mut out, 1, "installdir", &app_info.install_dir_name);
    write_field(&mut out, 1, "LastUpdated", &last_updated.to_string());
    write_field(
        &mut out,
        1,
        "SizeOnDisk",
        &total_uncompressed_bytes.to_string(),
    );
    write_field(&mut out, 1, "StagingSize", "0");
    write_field(&mut out, 1, "buildid", buildid);
    write_field(&mut out, 1, "LastOwner", "0");
    write_field(&mut out, 1, "BytesToDownload", "0");
    write_field(&mut out, 1, "BytesDownloaded", "0");
    write_field(&mut out, 1, "BytesStaged", "0");
    write_field(&mut out, 1, "TargetBuildID", buildid);
    write_field(&mut out, 1, "AutoUpdateBehavior", "0");
    write_field(&mut out, 1, "AllowOtherDownloadsWhileRunning", "0");
    write_field(&mut out, 1, "ScheduledAutoUpdate", "0");

    let _ = writeln!(out, "\t\"InstalledDepots\"");
    let _ = writeln!(out, "\t{{");
    for depot_id in depot_ids {
        let Some(manifest) = app_info.depot_manifests.get(depot_id) else {
            continue;
        };
        let _ = writeln!(out, "\t\t\"{depot_id}\"");
        let _ = writeln!(out, "\t\t{{");
        write_field(&mut out, 3, "manifest", &manifest.gid);
        write_field(&mut out, 3, "size", &manifest.size.to_string());
        let _ = writeln!(out, "\t\t}}");
    }
    let _ = writeln!(out, "\t}}");
    let _ = writeln!(out, "}}");
    out
}

fn write_field(out: &mut String, indent: usize, key: &str, value: &str) {
    let tabs = "\t".repeat(indent);
    let _ = writeln!(out, "{tabs}\"{key}\"\t\t\"{}\"", escape(value));
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::super::app_info::DepotManifest;

    #[test]
    fn recognizes_a_steamapps_common_folder_case_insensitively() {
        assert!(is_steam_library_common_dir(&PathBuf::from(
            r"D:\SteamLibrary\steamapps\common"
        )));
        assert!(is_steam_library_common_dir(&PathBuf::from(
            "/home/user/.steam/steam/STEAMAPPS/Common"
        )));
        assert!(!is_steam_library_common_dir(&PathBuf::from(
            r"D:\Games\MyGame"
        )));
        assert!(!is_steam_library_common_dir(&PathBuf::from(
            r"D:\SteamLibrary\steamapps\common\Squad"
        )));
    }

    #[test]
    fn renders_installed_depots_only_for_known_manifests() {
        let mut depot_manifests = HashMap::new();
        depot_manifests.insert(
            393_381,
            DepotManifest {
                gid: "123".to_string(),
                size: 456,
            },
        );
        let app_info = AppInfo {
            install_dir_name: "Squad".to_string(),
            display_name: Some("Squad".to_string()),
            buildid: Some("789".to_string()),
            depot_manifests,
        };
        let manifest = render_manifest("393380", &app_info, 1_000, &[393_381, 393_382]);
        assert!(manifest.contains("\"appid\"\t\t\"393380\""));
        assert!(manifest.contains("\"393381\""));
        assert!(!manifest.contains("\"393382\""));
        assert!(manifest.contains("\"manifest\"\t\t\"123\""));
    }
}
