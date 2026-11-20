//! The disk cache for the remote planet: byte ranges of one archive version in a SQLite file, so a
//! region seen once draws from disk afterwards, and with no network at all (vtile's
//! `Archive::open_cached`). One archive per file: opening a new version deletes the old one's rows.
//! Past `max_bytes` the least recently used ranges go, except the header and root directory (the
//! first 16 KiB by the PMTiles spec), which an offline open needs.
//!
//! `MAPS_DISK_CACHE=<path>` moves the file, `MAPS_DISK_CACHE=0` turns the cache off;
//! `MAPS_DISK_CACHE_MB` caps it (default 2048).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

/// Ranges starting below this are the header and root directory: never evicted.
const PINNED: i64 = 16384;

pub struct DiskCache {
    conn: Mutex<Connection>,
    version: String,
    max_bytes: i64,
    /// bytes of range data stored, kept in step with the table
    total: AtomicI64,
    /// a clock for least-recently-used: bumped on every get and put
    clock: AtomicI64,
}

/// Where the cache lives, or None when switched off.
pub fn configured_path() -> Option<PathBuf> {
    path_from(
        std::env::var("MAPS_DISK_CACHE").ok(),
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// `MAPS_DISK_CACHE` (a path, or "0" for off), else under the XDG cache directory, else ~/.cache.
fn path_from(
    setting: Option<String>,
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match setting.as_deref() {
        Some("0") => None,
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => {
            let base = xdg
                .filter(|p| p.is_absolute())
                .or_else(|| home.map(|h| h.join(".cache")))?;
            Some(base.join("local.maps-tauri").join("tiles.sqlite"))
        }
    }
}

pub fn configured_max_bytes() -> i64 {
    max_bytes_from(std::env::var("MAPS_DISK_CACHE_MB").ok())
}

/// Megabytes, default 2048; anything not a positive number up to 1 TiB is the default.
fn max_bytes_from(setting: Option<String>) -> i64 {
    let mb = setting
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|&mb| (1..=1 << 20).contains(&mb))
        .unwrap_or(2048);
    mb << 20
}

fn connect(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS ranges (
           version TEXT NOT NULL,
           off INTEGER NOT NULL,
           len INTEGER NOT NULL,
           data BLOB NOT NULL,
           used INTEGER NOT NULL,
           PRIMARY KEY (version, off, len)
         );
         CREATE INDEX IF NOT EXISTS ranges_used ON ranges (used);",
    )?;
    Ok(conn)
}

fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

impl DiskCache {
    /// The cache for `version` of the archive at `url`, made current: other versions' ranges are
    /// deleted (a replaced planet's bytes are of no use).
    pub fn open(path: &Path, url: &str, version: &str, max_bytes: i64) -> rusqlite::Result<Self> {
        let conn = connect(path)?;
        conn.execute("DELETE FROM ranges WHERE version <> ?1", [version])?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('url', ?1), ('version', ?2)",
            [url, version],
        )?;
        Self::from_conn(conn, version.to_owned(), max_bytes)
    }

    /// The cache as last made current, if it was for `url` and holds anything: for opening the
    /// archive with no network.
    pub fn open_last(path: &Path, url: &str, max_bytes: i64) -> rusqlite::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = connect(path)?;
        let (Some(u), Some(v)) = (meta(&conn, "url")?, meta(&conn, "version")?) else {
            return Ok(None);
        };
        if u != url {
            return Ok(None);
        }
        Ok(Some(Self::from_conn(conn, v, max_bytes)?))
    }

    fn from_conn(conn: Connection, version: String, max_bytes: i64) -> rusqlite::Result<Self> {
        let (total, clock): (i64, i64) = conn.query_row(
            "SELECT coalesce(sum(length(data)), 0), coalesce(max(used), 0) FROM ranges WHERE version = ?1",
            [&version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(DiskCache {
            conn: Mutex::new(conn),
            version,
            max_bytes,
            total: AtomicI64::new(total),
            clock: AtomicI64::new(clock),
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Bytes of range data stored.
    pub fn bytes(&self) -> i64 {
        self.total.load(Ordering::Relaxed)
    }

    fn tick(&self) -> i64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn try_get(&self, offset: u64, len: usize) -> rusqlite::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().expect("disk cache");
        let key = params![self.version, offset as i64, len as i64];
        let data: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM ranges WHERE version = ?1 AND off = ?2 AND len = ?3",
                key,
                |r| r.get(0),
            )
            .optional()?;
        if data.is_some() {
            // best effort: a cache that can be read but not written (a full disk, another instance
            // holding the write lock) still answers; only the eviction order goes stale
            if let Err(e) = conn.execute(
                "UPDATE ranges SET used = ?4 WHERE version = ?1 AND off = ?2 AND len = ?3",
                params![self.version, offset as i64, len as i64, self.tick()],
            ) {
                warn(&e);
            }
        }
        Ok(data)
    }

    fn try_put(&self, offset: u64, bytes: &[u8]) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("disk cache");
        let n = conn.execute(
            "INSERT OR IGNORE INTO ranges (version, off, len, data, used) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.version, offset as i64, bytes.len() as i64, bytes, self.tick()],
        )?;
        if n == 0 {
            return Ok(()); // already stored (two threads fetched the same range)
        }
        let total =
            self.total.fetch_add(bytes.len() as i64, Ordering::Relaxed) + bytes.len() as i64;
        if total > self.max_bytes {
            self.evict(&conn, total - self.max_bytes * 9 / 10)?;
        }
        Ok(())
    }

    /// Deletes the least recently used unpinned ranges until `need` bytes are freed (or none are
    /// left to delete).
    fn evict(&self, conn: &Connection, need: i64) -> rusqlite::Result<()> {
        let mut freed = 0;
        while freed < need {
            let batch: Vec<(i64, i64, i64)> = conn
                .prepare(
                    "SELECT off, len, length(data) FROM ranges WHERE version = ?1 AND off >= ?2
                     ORDER BY used LIMIT 256",
                )?
                .query_map(params![self.version, PINNED], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            if batch.is_empty() {
                break;
            }
            let tx = conn.unchecked_transaction()?;
            let mut batch_freed = 0;
            for (off, len, size) in batch {
                tx.execute(
                    "DELETE FROM ranges WHERE version = ?1 AND off = ?2 AND len = ?3",
                    params![self.version, off, len],
                )?;
                batch_freed += size;
                if freed + batch_freed >= need {
                    break;
                }
            }
            tx.commit()?;
            // counted per committed batch: a later batch that fails leaves the count true
            self.total.fetch_sub(batch_freed, Ordering::Relaxed);
            freed += batch_freed;
        }
        Ok(())
    }
}

/// The cache when the file cannot be opened: always empty, so the archive reads the network.
pub struct NoCache;

impl vtile::pmtiles::RangeCache for NoCache {
    fn get(&self, _: u64, _: usize) -> Option<Vec<u8>> {
        None
    }
    fn put(&self, _: u64, _: &[u8]) {}
}

/// Failures are reported once and otherwise ignored: a cache that cannot read or write makes the
/// archive slower, never broken.
static WARNED: std::sync::Once = std::sync::Once::new();

fn warn(e: &rusqlite::Error) {
    WARNED.call_once(|| eprintln!("[cache] disk cache error, carrying on without it: {e}"));
}

impl vtile::pmtiles::RangeCache for DiskCache {
    fn get(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.try_get(offset, len).unwrap_or_else(|e| {
            warn(&e);
            None
        })
    }

    fn put(&self, offset: u64, bytes: &[u8]) {
        if let Err(e) = self.try_put(offset, bytes) {
            warn(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtile::pmtiles::RangeCache;

    /// A cache file under logs/, removed (with its WAL files) when dropped.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../logs");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(format!(
                "diskcache-test-{}-{name}.sqlite",
                std::process::id()
            ));
            let s = Scratch(p);
            s.remove();
            s
        }
        fn remove(&self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0.display()));
            }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            self.remove();
        }
    }

    const URL: &str = "http://example.invalid/a.pmtiles";

    #[test]
    fn a_range_put_is_read_back_by_offset_and_length() {
        let s = Scratch::new("roundtrip");
        let c = DiskCache::open(&s.0, URL, "\"v1\"", 1 << 20).unwrap();
        c.put(20000, b"tile bytes");
        assert_eq!(c.get(20000, 10).as_deref(), Some(&b"tile bytes"[..]));
        assert_eq!(c.get(20000, 9), None); // same offset, other length: another range
        assert_eq!(c.get(20001, 10), None);
        assert_eq!(c.bytes(), 10);
    }

    #[test]
    fn a_range_stored_twice_counts_once() {
        let s = Scratch::new("twice");
        let c = DiskCache::open(&s.0, URL, "v", 1 << 20).unwrap();
        c.put(20000, b"abc");
        c.put(20000, b"abc");
        assert_eq!(c.bytes(), 3);
    }

    #[test]
    fn ranges_survive_closing_and_reopening_the_same_version() {
        let s = Scratch::new("persist");
        DiskCache::open(&s.0, URL, "\"v1\"", 1 << 20)
            .unwrap()
            .put(20000, b"kept");
        let c = DiskCache::open(&s.0, URL, "\"v1\"", 1 << 20).unwrap();
        assert_eq!(c.get(20000, 4).as_deref(), Some(&b"kept"[..]));
        assert_eq!(c.bytes(), 4);
    }

    #[test]
    fn a_new_version_deletes_the_old_one() {
        let s = Scratch::new("version");
        DiskCache::open(&s.0, URL, "\"v1\"", 1 << 20)
            .unwrap()
            .put(20000, b"old");
        let c = DiskCache::open(&s.0, URL, "\"v2\"", 1 << 20).unwrap();
        assert_eq!(c.get(20000, 3), None);
        assert_eq!(c.bytes(), 0);
        let back = DiskCache::open(&s.0, URL, "\"v1\"", 1 << 20).unwrap();
        assert_eq!(
            back.get(20000, 3),
            None,
            "the old version's rows are gone, not hidden"
        );
    }

    #[test]
    fn open_last_finds_the_current_version_for_the_same_url_only() {
        let s = Scratch::new("last");
        assert!(DiskCache::open_last(&s.0, URL, 1 << 20).unwrap().is_none()); // no file yet
        DiskCache::open(&s.0, URL, "\"v2\"", 1 << 20)
            .unwrap()
            .put(0, b"header");
        let c = DiskCache::open_last(&s.0, URL, 1 << 20).unwrap().unwrap();
        assert_eq!(c.version(), "\"v2\"");
        assert_eq!(c.get(0, 6).as_deref(), Some(&b"header"[..]));
        assert!(
            DiskCache::open_last(&s.0, "http://other.invalid/b.pmtiles", 1 << 20)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn past_the_cap_the_least_recently_used_go_first_and_the_header_stays() {
        let s = Scratch::new("evict");
        let c = DiskCache::open(&s.0, URL, "v", 1000).unwrap();
        c.put(0, &[0u8; 127]); // header: pinned
        c.put(127, &[1u8; 200]); // root: pinned
        c.put(20000, &[2u8; 200]);
        c.put(30000, &[3u8; 200]);
        c.put(40000, &[4u8; 200]);
        assert_eq!(c.bytes(), 927);
        assert!(c.get(20000, 200).is_some()); // 20000 is now the most recently used
        c.put(50000, &[5u8; 200]); // 1127 > 1000: free down to 900, oldest unpinned first
        assert_eq!(c.get(30000, 200), None);
        assert_eq!(c.get(40000, 200), None);
        assert!(c.get(20000, 200).is_some());
        assert!(c.get(50000, 200).is_some());
        assert!(c.get(0, 127).is_some() && c.get(127, 200).is_some());
        assert_eq!(c.bytes(), 727);
        // the count matches the table after a reopen
        drop(c);
        assert_eq!(DiskCache::open(&s.0, URL, "v", 1000).unwrap().bytes(), 727);
    }

    #[test]
    fn eviction_stops_when_only_pinned_ranges_are_left() {
        let s = Scratch::new("pinned");
        let c = DiskCache::open(&s.0, URL, "v", 100).unwrap();
        c.put(0, &[0u8; 127]);
        c.put(127, &[1u8; 300]);
        assert_eq!(c.bytes(), 427);
        assert!(c.get(0, 127).is_some() && c.get(127, 300).is_some());
    }

    #[test]
    fn an_unwritable_cache_reads_as_empty_and_does_not_panic() {
        let s = Scratch::new("broken");
        let c = DiskCache::open(&s.0, URL, "v", 1 << 20).unwrap();
        c.conn
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE ranges")
            .unwrap();
        c.put(20000, b"x");
        assert_eq!(c.get(20000, 1), None);
    }

    #[test]
    fn a_cache_another_writer_has_locked_still_answers_reads() {
        let s = Scratch::new("locked");
        let c = DiskCache::open(&s.0, URL, "v", 1 << 20).unwrap();
        c.put(20000, b"kept");
        c.conn
            .lock()
            .unwrap()
            .busy_timeout(std::time::Duration::from_millis(50))
            .unwrap();
        let other = Connection::open(&s.0).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap(); // holds the write lock
        assert_eq!(
            c.get(20000, 4).as_deref(),
            Some(&b"kept"[..]),
            "the read must survive a failed LRU update"
        );
        c.put(30000, b"lost"); // cannot be stored, and must not be counted
        assert_eq!(c.bytes(), 4);
        other.execute_batch("ROLLBACK").unwrap();
        assert_eq!(c.get(30000, 4), None);
    }

    #[test]
    fn no_cache_is_always_empty() {
        let c = NoCache;
        c.put(0, b"x");
        assert_eq!(c.get(0, 1), None);
    }

    #[test]
    fn the_size_cap_by_setting() {
        assert_eq!(max_bytes_from(None), 2048 << 20);
        assert_eq!(max_bytes_from(Some("512".into())), 512 << 20);
        assert_eq!(max_bytes_from(Some(" 1 ".into())), 1 << 20);
        for bad in ["0", "-1", "x", "", "1099511627776"] {
            assert_eq!(max_bytes_from(Some(bad.into())), 2048 << 20, "{bad}");
        }
        assert_eq!(max_bytes_from(Some((1 << 20).to_string())), 1 << 40);
    }

    // Unix path rules ("/x" is not absolute on Windows, and the paths follow HOME and XDG)
    #[test]
    #[cfg(unix)]
    fn the_cache_path_by_setting() {
        let home = Some(PathBuf::from("/home/u"));
        let xdg = Some(PathBuf::from("/x"));
        assert_eq!(path_from(Some("0".into()), xdg.clone(), home.clone()), None);
        assert_eq!(
            path_from(Some("/c/t.sqlite".into()), xdg.clone(), home.clone()),
            Some(PathBuf::from("/c/t.sqlite"))
        );
        assert_eq!(
            path_from(None, xdg.clone(), home.clone()),
            Some(PathBuf::from("/x/local.maps-tauri/tiles.sqlite"))
        );
        assert_eq!(
            path_from(Some(String::new()), None, home.clone()),
            Some(PathBuf::from(
                "/home/u/.cache/local.maps-tauri/tiles.sqlite"
            ))
        );
        // a relative XDG_CACHE_HOME is invalid by the spec: ignored
        assert_eq!(
            path_from(None, Some(PathBuf::from("rel")), home),
            Some(PathBuf::from(
                "/home/u/.cache/local.maps-tauri/tiles.sqlite"
            ))
        );
        assert_eq!(path_from(None, None, None), None);
    }
}
