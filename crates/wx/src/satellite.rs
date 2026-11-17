//! Satellite clouds: GOES-East and West longwave infrared from NOAA's nowCOAST map server (a WMS).
//!
//! One image every 5 minutes, about 6.5 hours of history. Each image is requested for a fixed box
//! in Web Mercator, so drawing it is a plain texture lookup; it comes back as a palette PNG of grey
//! levels (brighter = colder = higher cloud), transparent where no satellite sees.
//!
//! Decoded to one byte per pixel: 0 = no data, else the grey level (at least 1). The shader turns
//! grey into cloud opacity, so the threshold can be tuned without fetching again. Measured on a
//! real image (2026-11-16 21:33Z): clear ground and warm surfaces sit at grey 48-128, cloud tops
//! 130-224.

use std::io::Cursor;

/// The box every image covers: GOES-East and West over North America.
///
/// In degrees as `[west, south, east, north]`: longitude -135..-55, latitude 15..60. The request
/// gives it in Web Mercator metres ([`bbox`]).
pub const BBOX_LONLAT: [f64; 4] = [-135.0, 15.0, -55.0, 60.0];
/// The image width in pixels for [`BBOX_LONLAT`] (about 4 km a pixel); [`bbox`] gives the height.
pub const WIDTH: u32 = 2048;
/// nowCOAST's WMS layer of GOES-East and West longwave infrared.
pub const LAYER: &str = "goes_longwave_imagery";
/// nowCOAST's satellite WMS endpoint.
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

/// [`BBOX_LONLAT`] in Web Mercator metres `[x0, y0, x1, y1]`, and the image height in pixels that
/// keeps pixels square at [`WIDTH`].
pub fn bbox() -> ([f64; 4], u32) {
    let (x0, y0) = merc(BBOX_LONLAT[0], BBOX_LONLAT[1]);
    let (x1, y1) = merc(BBOX_LONLAT[2], BBOX_LONLAT[3]);
    let h = (WIDTH as f64 * (y1 - y0) / (x1 - x0)).round() as u32;
    ([x0, y0, x1, y1], h)
}

/// The `GetMap` request for the image valid at `time_iso` (an ISO time from the capabilities).
pub fn url(time_iso: &str) -> String {
    let ([x0, y0, x1, y1], h) = bbox();
    format!(
        "{WMS}?service=WMS&version=1.3.0&request=GetMap&layers={LAYER}&styles=&crs=EPSG:3857&bbox={x0:.0},{y0:.0},{x1:.0},{y1:.0}&width={WIDTH}&height={h}&format=image/png&transparent=true&time={time_iso}"
    )
}

/// Outside GOES-East and West: NOAA's hourly global infrared mosaic (nowCOAST keeps about 8 hours).
///
/// Its grey scale differs from GOES's; [`to_goes_scale`] brings it onto GOES's so one cloud shading
/// serves both.
pub const GLOBAL_LAYER: &str = "global_longwave_imagery_mosaic";
/// The global image's box in degrees, `[west, south, east, north]`: the world to ±72.7° latitude.
pub const GLOBAL_BBOX_LONLAT: [f64; 4] = [-180.0, -72.7, 180.0, 72.7];
/// The global image's width in pixels; [`global_bbox`] gives the height.
pub const GLOBAL_WIDTH: u32 = 4096;

/// [`GLOBAL_BBOX_LONLAT`] in Web Mercator metres `[x0, y0, x1, y1]`, and the image height in pixels
/// that keeps pixels square at [`GLOBAL_WIDTH`].
pub fn global_bbox() -> ([f64; 4], u32) {
    let (x0, y0) = merc(GLOBAL_BBOX_LONLAT[0], GLOBAL_BBOX_LONLAT[1]);
    let (x1, y1) = merc(GLOBAL_BBOX_LONLAT[2], GLOBAL_BBOX_LONLAT[3]);
    let h = (GLOBAL_WIDTH as f64 * (y1 - y0) / (x1 - x0)).round() as u32;
    ([x0, y0, x1, y1], h)
}

/// The `GetMap` request for the global mosaic's image valid at `time_iso` (an ISO time).
pub fn url_global(time_iso: &str) -> String {
    let ([x0, y0, x1, y1], h) = global_bbox();
    format!(
        "{WMS}?service=WMS&version=1.3.0&request=GetMap&layers={GLOBAL_LAYER}&styles=&crs=EPSG:3857&bbox={x0:.0},{y0:.0},{x1:.0},{y1:.0}&width={GLOBAL_WIDTH}&height={h}&format=image/png&transparent=true&time={time_iso}"
    )
}

/// A global-mosaic grey level on the GOES layer's scale, by the fixed fit [`GAIN`] and [`OFFSET`].
///
/// GOES = 0.650 x global + 51.4, fitted pixel by pixel over the central US with both images from
/// 23Z on 2026-11-16 (r = 0.976, median error 1.6 grey levels, 5.3 within cloud). 0 (no data) stays
/// 0.
pub fn to_goes_scale(g: u8) -> u8 {
    to_scale(g, GAIN, OFFSET)
}
/// The fixed fit's gain (see [`to_goes_scale`]); used when [`fit_edge`] cannot fit a pair.
pub const GAIN: f64 = 0.650;
/// The fixed fit's offset in grey levels (see [`to_goes_scale`]).
pub const OFFSET: f64 = 51.4;

/// A greyscale image in Web Mercator, borrowed; 0 is no data.
pub struct View<'a> {
    /// `width * height` grey levels, row by row from the north.
    pub grey: &'a [u8],
    /// Pixels per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// The box the image covers, Web Mercator metres `[x0, y0, x1, y1]`.
    pub bbox: [f64; 4],
}

/// The global mosaic's grey levels on GOES's scale, fitted where it matters.
///
/// That is along the edge of GOES's coverage, where one image gives way to the other. The relation
/// is not one line everywhere (central US: GOES = 0.650 g + 51.4; the Canadian border: 0.420 g +
/// 99.3, measured 2026-11-16 23Z), so a pair of images is fitted on the GOES pixels within about a
/// degree of no data (or of the image's edge). Returns `(gain, offset, samples)`, or `None` when
/// fewer than 500 pixel pairs or a correlation under 0.8.
pub fn fit_edge(goes: &View, global: &View) -> Option<(f64, f64, usize)> {
    let (w, h) = (goes.width, goes.height);
    let [x0, y0, x1, y1] = goes.bbox;
    let (dx, dy) = ((x1 - x0) / w as f64, (y1 - y0) / h as f64);
    // the band's width: about a degree (111 km of Mercator x at the equator), in GOES pixels
    let d = (111_319.0 / dx).ceil().max(1.0) as usize;
    // no-data, by blocks of b pixels; a pixel is in the band if any block within r of its own has
    // no data, or lies outside the image (the global image takes over there too)
    let b = (d / 3).max(1);
    let (bw, bh) = (w.div_ceil(b), h.div_ceil(b));
    let mut empty = vec![false; bw * bh];
    for y in 0..h {
        for x in 0..w {
            if goes.grey[y * w + x] == 0 {
                empty[(y / b) * bw + x / b] = true;
            }
        }
    }
    let r = d.div_ceil(b) as isize;
    let near_empty = |bx: isize, by: isize| {
        (-r..=r).any(|oy| {
            (-r..=r).any(|ox| {
                let (cx, cy) = (bx + ox, by + oy);
                cx < 0
                    || cy < 0
                    || cx >= bw as isize
                    || cy >= bh as isize
                    || empty[cy as usize * bw + cx as usize]
            })
        })
    };
    let [gx0, gy0, gx1, gy1] = global.bbox;
    let (gdx, gdy) = (
        (gx1 - gx0) / global.width as f64,
        (gy1 - gy0) / global.height as f64,
    );
    let (mut n, mut sx, mut sy, mut sxx, mut sxy, mut syy) = (0usize, 0.0, 0.0, 0.0, 0.0, 0.0);
    for by in 0..bh {
        for bx in 0..bw {
            if !near_empty(bx as isize, by as isize) {
                continue;
            }
            for y in by * b..((by + 1) * b).min(h) {
                for x in bx * b..((bx + 1) * b).min(w) {
                    let v = goes.grey[y * w + x];
                    if v == 0 {
                        continue;
                    }
                    // the pixel's centre in metres, then in the global image
                    let mx = x0 + (x as f64 + 0.5) * dx;
                    let my = y1 - (y as f64 + 0.5) * dy;
                    let gx = ((mx - gx0) / gdx).floor();
                    let gy = ((gy1 - my) / gdy).floor();
                    if gx < 0.0
                        || gy < 0.0
                        || gx >= global.width as f64
                        || gy >= global.height as f64
                    {
                        continue;
                    }
                    let g = global.grey[gy as usize * global.width + gx as usize];
                    if g == 0 {
                        continue;
                    }
                    let (a, c) = (g as f64, v as f64);
                    n += 1;
                    sx += a;
                    sy += c;
                    sxx += a * a;
                    sxy += a * c;
                    syy += c * c;
                }
            }
        }
    }
    if n < 500 {
        return None;
    }
    let nf = n as f64;
    let (vx, vy, cov) = (sxx - sx * sx / nf, syy - sy * sy / nf, sxy - sx * sy / nf);
    if vx <= 0.0 || vy <= 0.0 || cov / (vx * vy).sqrt() < 0.8 {
        return None;
    }
    let k = cov / vx;
    Some((k, (sy - k * sx) / nf, n))
}

/// `g` (global mosaic, 0 = no data) as `gain * g + offset` on GOES's scale, kept in 1..255.
pub fn to_scale(g: u8, gain: f64, offset: f64) -> u8 {
    if g == 0 {
        return 0;
    }
    (gain * g as f64 + offset).round().clamp(1.0, 255.0) as u8
}

/// The WMS `GetCapabilities` request, which lists each layer's image times.
pub fn capabilities_url() -> String {
    format!("{WMS}?service=WMS&version=1.3.0&request=GetCapabilities")
}

/// The GOES layer's image times, ascending, in Unix seconds (see [`times_of`]).
pub fn times(capabilities: &str) -> Vec<i64> {
    times_of(capabilities, LAYER)
}

/// The image times the server offers for `layer`, ascending, in Unix seconds; empty if not found.
///
/// The layer's `<Dimension name="time">` in the capabilities XML is a comma-separated list of ISO
/// times.
pub fn times_of(capabilities: &str, layer: &str) -> Vec<i64> {
    let Some(i) = capabilities.find(&format!("<Name>{layer}</Name>")) else {
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

/// Unix seconds `t` as `2026-11-16T21:33:00Z`, for the WMS time parameter.
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

/// A decoded satellite image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// one byte per pixel, rows north to south: 0 = no data, else grey (1..=255)
    pub grey: Vec<u8>,
}

/// Decode the server's PNG (palette or grey, with or without transparency) to grey levels.
///
/// # Errors
///
/// The bytes are not a PNG the decoder reads, the image is not `width` x `height`, it is larger
/// than that size allows, or its colour type is not one handled here.
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

    /// A GOES image whose top half has no data, and a global image over the same box whose relation
    /// to GOES is one line near that edge and another deep inside: the fit must follow the edge's.
    #[test]
    fn the_fit_follows_the_edge_of_goes_coverage() {
        let (w, h) = (200usize, 200usize);
        let bbox = [0.0, 0.0, 2.0e6, 2.0e6];
        let mut glob = vec![0u8; w * h];
        let mut goes = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let g = 40 + ((x * 7 + y * 13) % 160) as u8; // varied cloud
                glob[y * w + x] = g;
                if y >= 100 {
                    // within ~25 px of the coverage edge (row 100) and of the sides: 0.42 g + 99;
                    // deep inside: 0.65 g + 51
                    let near = y < 125 || x < 25 || x >= w - 25 || y >= h - 25;
                    let v = if near {
                        0.42 * g as f64 + 99.3
                    } else {
                        0.65 * g as f64 + 51.4
                    };
                    goes[y * w + x] = v.round() as u8;
                }
            }
        }
        let gv = View {
            grey: &goes,
            width: w,
            height: h,
            bbox,
        };
        let wv = View {
            grey: &glob,
            width: w,
            height: h,
            bbox,
        };
        let (k, c, n) = fit_edge(&gv, &wv).expect("a fit");
        assert!(
            (k - 0.42).abs() < 0.03 && (c - 99.3).abs() < 4.0,
            "fitted {k:.3} g + {c:.1} on {n}"
        );
        // too little overlap: no fit
        let none = vec![0u8; w * h];
        assert!(fit_edge(
            &View {
                grey: &none,
                width: w,
                height: h,
                bbox
            },
            &wv
        )
        .is_none());
    }

    /// The global image covers a larger box at a different scale: pixels are matched by position.
    #[test]
    fn the_fit_matches_pixels_by_position_across_boxes() {
        let (w, h) = (100usize, 100usize);
        let gbox = [0.0, 0.0, 1.0e6, 1.0e6];
        // the global image: 4x the area at half the resolution, its value a function of position
        let wbox = [-1.0e6, -1.0e6, 3.0e6, 3.0e6];
        let at = |x: f64, y: f64| {
            40.0 + ((x / 1.0e5).floor() * 17.0 + (y / 1.0e5).floor() * 11.0) % 150.0
        };
        let glob: Vec<u8> = (0..200 * 200)
            .map(|i| {
                let (px, py) = ((i % 200) as f64 + 0.5, (i / 200) as f64 + 0.5);
                at(-1.0e6 + px * 2.0e4, 3.0e6 - py * 2.0e4) as u8
            })
            .collect();
        let goes: Vec<u8> = (0..w * h)
            .map(|i| {
                let (px, py) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                (0.5 * at(px * 1.0e4, 1.0e6 - py * 1.0e4) + 80.0).round() as u8
            })
            .collect();
        let (k, c, _) = fit_edge(
            &View {
                grey: &goes,
                width: w,
                height: h,
                bbox: gbox,
            },
            &View {
                grey: &glob,
                width: 200,
                height: 200,
                bbox: wbox,
            },
        )
        .expect("a fit");
        assert!(
            (k - 0.5).abs() < 0.05 && (c - 80.0).abs() < 6.0,
            "fitted {k:.3} g + {c:.1}"
        );
    }

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

    #[test]
    fn the_global_mosaic_on_the_goes_scale() {
        // fitted pixel by pixel over the central US, both images at 23Z on 2026-11-16 (r = 0.976)
        assert_eq!(to_goes_scale(0), 0, "no data stays no data");
        assert_eq!(to_goes_scale(1), 52);
        assert_eq!(to_goes_scale(100), 116);
        assert_eq!(to_goes_scale(255), 217);
        let caps = r#"<Layer><Name>goes_longwave_imagery</Name><Dimension name="time" default="x">2026-11-16T22:58:00.000Z</Dimension></Layer>
<Layer><Name>global_longwave_imagery_mosaic</Name><Dimension name="time" default="x">2026-11-16T22:00:00.000Z,2026-11-16T23:00:00.000Z</Dimension></Layer>"#;
        assert_eq!(times_of(caps, GLOBAL_LAYER).len(), 2);
        assert_eq!(times(caps).len(), 1, "the GOES list is still its own");
        let ([x0, y0, x1, y1], h) = global_bbox();
        assert!(
            (x0 + 20_037_508.0).abs() < 1.0
                && (x1 - 20_037_508.0).abs() < 1.0
                && (y0 + y1).abs() < 1.0,
            "{x0} {x1} {y0} {y1}"
        );
        assert!(h > 2400 && h < 2500, "{h}");
        let u = url_global("2026-11-16T23:00:00Z");
        assert!(
            u.contains("layers=global_longwave_imagery_mosaic")
                && u.contains("width=4096")
                && u.contains(&format!("height={h}"))
        );
    }

    /// A real image, when downloaded: `WX_SAT=file.png WX_SAT_SIZE=1400x1120`
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
