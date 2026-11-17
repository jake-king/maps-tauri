//! Getting radar frames and hazard areas from NOAA over HTTPS.
//!
//! Plain blocking calls: the app runs them on its own background thread. Each failure is returned,
//! not retried here; the caller keeps what it had and tries again on its next round.
//!
//! # Errors
//!
//! Every function here returns `Err` with a message naming the URL when the request fails (no
//! connection, a non-2xx status, the 60 s timeout) or the body is larger than its limit, and when
//! replaying a tape that does not hold that URL ([`crate::tape`]).

use std::time::Duration;

pub use ureq::Agent;

/// The directory listing of NCEP's MRMS base reflectivity mosaics; frame names are appended to it.
pub const RADAR_DIR: &str = "https://mrms.ncep.noaa.gov/2D/MergedBaseReflectivityQC/";
/// The Aviation Weather Center's G-AIRMET query as GeoJSON; `&fore=` is appended per snapshot.
pub const GAIRMET: &str = "https://aviationweather.gov/api/data/gairmet?format=geojson";
/// The largest frame accepted, compressed and not (a frame is ~0.7 MB both ways).
pub const MAX_FRAME: usize = 64 << 20;

/// An HTTP agent with a 60 s overall timeout and the app's user agent, shared by every call here.
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

/// Every request goes through here: answered from the tape when replaying, kept on it when
/// recording ([`crate::tape`]).
fn get(agent: &ureq::Agent, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let tape = crate::tape::global();
    if let Some(r) = tape.replay(url) {
        return r;
    }
    let mut r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let body = r
        .body_mut()
        .with_config()
        .limit(max as u64)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))?;
    tape.record(url, &body);
    Ok(body)
}

/// The radar frames on the server now, oldest first: each file name with its time in Unix seconds.
///
/// # Errors
///
/// The request fails (see the module's `# Errors`), or the listing is over 4 MiB.
pub fn radar_index(agent: &ureq::Agent) -> Result<Vec<(String, i64)>, String> {
    let html = get(agent, RADAR_DIR, 4 << 20)?;
    Ok(crate::radar::frames_in_index(&String::from_utf8_lossy(
        &html,
    )))
}

/// One frame, still gzipped (what the disk cache keeps).
///
/// # Errors
///
/// The request fails, or the frame is over [`MAX_FRAME`] bytes.
pub fn radar_frame(agent: &ureq::Agent, name: &str) -> Result<Vec<u8>, String> {
    get(agent, &format!("{RADAR_DIR}{name}"), MAX_FRAME)
}

/// One G-AIRMET snapshot (fore = 0, 3, 6, 9 or 12 hours into the current package), as GeoJSON text.
///
/// An empty response (204: nothing issued) is an empty collection.
///
/// # Errors
///
/// The request fails, the body is over 16 MiB, or it is not UTF-8.
pub fn gairmet(agent: &ureq::Agent, fore: u32) -> Result<String, String> {
    let body = get(agent, &format!("{GAIRMET}&fore={fore}"), 16 << 20)?;
    if body.is_empty() {
        return Ok(r#"{"type":"FeatureCollection","features":[]}"#.into());
    }
    String::from_utf8(body).map_err(|e| format!("gairmet: {e}"))
}

/// The satellite image times nowCOAST offers now (its WMS capabilities), ascending, in Unix
/// seconds.
///
/// # Errors
///
/// The request fails, the capabilities are over 16 MiB, or they list no time for the layer.
pub fn satellite_times(agent: &ureq::Agent) -> Result<Vec<i64>, String> {
    satellite_times_of(agent, crate::satellite::LAYER)
}

/// The global mosaic's image times (hourly), ascending, in Unix seconds.
///
/// # Errors
///
/// The request fails, the capabilities are over 16 MiB, or they list no time for the layer.
pub fn satellite_global_times(agent: &ureq::Agent) -> Result<Vec<i64>, String> {
    satellite_times_of(agent, crate::satellite::GLOBAL_LAYER)
}

/// The global mosaic's image valid at `t` (Unix seconds), as the server's PNG.
///
/// # Errors
///
/// The request fails, the body is over [`MAX_FRAME`] bytes, or it is not a PNG (a WMS error).
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

/// The satellite image valid at `t` (Unix seconds), as the server's PNG.
///
/// A WMS error comes back as XML with a 200, so anything that is not a PNG is an error.
///
/// # Errors
///
/// The request fails, the body is over [`MAX_FRAME`] bytes, or it is not a PNG; the message then
/// quotes the first 200 bytes of what came back.
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

/// Every station's latest METAR worldwide (the AWC cache file); rows that cannot be read are
/// dropped.
///
/// The file is gzipped CSV.
///
/// # Errors
///
/// The request fails, the download is over 16 MiB or 64 MiB unpacked, it does not gunzip, or the
/// CSV is empty or lacks one of the columns [`crate::metar::parse_cache`] reads.
pub fn metars(agent: &ureq::Agent) -> Result<Vec<crate::metar::Metar>, String> {
    let gz = get(agent, crate::metar::CACHE_URL, 16 << 20)?;
    let csv = crate::radar::gunzip(&gz, 64 << 20)?;
    let (m, _) = crate::metar::parse_cache(&String::from_utf8_lossy(&csv))?;
    Ok(m)
}

/// U and V wind at `hpa` from GFS run `run`, forecast hour `fhr`, cut by NOMADS (about 150 KB).
///
/// `run` is the model run's start in Unix seconds, `hpa` the pressure level in hectopascals.
///
/// # Errors
///
/// The request fails, the body is over 16 MiB, or it is not the GRIB2 the decoder expects.
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
///
/// # Errors
///
/// The request fails, the body is over 16 MiB, or it is not UTF-8.
pub fn sigmets_international(agent: &ureq::Agent, date: Option<i64>) -> Result<String, String> {
    let mut url = "https://aviationweather.gov/api/data/isigmet?format=json".to_string();
    if let Some(t) = date {
        url.push_str(&format!("&date={}", crate::satellite::iso(t)));
    }
    let body = get(agent, &url, 16 << 20)?;
    String::from_utf8(body).map_err(|e| format!("isigmet: {e}"))
}

/// US domestic SIGMETs (convective among them) in force now, as JSON text.
///
/// # Errors
///
/// The request fails, the body is over 16 MiB, or it is not UTF-8.
pub fn sigmets_domestic(agent: &ureq::Agent) -> Result<String, String> {
    let body = get(
        agent,
        "https://aviationweather.gov/api/data/airsigmet?format=json",
        16 << 20,
    )?;
    String::from_utf8(body).map_err(|e| format!("airsigmet: {e}"))
}
