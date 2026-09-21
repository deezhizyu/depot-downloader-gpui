use std::time::Duration;

use serde_json::Value;

/// The lookup only chooses a folder name, so a slow API must not hold the
/// download hostage.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Looks up the folder name Steam itself would use under `steamapps/common`
/// (`installdir`) on api.steamcmd.net. DepotDownloader only reports it after
/// launch, but `-dir` has to be chosen before. Blocking; falls back to the app
/// id when the lookup fails.
pub fn fetch_install_dir_name(app_id: &str) -> String {
    request_app_info(app_id)
        .ok()
        .and_then(|response| extract_install_dir_name(&response["data"][app_id]))
        .unwrap_or_else(|| app_id.to_string())
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
}
