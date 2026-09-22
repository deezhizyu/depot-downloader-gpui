use std::io::Read as _;
use std::time::Duration;

/// Logos are cosmetic, so a slow CDN response must not hold up populating
/// the game list.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The capsule image Steam serves for every app id, no API call needed.
pub fn logo_url(app_id: u64) -> String {
    format!("https://cdn.akamai.steamstatic.com/steam/apps/{app_id}/capsule_184x69.jpg")
}

/// Fetches the JPEG bytes of `app_id`'s capsule image. Blocking; call from a
/// background thread.
pub fn fetch_logo_bytes(app_id: u64) -> anyhow::Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let mut body = Vec::new();
    agent
        .get(logo_url(app_id))
        .call()?
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)?;
    Ok(body)
}
