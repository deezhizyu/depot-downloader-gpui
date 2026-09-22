use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

/// The lookup only chooses a folder name, so a slow API must not hold the
/// download hostage.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A depot's currently published manifest, from the `public` branch.
#[derive(Debug, Clone)]
pub struct DepotManifest {
    pub gid: String,
    pub size: u64,
}

/// Everything api.steamcmd.net can tell us about an app: the folder name
/// Steam itself would install it under, plus what a Steam-recognized
/// `appmanifest_*.acf` needs (display name, current build id, each depot's
/// published manifest). Fields stay `None`/empty when the lookup fails or the
/// app is missing them, since none of it is required to run DepotDownloader
/// itself.
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub install_dir_name: String,
    pub display_name: Option<String>,
    pub buildid: Option<String>,
    pub depot_manifests: HashMap<u64, DepotManifest>,
}

/// Looks up install dir name plus the data needed to write a Steam
/// `appmanifest_*.acf` after a finished download, all in the one blocking
/// call to api.steamcmd.net. Blocking; falls back to the app id as the
/// install dir name when the lookup or that one field fails.
pub fn fetch_app_info(app_id: &str) -> AppInfo {
    let app = request_app_info(app_id)
        .ok()
        .map(|response| response["data"][app_id].clone())
        .unwrap_or(Value::Null);
    AppInfo {
        install_dir_name: extract_install_dir_name(&app).unwrap_or_else(|| app_id.to_string()),
        display_name: app["common"]["name"].as_str().map(str::to_string),
        buildid: app["depots"]["branches"]["public"]["buildid"]
            .as_str()
            .map(str::to_string),
        depot_manifests: extract_depot_manifests(&app),
    }
}

fn request_app_info(app_id: &str) -> anyhow::Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let body = agent
        .get(format!("https://api.steamcmd.net/v1/info/{app_id}"))
        .call()?
        .body_mut()
        .read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Rejects names that could escape the library folder.
fn extract_install_dir_name(app: &Value) -> Option<String> {
    let name = app["config"]["installdir"].as_str()?.trim();
    let is_single_path_component =
        !name.is_empty() && name != ".." && name != "." && !name.contains(['/', '\\', ':']);
    is_single_path_component.then(|| name.to_string())
}

fn extract_depot_manifests(app: &Value) -> HashMap<u64, DepotManifest> {
    let Value::Object(depots) = &app["depots"] else {
        return HashMap::new();
    };
    depots
        .iter()
        .filter_map(|(key, value)| {
            let depot_id: u64 = key.parse().ok()?;
            let public = &value["manifests"]["public"];
            let gid = public["gid"].as_str()?.to_string();
            let size = public["size"].as_str()?.parse().ok()?;
            Some((depot_id, DepotManifest { gid, size }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_a_safe_install_dir_name() {
        let app = json!({"config": {"installdir": "Team Fortress 2"}});
        assert_eq!(
            extract_install_dir_name(&app).as_deref(),
            Some("Team Fortress 2")
        );
    }

    #[test]
    fn rejects_missing_or_unsafe_install_dir_names() {
        assert_eq!(extract_install_dir_name(&json!({})), None);
        for unsafe_name in ["", "..", "a/b", "a\\b", "C:"] {
            let app = json!({"config": {"installdir": unsafe_name}});
            assert_eq!(extract_install_dir_name(&app), None);
        }
    }

    #[test]
    fn extracts_depot_manifests_and_skips_incomplete_entries() {
        let app = json!({
            "depots": {
                "228990": {"manifests": {"public": {"gid": "123", "size": "456"}}},
                "branches": {"public": {"buildid": "789"}},
                "no_manifest": {"config": {"oslist": "macos"}},
            }
        });
        let manifests = extract_depot_manifests(&app);
        assert_eq!(manifests.len(), 1);
        let manifest = &manifests[&228990];
        assert_eq!(manifest.gid, "123");
        assert_eq!(manifest.size, 456);
    }
}
