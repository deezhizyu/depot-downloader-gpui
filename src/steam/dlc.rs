use std::time::Duration;

use serde_json::Value;

/// DLC discovery only decides whether to show a dropdown, so a slow response
/// must not hold up game selection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The DLC app ids for `app_id`, or empty on any failure - the fork has no
/// `-list-dlc`, so this is the one piece of catalog data that comes from
/// Steam's public store API rather than DepotDownloader. Blocking; call from
/// a background thread.
pub fn fetch_dlc_app_ids(app_id: u64) -> Vec<u64> {
    request_app_details(app_id)
        .map(|response| extract_dlc_app_ids(&response, app_id))
        .unwrap_or_default()
}

/// `app_id`'s store name - used to resolve a DLC's display name, since the
/// `dlc` field on the base app is only ids. `None` on any failure. Blocking;
/// call from a background thread.
pub fn fetch_app_name(app_id: u64) -> Option<String> {
    let response = request_app_details(app_id).ok()?;
    extract_name(&response, app_id)
}

fn request_app_details(app_id: u64) -> anyhow::Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    // "basic" is the filter group that includes both `name` and `dlc` - the
    // store API silently ignores an unrecognized filter name and returns an
    // empty `data`, which is why `filters=dlc` alone (a name, not a group)
    // does not work.
    let body = agent
        .get(format!(
            "https://store.steampowered.com/api/appdetails?appids={app_id}&filters=basic"
        ))
        .call()?
        .body_mut()
        .read_to_string()?;
    Ok(serde_json::from_str(&body)?)
}

fn is_successful(app: &Value) -> bool {
    app["success"].as_bool().unwrap_or(false)
}

fn extract_dlc_app_ids(response: &Value, app_id: u64) -> Vec<u64> {
    let app = &response[app_id.to_string()];
    if !is_successful(app) {
        return Vec::new();
    }
    app["data"]["dlc"]
        .as_array()
        .map(|dlc| dlc.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default()
}

fn extract_name(response: &Value, app_id: u64) -> Option<String> {
    let app = &response[app_id.to_string()];
    is_successful(app)
        .then(|| app["data"]["name"].as_str())
        .flatten()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_dlc_ids_from_a_successful_response() {
        let response = json!({
            "393380": {
                "success": true,
                "data": {"dlc": [393_381, 393_382]}
            }
        });
        assert_eq!(
            extract_dlc_app_ids(&response, 393_380),
            vec![393_381, 393_382]
        );
    }

    #[test]
    fn returns_empty_when_unsuccessful_or_missing_dlc() {
        let response = json!({"393380": {"success": false}});
        assert!(extract_dlc_app_ids(&response, 393_380).is_empty());

        let response = json!({"393380": {"success": true, "data": {}}});
        assert!(extract_dlc_app_ids(&response, 393_380).is_empty());
    }

    #[test]
    fn reads_name_from_a_successful_response() {
        let response = json!({"393381": {"success": true, "data": {"name": "Squad DLC"}}});
        assert_eq!(extract_name(&response, 393_381).as_deref(), Some("Squad DLC"));
    }

    #[test]
    fn returns_no_name_when_unsuccessful_or_missing() {
        let response = json!({"393381": {"success": false}});
        assert_eq!(extract_name(&response, 393_381), None);

        let response = json!({"393381": {"success": true, "data": {}}});
        assert_eq!(extract_name(&response, 393_381), None);
    }
}
