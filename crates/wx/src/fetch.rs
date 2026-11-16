//! Getting radar frames and hazard areas from NOAA over HTTPS. Plain blocking calls: the app runs
//! them on its own background thread. Each failure is returned, not retried here; the caller keeps
//! what it had and tries again on its next round.

use std::time::Duration;

pub use ureq::Agent;

pub const RADAR_DIR: &str = "https://mrms.ncep.noaa.gov/2D/MergedBaseReflectivityQC/";
pub const GAIRMET: &str = "https://aviationweather.gov/api/data/gairmet?format=geojson";
/// The largest frame accepted, compressed and not (a frame is ~0.7 MB both ways).
pub const MAX_FRAME: usize = 64 << 20;

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!(
            "maps-tauri/",
            env!("CARGO_PKG_VERSION"),
            " (personal project)"
        ))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    r.body_mut()
        .with_config()
        .limit(max as u64)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

/// The radar frames on the server now, oldest first.
pub fn radar_index(agent: &ureq::Agent) -> Result<Vec<(String, i64)>, String> {
    let html = get(agent, RADAR_DIR, 4 << 20)?;
    Ok(crate::radar::frames_in_index(&String::from_utf8_lossy(
        &html,
    )))
}

/// One frame, still gzipped (what the disk cache keeps).
pub fn radar_frame(agent: &ureq::Agent, name: &str) -> Result<Vec<u8>, String> {
    get(agent, &format!("{RADAR_DIR}{name}"), MAX_FRAME)
}

/// One G-AIRMET snapshot (fore = 0, 3, 6, 9 or 12 hours into the current package), as GeoJSON text.
/// An empty response (204: nothing issued) is an empty collection.
pub fn gairmet(agent: &ureq::Agent, fore: u32) -> Result<String, String> {
    let body = get(agent, &format!("{GAIRMET}&fore={fore}"), 16 << 20)?;
    if body.is_empty() {
        return Ok(r#"{"type":"FeatureCollection","features":[]}"#.into());
    }
    String::from_utf8(body).map_err(|e| format!("gairmet: {e}"))
}

/// The satellite image times nowCOAST offers now (its WMS capabilities), ascending.
pub fn satellite_times(agent: &ureq::Agent) -> Result<Vec<i64>, String> {
    let xml = get(agent, &crate::satellite::capabilities_url(), 16 << 20)?;
    let t = crate::satellite::times(&String::from_utf8_lossy(&xml));
    if t.is_empty() {
        return Err("satellite: no image times in the capabilities".into());
    }
    Ok(t)
}

/// The satellite image valid at `t`, as the server's PNG. A WMS error comes back as XML with a 200,
/// so anything that is not a PNG is an error.
pub fn satellite_image(agent: &ureq::Agent, t: i64) -> Result<Vec<u8>, String> {
    let body = get(
        agent,
        &crate::satellite::url(&crate::satellite::iso(t)),
        MAX_FRAME,
    )?;
    if !body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(format!(
            "satellite {}: not a PNG: {}",
            crate::satellite::iso(t),
            String::from_utf8_lossy(&body[..body.len().min(200)])
        ));
    }
    Ok(body)
}
