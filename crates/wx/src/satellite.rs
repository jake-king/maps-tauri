//! Satellite clouds: GOES-East and West longwave infrared from NOAA's nowCOAST map server (a WMS),
//! one image every 5 minutes, about 6.5 hours of history. Each image is requested for a fixed box
//! in Web Mercator, so drawing it is a plain texture lookup; it comes back as a palette PNG of grey
//! levels (brighter = colder = higher cloud), transparent where no satellite sees.
//!
//! Decoded to one byte per pixel: 0 = no data, else the grey level (at least 1). The shader turns
//! grey into cloud opacity, so the threshold can be tuned without fetching again. Measured on a real
//! image (2026-11-16 21:33Z): clear ground and warm surfaces sit at grey 48-128, cloud tops 130-224.

use std::io::Cursor;

/// The box every image covers: GOES-East and West over North America (longitude -135..-55,
/// latitude 15..60), in Web Mercator metres, and the image size for it (about 4 km a pixel).
pub const BBOX_LONLAT: [f64; 4] = [-135.0, 15.0, -55.0, 60.0];
pub const WIDTH: u32 = 2048;
pub const LAYER: &str = "goes_longwave_imagery";
pub const WMS: &str = "https://nowcoast.noaa.gov/geoserver/satellite/wms";

const R: f64 = 6_378_137.0;

fn merc(lon: f64, lat: f64) -> (f64, f64) {
    (
        R * lon.to_radians(),
        R * (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
            .tan()
            .ln(),
    )
}

/// The box in Web Mercator metres (x0, y0, x1, y1) and the image height that keeps pixels square.
pub fn bbox() -> ([f64; 4], u32) {
    let (x0, y0) = merc(BBOX_LONLAT[0], BBOX_LONLAT[1]);
    let (x1, y1) = merc(BBOX_LONLAT[2], BBOX_LONLAT[3]);
    let h = (WIDTH as f64 * (y1 - y0) / (x1 - x0)).round() as u32;
    ([x0, y0, x1, y1], h)
}

/// The GetMap request for the image valid at `time` (an ISO time from the capabilities).
pub fn url(time_iso: &str) -> String {
    let ([x0, y0, x1, y1], h) = bbox();
    format!(
        "{WMS}?service=WMS&version=1.3.0&request=GetMap&layers={LAYER}&styles=&crs=EPSG:3857&bbox={x0:.0},{y0:.0},{x1:.0},{y1:.0}&width={WIDTH}&height={h}&format=image/png&transparent=true&time={time_iso}"
    )
}

pub fn capabilities_url() -> String {
    format!("{WMS}?service=WMS&version=1.3.0&request=GetCapabilities")
}

/// The image times the server offers for [`LAYER`], ascending, from its capabilities XML: the
/// layer's `<Dimension name="time">` is a comma-separated list of ISO times.
pub fn times(capabilities: &str) -> Vec<i64> {
    let Some(i) = capabilities.find(&format!("<Name>{LAYER}</Name>")) else {
        return Vec::new();
    };
    let rest = &capabilities[i..];
    let end = rest.find("</Layer>").unwrap_or(rest.len());
    let layer = &rest[..end];
    let Some(d) = layer.find("<Dimension name=\"time\"") else {
        return Vec::new();
    };
    let Some(open) = layer[d..].find('>') else {
        return Vec::new();
    };
    let body_start = d + open + 1;
    let Some(close) = layer[body_start..].find("</Dimension>") else {
        return Vec::new();
    };
    let mut out: Vec<i64> = layer[body_start..body_start + close]
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            // "2026-11-16T21:33:00.000Z": parse_time wants no fraction or a 3-digit one, both fine
            crate::hazards::parse_time(s)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// "2026-11-16T21:33:00Z" for the WMS time parameter.
pub fn iso(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// one byte per pixel, rows north to south: 0 = no data, else grey (1..=255)
    pub grey: Vec<u8>,
}

/// Decode the server's PNG (palette or grey, with or without transparency) to grey levels.
pub fn decode(png_bytes: &[u8], width: u32, height: u32) -> Result<Image, String> {
    let limits = png::Limits {
        bytes: (width as usize) * (height as usize) * 4 + 1024,
    };
    let mut d = png::Decoder::new_with_limits(Cursor::new(png_bytes), limits);
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(|e| format!("satellite png: {e}"))?;
    let info = r.info();
    if info.width != width || info.height != height {
        return Err(format!(
            "satellite png is {} x {}, asked for {width} x {height}",
            info.width, info.height
        ));
    }
    let size = r.output_buffer_size().ok_or("satellite png: too large")?;
    let mut buf = vec![0u8; size];
    let out = r
        .next_frame(&mut buf)
        .map_err(|e| format!("satellite png: {e}"))?;
    let buf = &buf[..out.buffer_size()];
    let n = (width * height) as usize;
    let grey: Vec<u8> = match out.color_type {
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| if p[3] == 0 { 0 } else { luma(p[0], p[1], p[2]) })
            .collect(),
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| luma(p[0], p[1], p[2]))
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| if p[1] == 0 { 0 } else { p[0].max(1) })
            .collect(),
        png::ColorType::Grayscale => buf.iter().map(|&g| g.max(1)).collect(),
        other => return Err(format!("satellite png: colour type {other:?}")),
    };
    if grey.len() != n {
        return Err(format!(
            "satellite png: {} pixels for {width} x {height}",
            grey.len()
        ));
    }
    Ok(Image {
        width,
        height,
        grey,
    })
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000).max(1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_png(w: u32, h: u32, idx: &[u8], palette: &[[u8; 3]], alpha: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Indexed);
            e.set_depth(png::BitDepth::Eight);
            e.set_palette(palette.iter().flatten().copied().collect::<Vec<u8>>());
            e.set_trns(alpha.to_vec());
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(idx).unwrap();
        }
        out
    }

    #[test]
    fn palette_grey_with_transparency_as_the_server_sends_it() {
        // entry 0: transparent black (no data); 1: grey 100 (ground); 2: grey 200 (cloud); 3: black, opaque
        let png = indexed_png(
            4,
            1,
            &[0, 1, 2, 3],
            &[[0, 0, 0], [100, 100, 100], [200, 200, 200], [0, 0, 0]],
            &[0, 255, 255, 255],
        );
        let img = decode(&png, 4, 1).unwrap();
        assert_eq!(
            img.grey,
            vec![0, 100, 200, 1],
            "no data is 0; an opaque black pixel is still data"
        );
    }

    #[test]
    fn refuses_the_wrong_size_and_garbage() {
        let png = indexed_png(2, 1, &[1, 1], &[[0, 0, 0], [9, 9, 9]], &[0, 255]);
        assert!(decode(&png, 3, 1).unwrap_err().contains("2 x 1"));
        assert!(decode(b"<ServiceExceptionReport>", 2, 1).is_err());
    }

    #[test]
    fn times_from_the_capabilities() {
        let caps = r#"<Layer queryable="1"><Name>other</Name><Dimension name="time" default="x">2020-01-01T00:00:00.000Z</Dimension></Layer>
<Layer queryable="1"><Name>goes_longwave_imagery</Name><Title>t</Title>
<Dimension name="time" default="2026-11-16T21:33:00Z" units="ISO8601">2026-11-16T21:28:00.000Z,2026-11-16T15:03:00.000Z,2026-11-16T21:33:00.000Z,bad</Dimension></Layer>"#;
        let t = times(caps);
        assert_eq!(
            t.len(),
            3,
            "the other layer's time and the bad entry are not included"
        );
        assert_eq!(iso(t[0]), "2026-11-16T15:03:00Z");
        assert_eq!(iso(t[2]), "2026-11-16T21:33:00Z");
        assert!(times("<WMS_Capabilities/>").is_empty());
    }

    #[test]
    fn iso_round_trips() {
        for s in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T23:59:59Z",
            "2026-11-16T21:33:00Z",
            "2100-03-01T00:00:00Z",
        ] {
            assert_eq!(iso(crate::hazards::parse_time(s).unwrap()), s);
        }
    }

    #[test]
    fn the_box_and_request() {
        let ([x0, y0, x1, y1], h) = bbox();
        assert!(
            (x0 - -15_028_131.0).abs() < 1.0 && (x1 - -6_122_571.0).abs() < 1.0,
            "{x0} {x1}"
        );
        assert!(
            y0 > 1_600_000.0 && y1 > 8_300_000.0 && y1 < 8_500_000.0,
            "{y0} {y1}"
        );
        assert!(h > 1500 && h < 1600, "{h}");
        let u = url("2026-11-16T21:33:00Z");
        assert!(
            u.contains("layers=goes_longwave_imagery")
                && u.contains("crs=EPSG:3857")
                && u.contains(&format!("height={h}"))
                && u.ends_with("time=2026-11-16T21:33:00Z")
        );
    }

    /// A real image, when downloaded: WX_SAT=file.png WX_SAT_SIZE=1400x1120
    #[test]
    fn a_real_image() {
        let (Ok(path), Ok(size)) = (std::env::var("WX_SAT"), std::env::var("WX_SAT_SIZE")) else {
            return;
        };
        let (w, h) = size
            .split_once('x')
            .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
            .unwrap();
        let img = decode(&std::fs::read(path).unwrap(), w, h).unwrap();
        let none = img.grey.iter().filter(|&&g| g == 0).count();
        let cloud = img.grey.iter().filter(|&&g| g >= 130).count();
        eprintln!(
            "no data {:.1}%, grey >= 130 {:.1}%",
            100.0 * none as f64 / img.grey.len() as f64,
            100.0 * cloud as f64 / img.grey.len() as f64
        );
        assert!(none > 0 && cloud > 0 && none + cloud < img.grey.len());
    }
}
