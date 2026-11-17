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
    satellite_times_of(agent, crate::satellite::LAYER)
}

/// The global mosaic's image times (hourly), ascending.
pub fn satellite_global_times(agent: &ureq::Agent) -> Result<Vec<i64>, String> {
    satellite_times_of(agent, crate::satellite::GLOBAL_LAYER)
}

/// The global mosaic's image valid at `t`, as the server's PNG.
pub fn satellite_global_image(agent: &ureq::Agent, t: i64) -> Result<Vec<u8>, String> {
    let body = get(
        agent,
        &crate::satellite::url_global(&crate::satellite::iso(t)),
        MAX_FRAME,
    )?;
    if !body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(format!(
            "global satellite {}: not a PNG",
            crate::satellite::iso(t)
        ));
    }
    Ok(body)
}

fn satellite_times_of(agent: &ureq::Agent, layer: &str) -> Result<Vec<i64>, String> {
    let xml = get(agent, &crate::satellite::capabilities_url(), 16 << 20)?;
    let t = crate::satellite::times_of(&String::from_utf8_lossy(&xml), layer);
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

/// Every station's latest METAR (the AWC cache file), those in the box. The file is gzipped CSV.
pub fn metars(agent: &ureq::Agent) -> Result<Vec<crate::metar::Metar>, String> {
    let gz = get(agent, crate::metar::CACHE_URL, 16 << 20)?;
    let csv = crate::radar::gunzip(&gz, 64 << 20)?;
    let (m, _) = crate::metar::parse_cache(&String::from_utf8_lossy(&csv))?;
    Ok(m)
}

/// U and V wind at `hpa` from GFS run `run`, forecast hour `fhr`, cut by NOMADS (about 150 KB).
pub fn wind(
    agent: &ureq::Agent,
    run: i64,
    fhr: i64,
    hpa: u32,
) -> Result<crate::wind::Wind, String> {
    let bytes = get(agent, &crate::wind::url(run, fhr, hpa), 16 << 20)?;
    crate::wind::from_grib(&bytes, hpa)
}

/// International SIGMETs in force now, or at `date` (Unix seconds) if given, as JSON text.
pub fn sigmets_international(agent: &ureq::Agent, date: Option<i64>) -> Result<String, String> {
    let mut url = "https://aviationweather.gov/api/data/isigmet?format=json".to_string();
    if let Some(t) = date {
        url.push_str(&format!("&date={}", crate::satellite::iso(t)));
    }
    let body = get(agent, &url, 16 << 20)?;
    String::from_utf8(body).map_err(|e| format!("isigmet: {e}"))
}

/// US domestic SIGMETs (convective among them) in force now, as JSON text.
pub fn sigmets_domestic(agent: &ureq::Agent) -> Result<String, String> {
    let body = get(
        agent,
        "https://aviationweather.gov/api/data/airsigmet?format=json",
        16 << 20,
    )?;
    String::from_utf8(body).map_err(|e| format!("airsigmet: {e}"))
}
