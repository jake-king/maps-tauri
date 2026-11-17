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

/// The one path served; it maps to the archive chosen with `MAPS_ARCHIVE`. Nothing else is served.
const SERVED_PATH: &str = "/basemap.pmtiles";

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
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Range::Unsatisfiable;
    };
    if spec.contains(',') || len == 0 {
        return Range::Unsatisfiable;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Range::Unsatisfiable;
    };
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
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Content-Range, Content-Length, ETag",
        )
        .header(header::ACCEPT_RANGES, "bytes")
}

fn error(status: StatusCode) -> Response<Vec<u8>> {
    respond(status).body(Vec::new()).expect("static response")
}

thread_local! {
    /// Each blocking-pool thread keeps one mesher and one output buffer, reused request after
    /// request: their capacity grows to the largest tile seen and stays (guile-2d's scratch memory).
    static MESHER: std::cell::RefCell<(vtile::mesh::Mesher, Vec<u8>)> = Default::default();
}

/// `/mesh/{z}/{x}/{y}` -> the tile's z/x/y in the archive, or None for any other path.
pub fn parse_mesh_path(path: &str) -> Option<(u8, u32, u32)> {
    let mut it = path.strip_prefix("/mesh/")?.split('/');
    let z: u8 = it.next()?.parse().ok()?;
    let x: u32 = it.next()?.parse().ok()?;
    let y: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || z > 24 || x >= (1 << z) || y >= (1 << z) {
        return None;
    }
    Some((z, x, y))
}

/// Serialized meshes by tile, least recently used evicted first, bounded by bytes. A hit skips read,
/// decode and mesh: the work a resting CPU does slowly (see the journal's profiling entry).
pub struct MeshCache {
    budget: usize,
    inner: std::sync::Mutex<MeshCacheInner>,
}

/// A cached mesh and the tick it was last used at, by tile.
type MeshEntries = std::collections::HashMap<(u8, u32, u32), (std::sync::Arc<Vec<u8>>, u64)>;

#[derive(Default)]
struct MeshCacheInner {
    map: MeshEntries,
    bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
}

impl MeshCache {
    pub fn new(budget_bytes: usize) -> Self {
        MeshCache {
            budget: budget_bytes,
            inner: Default::default(),
        }
    }

    pub fn get(&self, k: (u8, u32, u32)) -> Option<std::sync::Arc<Vec<u8>>> {
        let mut c = self.inner.lock().expect("mesh cache lock");
        c.tick += 1;
        let tick = c.tick;
        match c.map.get_mut(&k) {
            Some(e) => {
                e.1 = tick;
                let v = e.0.clone();
                c.hits += 1;
                Some(v)
            }
            None => {
                c.misses += 1;
                None
            }
        }
    }

    /// Insert, then evict least recently used entries until within budget. A mesh larger than the
    /// whole budget is not kept.
    pub fn put(&self, k: (u8, u32, u32), v: std::sync::Arc<Vec<u8>>) {
        if v.len() > self.budget {
            return;
        }
        let mut c = self.inner.lock().expect("mesh cache lock");
        c.tick += 1;
        let tick = c.tick;
        if let Some((old, _)) = c.map.insert(k, (v.clone(), tick)) {
            c.bytes -= old.len();
        }
        c.bytes += v.len();
        while c.bytes > self.budget {
            let Some((&victim, _)) = c.map.iter().min_by_key(|(_, (_, t))| *t) else {
                break;
            };
            if let Some((old, _)) = c.map.remove(&victim) {
                c.bytes -= old.len();
            }
        }
    }

    /// (entries, bytes, hits, misses); hits and misses reset on read.
    pub fn take_stats(&self) -> (usize, usize, u64, u64) {
        let mut c = self.inner.lock().expect("mesh cache lock");
        let out = (c.map.len(), c.bytes, c.hits, c.misses);
        c.hits = 0;
        c.misses = 0;
        out
    }
}

/// How long each stage of one mesh request took.
#[derive(Default, Debug, Clone, Copy)]
pub struct MeshStages {
    pub read: std::time::Duration,
    pub decode: std::time::Duration,
    pub mesh: std::time::Duration,
    pub serialize: std::time::Duration,
    pub copy: std::time::Duration,
    pub cache_hit: bool,
}

/// The native renderer's tiles, through the cache: decoded and meshed in Rust, sent as the mesh wire
/// format (crates/vtile/src/mesh.rs); 204 when the archive has no such tile. A hit is copied out of
/// the cache, a miss is built and kept.
pub fn handle_mesh_cached(
    archive: &vtile::pmtiles::Archive,
    cache: &MeshCache,
    z: u8,
    x: u32,
    y: u32,
) -> (Response<Vec<u8>>, MeshStages) {
    let t0 = std::time::Instant::now();
    if let Some(bytes) = cache.get((z, x, y)) {
        let body = bytes.as_ref().clone();
        let st = MeshStages {
            copy: t0.elapsed(),
            cache_hit: true,
            ..Default::default()
        };
        return (mesh_response(body), st);
    }
    let (res, st) = handle_mesh_timed(archive, z, x, y);
    if res.status() == StatusCode::OK {
        cache.put((z, x, y), std::sync::Arc::new(res.body().clone()));
    }
    (res, st)
}

fn mesh_response(body: Vec<u8>) -> Response<Vec<u8>> {
    respond(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, body.len())
        .body(body)
        .expect("valid response")
}

/// Build one tile's mesh response, with the time each stage took.
pub fn handle_mesh_timed(
    archive: &vtile::pmtiles::Archive,
    z: u8,
    x: u32,
    y: u32,
) -> (Response<Vec<u8>>, MeshStages) {
    use std::time::Instant;
    let mut st = MeshStages::default();
    let t0 = Instant::now();
    let tile = match archive.tile(z, x, y) {
        Ok(Some(t)) => t,
        Ok(None) => return (error(StatusCode::NO_CONTENT), st),
        Err(_) => return (error(StatusCode::INTERNAL_SERVER_ERROR), st),
    };
    let t1 = Instant::now();
    st.read = t1 - t0;
    let Ok(layers) = vtile::mvt::decode(&tile) else {
        return (error(StatusCode::INTERNAL_SERVER_ERROR), st);
    };
    let t2 = Instant::now();
    st.decode = t2 - t1;
    let body = MESHER.with(|m| {
        let (mesher, out) = &mut *m.borrow_mut();
        mesher.build(&layers, z);
        let t3 = Instant::now();
        st.mesh = t3 - t2;
        mesher.mesh.to_bytes(out);
        let t4 = Instant::now();
        st.serialize = t4 - t3;
        let body = out.clone();
        st.copy = t4.elapsed();
        body
    });
    (mesh_response(body), st)
}

/// Answer one request. Runs on a worker thread, never the UI thread.
pub fn handle(data_dir: &Path, archive_name: &str, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if req.method() == tauri::http::Method::OPTIONS {
        return respond(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range, If-Match")
            .body(Vec::new())
            .expect("static response");
    }
    if req.uri().path() != SERVED_PATH {
        return error(StatusCode::NOT_FOUND);
    }
    let path: PathBuf = data_dir.join(archive_name);
    let Ok(mut file) = File::open(&path) else {
        return error(StatusCode::NOT_FOUND);
    };
    let Ok(meta) = file.metadata() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let etag = format!("\"{len:x}-{mtime:x}\"");

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
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
    if file
        .seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut body))
        .is_err()
    {
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
        for h in [
            "bytes=100-",
            "bytes=5-4",
            "bytes=0-1,5-6",
            "bytes=-0",
            "bytes=-",
            "items=0-1",
            "bytes=a-b",
            "bytes=0-x",
        ] {
            assert_eq!(parse_range(Some(h), 100), Range::Unsatisfiable, "{h}");
        }
        assert_eq!(parse_range(Some("bytes=0-0"), 0), Range::Unsatisfiable);
        let big = MAX_RANGE * 2;
        assert_eq!(
            parse_range(Some(&format!("bytes=0-{}", MAX_RANGE)), big),
            Range::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some(&format!("bytes=0-{}", MAX_RANGE - 1)), big),
            Range::Bytes(0, MAX_RANGE - 1)
        );
    }

    #[test]
    fn mesh_cache_evicts_least_recently_used_within_budget() {
        let c = MeshCache::new(100);
        let v = |n: usize| std::sync::Arc::new(vec![0u8; n]);
        c.put((1, 0, 0), v(40));
        c.put((1, 0, 1), v(40));
        assert!(c.get((1, 0, 0)).is_some()); // (1,0,0) is now the most recent
        c.put((1, 1, 0), v(40)); // 120 > 100: evicts the least recent, (1,0,1)
        assert!(c.get((1, 0, 1)).is_none());
        assert!(c.get((1, 0, 0)).is_some() && c.get((1, 1, 0)).is_some());
        c.put((1, 1, 1), v(500)); // bigger than the budget: not kept, nothing evicted
        assert!(c.get((1, 1, 1)).is_none());
        let (n, bytes, hits, misses) = c.take_stats();
        assert_eq!((n, bytes), (2, 80));
        assert_eq!((hits, misses), (3, 2));
        assert_eq!(c.take_stats().2, 0, "hit counts reset on read");
        c.put((1, 0, 0), v(10)); // replacing an entry adjusts the byte count
        assert_eq!(c.take_stats().1, 50);
    }

    #[test]
    fn mesh_paths() {
        assert_eq!(
            parse_mesh_path("/mesh/14/2801/6543"),
            Some((14, 2801, 6543))
        );
        for bad in [
            "/mesh/14/2801",
            "/mesh/14/2801/6543/1",
            "/mesh/1/2/0",
            "/mesh/x/0/0",
            "/mesh/25/0/0",
            "/basemap.pmtiles",
        ] {
            assert_eq!(parse_mesh_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_listed_paths_are_served() {
        let dir = std::env::temp_dir();
        for p in ["/../Cargo.toml", "/basemap.pmtiles/..", "/etc/passwd", "/"] {
            let req = Request::builder()
                .uri(format!("tiles://localhost{p}"))
                .body(Vec::new())
                .unwrap();
            assert_eq!(
                handle(&dir, "basemap.pmtiles", &req).status(),
                StatusCode::NOT_FOUND,
                "{p}"
            );
        }
    }
}
