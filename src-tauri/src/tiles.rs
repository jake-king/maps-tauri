//! The `tiles://` URI scheme: serves the PMTiles archive to the webview with HTTP Range semantics.
//!
//! PMTiles is designed for exactly this: the client reads a 16 KB header, then a directory, then
//! individual tiles, each as a byte range of ONE file. In the browser version nginx does the Range
//! handling; here the Rust backend does, and the webview never touches the filesystem.
//!
//! Only names in [`SERVED`] are reachable, so no request path can name another file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tauri::http::{header, Request, Response, StatusCode};

/// Request path -> file name in the data directory. Nothing else is served.
const SERVED: &[(&str, &str)] = &[("/basemap.pmtiles", "basemap.pmtiles")];

/// Largest single range answered. PMTiles asks for 16 KB headers and directories of a few
/// hundred KB; a tile is rarely over 1 MB.
const MAX_RANGE: u64 = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Range {
    /// Inclusive byte offsets, already clamped to the file.
    Bytes(u64, u64),
    /// No Range header: the whole file.
    Whole,
    /// A Range header this server will not satisfy (416).
    Unsatisfiable,
}

/// Parse a single-range `Range` header against a file of `len` bytes. Multi-range requests are
/// refused rather than half-answered; PMTiles never sends them.
pub fn parse_range(header: Option<&str>, len: u64) -> Range {
    let Some(h) = header else { return Range::Whole };
    let Some(spec) = h.trim().strip_prefix("bytes=") else { return Range::Unsatisfiable };
    if spec.contains(',') || len == 0 {
        return Range::Unsatisfiable;
    }
    let Some((a, b)) = spec.split_once('-') else { return Range::Unsatisfiable };
    let (a, b) = (a.trim(), b.trim());
    let (start, end) = match (a.is_empty(), b.is_empty()) {
        // bytes=-N: the last N bytes
        (true, false) => match b.parse::<u64>() {
            Ok(0) | Err(_) => return Range::Unsatisfiable,
            Ok(n) => (len.saturating_sub(n), len - 1),
        },
        // bytes=S-: from S to the end
        (false, true) => match a.parse::<u64>() {
            Ok(s) => (s, len - 1),
            Err(_) => return Range::Unsatisfiable,
        },
        // bytes=S-E
        (false, false) => match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(s), Ok(e)) if s <= e => (s, e.min(len - 1)),
            _ => return Range::Unsatisfiable,
        },
        (true, true) => return Range::Unsatisfiable,
    };
    if start >= len || end - start + 1 > MAX_RANGE {
        return Range::Unsatisfiable;
    }
    Range::Bytes(start, end)
}

fn respond(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        // The page is served from another origin (Vite in dev, tauri:// in a build), so the
        // archive's headers must be readable cross-origin: pmtiles checks Content-Range and ETag.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, "Content-Range, Content-Length, ETag")
        .header(header::ACCEPT_RANGES, "bytes")
}

fn error(status: StatusCode) -> Response<Vec<u8>> {
    respond(status).body(Vec::new()).expect("static response")
}

/// Answer one request. Runs on a worker thread, never the UI thread.
pub fn handle(data_dir: &Path, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if req.method() == tauri::http::Method::OPTIONS {
        return respond(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range, If-Match")
            .body(Vec::new())
            .expect("static response");
    }
    let Some((_, name)) = SERVED.iter().find(|(p, _)| *p == req.uri().path()) else {
        return error(StatusCode::NOT_FOUND);
    };
    let path: PathBuf = data_dir.join(name);
    let Ok(mut file) = File::open(&path) else { return error(StatusCode::NOT_FOUND) };
    let Ok(meta) = file.metadata() else { return error(StatusCode::INTERNAL_SERVER_ERROR) };
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let etag = format!("\"{len:x}-{mtime:x}\"");

    let range_header = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match parse_range(range_header, len) {
        Range::Bytes(s, e) => (StatusCode::PARTIAL_CONTENT, s, e),
        Range::Whole if len <= MAX_RANGE => (StatusCode::OK, 0, len.saturating_sub(1)),
        Range::Whole | Range::Unsatisfiable => {
            return respond(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Vec::new())
                .expect("static response");
        }
    };
    let mut body = vec![0u8; (end - start + 1) as usize];
    if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut body)).is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut res = respond(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::ETAG, etag);
    if status == StatusCode::PARTIAL_CONTENT {
        res = res.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    res.body(body).expect("valid response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-15"), 100), Range::Bytes(0, 15));
        assert_eq!(parse_range(Some("bytes=90-200"), 100), Range::Bytes(90, 99)); // clamped
        assert_eq!(parse_range(Some("bytes=95-"), 100), Range::Bytes(95, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Range::Bytes(90, 99));
        assert_eq!(parse_range(Some("bytes=-500"), 100), Range::Bytes(0, 99));
    }

    #[test]
    fn unsatisfiable() {
        for h in ["bytes=100-", "bytes=5-4", "bytes=0-1,5-6", "bytes=-0", "bytes=-", "items=0-1",
                  "bytes=a-b", "bytes=0-x"] {
            assert_eq!(parse_range(Some(h), 100), Range::Unsatisfiable, "{h}");
        }
        assert_eq!(parse_range(Some("bytes=0-0"), 0), Range::Unsatisfiable);
        let big = MAX_RANGE * 2;
        assert_eq!(parse_range(Some(&format!("bytes=0-{}", MAX_RANGE)), big), Range::Unsatisfiable);
        assert_eq!(parse_range(Some(&format!("bytes=0-{}", MAX_RANGE - 1)), big), Range::Bytes(0, MAX_RANGE - 1));
    }

    #[test]
    fn only_listed_paths_are_served() {
        let dir = std::env::temp_dir();
        for p in ["/../Cargo.toml", "/basemap.pmtiles/..", "/etc/passwd", "/"] {
            let req = Request::builder().uri(format!("tiles://localhost{p}")).body(Vec::new()).unwrap();
            assert_eq!(handle(&dir, &req).status(), StatusCode::NOT_FOUND, "{p}");
        }
    }
}
