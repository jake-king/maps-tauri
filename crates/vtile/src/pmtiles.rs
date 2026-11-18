//! PMTiles v3 archives, read from a file, over HTTP Range requests, or from a disk cache alone.
//!
//! An archive is a 127-byte header, a root directory, optional leaf directories, then tile data.
//! Directories are varint-encoded and (here) gzip-compressed; tiles are addressed by a Hilbert-curve
//! tile id. Spec: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use flate2::read::GzDecoder;

/// The fields of the archive header this reader uses (offsets and lengths in bytes).
#[derive(Debug, Clone)]
pub struct Header {
    /// Where the root directory starts, from the start of the archive.
    pub root_offset: u64,
    /// The root directory's compressed length.
    pub root_length: u64,
    /// Where the leaf directory section starts; leaf entries' offsets count from here.
    pub leaf_offset: u64,
    /// Where the tile data section starts; tile entries' offsets count from here.
    pub tile_data_offset: u64,
    /// Directory compression, as the spec numbers it (2: gzip, the only one read here).
    pub internal_compression: u8,
    /// Tile compression, as the spec numbers it (2: gzip, the only one read here).
    pub tile_compression: u8,
    /// Tile format, as the spec numbers it (1: Mapbox Vector Tile).
    pub tile_type: u8,
    /// The lowest zoom the archive holds.
    pub min_zoom: u8,
    /// The highest zoom the archive holds.
    pub max_zoom: u8,
    /// The area covered, in degrees: min lon, min lat, max lon, max lat.
    pub bounds: [f64; 4],
}

/// One directory entry: a run of tiles sharing the same bytes, or a pointer to a leaf directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The Hilbert tile id of the first tile the entry covers.
    pub tile_id: u64,
    /// Byte offset into the tile data section, or into the leaf section for a leaf directory.
    pub offset: u64,
    /// Length in bytes of the (compressed) tile or leaf directory.
    pub length: u32,
    /// How many consecutive tile ids share these bytes; 0 marks a leaf directory.
    pub run_length: u32,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
fn i32le(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// Parse the 127-byte header at the start of `b`.
///
/// # Errors
///
/// `InvalidData` when `b` is shorter than 127 bytes, does not start with `PMTiles`, or is not
/// spec version 3.
pub fn parse_header(b: &[u8]) -> io::Result<Header> {
    if b.len() < 127 || &b[0..7] != b"PMTiles" {
        return Err(bad("not a PMTiles archive"));
    }
    if b[7] != 3 {
        return Err(bad("only PMTiles spec version 3 is supported"));
    }
    Ok(Header {
        root_offset: u64le(b, 8),
        root_length: u64le(b, 16),
        leaf_offset: u64le(b, 40),
        tile_data_offset: u64le(b, 56),
        internal_compression: b[97],
        tile_compression: b[98],
        tile_type: b[99],
        min_zoom: b[100],
        max_zoom: b[101],
        bounds: [
            i32le(b, 102) as f64 / 1e7,
            i32le(b, 106) as f64 / 1e7,
            i32le(b, 110) as f64 / 1e7,
            i32le(b, 114) as f64 / 1e7,
        ],
    })
}

/// A protobuf-style varint (LEB128), advancing `pos`.
///
/// # Errors
///
/// `InvalidData` when `b` ends before the varint does, or it runs past 10 bytes.
///
/// # Examples
///
/// ```
/// let b = [0xac, 0x02, 0x05];
/// let mut pos = 0;
/// assert_eq!(vtile::pmtiles::varint(&b, &mut pos).unwrap(), 300);
/// assert_eq!(pos, 2);
/// assert!(vtile::pmtiles::varint(&[0x80], &mut 0).is_err()); // ends mid-varint
/// ```
pub fn varint(b: &[u8], pos: &mut usize) -> io::Result<u64> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos).ok_or_else(|| bad("truncated varint"))?;
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(v);
        }
    }
    Err(bad("varint longer than 10 bytes"))
}

/// Parse a decompressed directory into its entries.
///
/// The layout is the entry count, then ids (delta-encoded), run lengths, lengths, and offsets
/// (stored plus one; 0 means "right after the previous entry").
///
/// # Errors
///
/// `InvalidData` when a varint is truncated or too long, the entry count exceeds the directory's
/// size, or a tile id or offset overflows `u64`.
pub fn parse_directory(b: &[u8]) -> io::Result<Vec<Entry>> {
    let mut p = 0;
    let n = varint(b, &mut p)? as usize;
    if n > b.len() {
        return Err(bad("directory entry count exceeds its size"));
    }
    let mut e = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 0,
            run_length: 0
        };
        n
    ];
    let mut last = 0u64;
    for x in e.iter_mut() {
        // checked: a corrupt delta must be an error, not a debug-build panic or a wrapped id
        last = last
            .checked_add(varint(b, &mut p)?)
            .ok_or_else(|| bad("directory tile id overflows"))?;
        x.tile_id = last;
    }
    for x in e.iter_mut() {
        x.run_length = varint(b, &mut p)? as u32;
    }
    for x in e.iter_mut() {
        x.length = varint(b, &mut p)? as u32;
    }
    for i in 0..n {
        let v = varint(b, &mut p)?;
        e[i].offset = if v == 0 && i > 0 {
            e[i - 1]
                .offset
                .checked_add(u64::from(e[i - 1].length))
                .ok_or_else(|| bad("directory offset overflows"))?
        } else {
            v.saturating_sub(1)
        };
    }
    Ok(e)
}

/// The Hilbert tile id of z/x/y, as the spec defines it.
///
/// # Examples
///
/// ```
/// use vtile::pmtiles::{tile_id_to_zxy, zxy_to_tile_id};
///
/// assert_eq!(zxy_to_tile_id(0, 0, 0), 0);
/// // ids count every tile of the lower zooms first: zoom 1 starts at 1, zoom 2 at 5
/// assert_eq!(zxy_to_tile_id(1, 0, 0), 1);
/// assert_eq!(zxy_to_tile_id(2, 0, 0), 5);
/// assert_eq!(tile_id_to_zxy(zxy_to_tile_id(12, 703, 1635)), Some((12, 703, 1635)));
/// ```
pub fn zxy_to_tile_id(z: u8, x: u32, y: u32) -> u64 {
    let mut acc: u64 = ((1u64 << (2 * z as u64)) - 1) / 3; // tiles at all lower zooms
    let (mut x, mut y) = (x as u64, y as u64);
    let mut s = 1u64 << z.saturating_sub(1);
    let n = 1u64 << z;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        acc += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = n - 1 - x;
                y = n - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    acc
}

/// Where a section's entry starts in the file: the section's offset from the header plus the
/// entry's offset in it. Both come from the file, so the sum is checked.
fn offset_in(section: u64, offset: u64, what: &str) -> io::Result<u64> {
    section
        .checked_add(offset)
        .ok_or_else(|| bad(&format!("{what} offset overflows")))
}

/// The entry covering `id`: an exact match, or a run that started before it.
pub fn find(entries: &[Entry], id: u64) -> Option<Entry> {
    let i = entries.partition_point(|e| e.tile_id <= id);
    let e = *entries.get(i.checked_sub(1)?)?;
    // run_length 0 marks a leaf directory, which may hold the id; otherwise the run covers it.
    // id >= e.tile_id here, so the distance cannot underflow (or, compared, overflow)
    if e.run_length == 0 || id - e.tile_id < u64::from(e.run_length) {
        Some(e)
    } else {
        None
    }
}

/// Decompress gzip data `b` into `out`, which is cleared first.
///
/// # Errors
///
/// Whatever the gzip decoder reports for corrupt or truncated input.
pub fn gunzip(b: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
    out.clear();
    GzDecoder::new(b).read_to_end(out)?;
    Ok(())
}

/// Somewhere to keep byte ranges of ONE version of a remote archive between runs.
///
/// A region seen once is read from disk afterwards, and with no network at all. The archive asks
/// for the same
/// (offset, length) pairs every time (the header, directories and tiles are fixed ranges), so a
/// range is the unit stored. Implementations swallow their own failures: a cache that cannot read
/// or write is a slower archive, never a broken one.
pub trait RangeCache: Send + Sync {
    /// The `len` bytes stored at `offset`, or `None` when they were never stored (or cannot be read).
    fn get(&self, offset: u64, len: usize) -> Option<Vec<u8>>;
    /// Store `bytes` as the range at `offset`; a failure is swallowed.
    fn put(&self, offset: u64, bytes: &[u8]);
}

/// Where an archive's bytes come from: a local file (pread), a URL (HTTP Range requests over one
/// reused connection pool, through a disk cache when one is given), or only a disk cache (offline).
enum Source {
    File(File),
    #[cfg(feature = "http")]
    Http {
        agent: ureq::Agent,
        url: String,
        /// The version read at open; later requests carry it in If-Range, so a planet replaced on
        /// the server mid-session answers 200 (refused below) instead of bytes of another build
        etag: Option<String>,
        cache: Option<Box<dyn RangeCache>>,
    },
    Cached(Box<dyn RangeCache>),
}

/// The error a cache-only (offline) archive gives for a range it does not have.
fn not_cached(offset: u64, len: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{len} bytes at {offset} are not in the offline cache"),
    )
}

#[cfg(feature = "http")]
fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .http_status_as_error(false) // a non-206 is reported by fetch
        .build()
        .into()
}

/// One Range request: exactly `len` bytes at `offset`, and the response's ETag.
#[cfg(feature = "http")]
fn fetch(
    agent: &ureq::Agent,
    url: &str,
    offset: u64,
    len: usize,
    if_range: Option<&str>,
) -> io::Result<(Vec<u8>, Option<String>)> {
    if len == 0 {
        return Ok((Vec::new(), None));
    }
    let last = offset
        .checked_add(len as u64 - 1)
        .ok_or_else(|| bad("range end overflows"))?;
    let range = format!("bytes={offset}-{last}");
    let mut req = agent.get(url).header("Range", &range);
    if let Some(tag) = if_range {
        req = req.header("If-Range", tag);
    }
    let mut res = req.call().map_err(io::Error::other)?;
    if res.status().as_u16() != 206 {
        // a 200 here would be the whole planet (or, after If-Range, a different one): refuse it
        return Err(bad(&format!(
            "expected 206 Partial Content for {range}, got {}",
            res.status()
        )));
    }
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let buf = res
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_to_vec()
        .map_err(io::Error::other)?;
    if buf.len() != len {
        return Err(bad(&format!(
            "asked for {len} bytes at {offset}, got {}",
            buf.len()
        )));
    }
    Ok((buf, etag))
}

impl Source {
    /// The bytes, and whether they came from the network or a file (false: from the cache).
    fn read(&self, offset: u64, len: usize) -> io::Result<(Vec<u8>, bool)> {
        match self {
            Source::File(f) => {
                let mut buf = vec![0u8; len];
                f.read_exact_at(&mut buf, offset)?;
                Ok((buf, true))
            }
            #[cfg(feature = "http")]
            Source::Http {
                agent,
                url,
                etag,
                cache,
            } => {
                if let Some(hit) = cache.as_ref().and_then(|c| c.get(offset, len)) {
                    if hit.len() == len {
                        return Ok((hit, false));
                    }
                }
                // If-Range needs a strong ETag (RFC 9110 13.1.5): with a weak one a compliant
                // server would answer every request with the whole file
                let if_range = etag.as_deref().filter(|t| !t.starts_with("W/"));
                let (buf, got) = fetch(agent, url, offset, len, if_range)?;
                // and in case a server ignores If-Range: bytes of another version are refused here,
                // before they can be used or cached under this one
                if let (Some(want), Some(got)) = (etag, &got) {
                    if want != got {
                        return Err(bad(&format!(
                            "the archive changed on the server (ETag {got}, opened as {want})"
                        )));
                    }
                }
                if let Some(c) = cache {
                    c.put(offset, &buf);
                }
                Ok((buf, true))
            }
            Source::Cached(c) => match c.get(offset, len) {
                Some(hit) if hit.len() == len => Ok((hit, false)),
                _ => Err(not_cached(offset, len)),
            },
        }
    }
}

/// Leaf directories read so far, by (offset, length). A planet archive has many; each costs a round
/// trip over the network, so a region's leaf is fetched once and kept.
const LEAF_CACHE: usize = 512;

/// Leaf directories by (offset, length) within the leaf section.
type LeafCache = HashMap<(u64, u64), Arc<Vec<Entry>>>;

/// An open archive: its header and root directory, read once, and the leaf directories read since.
pub struct Archive {
    source: Source,
    /// The parsed header.
    pub header: Header,
    root: Vec<Entry>,
    leaves: Mutex<LeafCache>,
    /// round trips made (header/root, leaves, tiles): for measuring a remote archive
    pub reads: std::sync::atomic::AtomicU64,
    /// leaves and tiles answered by the disk cache instead
    pub cache_hits: std::sync::atomic::AtomicU64,
}

impl Archive {
    /// A local archive file.
    ///
    /// # Errors
    ///
    /// The file cannot be opened or read, its header is not PMTiles v3, its directories or tiles
    /// are not gzip-compressed, or the root directory does not decode.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_source(Source::File(File::open(path)?))
    }

    /// An archive served over HTTP(S) with Range support, e.g. a planet on the LAN.
    ///
    /// # Errors
    ///
    /// The header or root directory request fails or is not answered with exactly the bytes asked
    /// for (status 206), or the archive is not one [`Archive::open`] accepts.
    #[cfg(feature = "http")]
    pub fn open_url(url: &str) -> io::Result<Self> {
        let agent = http_agent();
        Self::from_source(Source::Http {
            agent,
            url: url.to_owned(),
            etag: None,
            cache: None,
        })
    }

    /// A remote archive read through a disk cache. The header always comes from the network: its
    /// ETag names the version, `make_cache(version)` opens that version's cache, and every later
    /// range is read from it when present. With no ETag the version is a hash of the header bytes
    /// (a rebuilt archive has a different header), and requests go without If-Range.
    ///
    /// # Errors
    ///
    /// As [`Archive::open_url`], and whatever `make_cache` returns.
    #[cfg(feature = "http")]
    pub fn open_url_cached(
        url: &str,
        make_cache: impl FnOnce(&str) -> io::Result<Box<dyn RangeCache>>,
    ) -> io::Result<Self> {
        let agent = http_agent();
        let (head, etag) = fetch(&agent, url, 0, 127, None)?;
        let version = etag.clone().unwrap_or_else(|| header_version(&head));
        let cache = make_cache(&version)?;
        cache.put(0, &head);
        Self::from_source(Source::Http {
            agent,
            url: url.to_owned(),
            etag,
            cache: Some(cache),
        })
    }

    /// An archive read only from a disk cache: what was seen before, with no network. A tile or
    /// leaf it does not have reads as absent, so the renderer draws what it has.
    ///
    /// # Errors
    ///
    /// `NotFound` when the cache lacks the header or root directory, or the archive is not one
    /// [`Archive::open`] accepts.
    pub fn open_cached(cache: Box<dyn RangeCache>) -> io::Result<Self> {
        Self::from_source(Source::Cached(cache))
    }

    /// True for an archive opened from the cache alone.
    pub fn offline(&self) -> bool {
        matches!(self.source, Source::Cached(_))
    }

    fn from_source(source: Source) -> io::Result<Self> {
        let (head, _) = source.read(0, 127)?;
        let header = parse_header(&head)?;
        if header.internal_compression != 2 || header.tile_compression != 2 {
            return Err(bad("this reader handles gzip-compressed archives only"));
        }
        let (root, _) = Self::read_dir(&source, header.root_offset, header.root_length)?;
        Ok(Archive {
            source,
            header,
            root,
            leaves: Mutex::new(HashMap::new()),
            reads: Default::default(),
            cache_hits: Default::default(),
        })
    }

    /// A directory, and whether it came over the network (false: from the cache).
    fn read_dir(source: &Source, offset: u64, length: u64) -> io::Result<(Vec<Entry>, bool)> {
        let (raw, net) = source.read(offset, length as usize)?;
        let mut dec = Vec::new();
        gunzip(&raw, &mut dec)?;
        Ok((parse_directory(&dec)?, net))
    }

    fn leaf(&self, offset: u64, length: u64) -> io::Result<Arc<Vec<Entry>>> {
        if let Some(l) = self
            .leaves
            .lock()
            .expect("leaf cache")
            .get(&(offset, length))
        {
            return Ok(l.clone());
        }
        let at = offset_in(self.header.leaf_offset, offset, "leaf directory")?;
        let (dir, net) = Self::read_dir(&self.source, at, length)?;
        self.count(net);
        let dir = Arc::new(dir);
        let mut c = self.leaves.lock().expect("leaf cache");
        if c.len() >= LEAF_CACHE {
            c.clear(); // crude, but bounded: refill on demand
        }
        c.insert((offset, length), dir.clone());
        Ok(dir)
    }

    fn count(&self, network: bool) {
        let c = if network {
            &self.reads
        } else {
            &self.cache_hits
        };
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The decompressed tile at z/x/y, or None if the archive does not have it (or, offline, the
    /// cache does not).
    ///
    /// # Errors
    ///
    /// A read fails (file, network, or a remote archive that changed since it was opened), a
    /// directory or tile does not decompress or decode, or directories nest deeper than 4.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> io::Result<Option<Vec<u8>>> {
        match self.tile_inner(z, x, y) {
            Err(e) if self.offline() && e.kind() == io::ErrorKind::NotFound => Ok(None),
            r => r,
        }
    }

    fn tile_inner(&self, z: u8, x: u32, y: u32) -> io::Result<Option<Vec<u8>>> {
        let id = zxy_to_tile_id(z, x, y);
        let mut leaf: Option<Arc<Vec<Entry>>> = None;
        for _depth in 0..4 {
            let dir: &[Entry] = leaf.as_deref().map_or(&self.root, |v| v);
            let Some(e) = find(dir, id) else {
                return Ok(None);
            };
            if e.run_length == 0 {
                leaf = Some(self.leaf(e.offset, u64::from(e.length))?);
                continue;
            }
            // find() returned the entry at or before id, so the distance cannot underflow
            if id - e.tile_id >= u64::from(e.run_length) {
                return Ok(None);
            }
            let at = offset_in(self.header.tile_data_offset, e.offset, "tile")?;
            let (raw, net) = self.source.read(at, e.length as usize)?;
            self.count(net);
            let mut out = Vec::new();
            gunzip(&raw, &mut out)?;
            return Ok(Some(out));
        }
        Err(bad("directory nesting deeper than 4"))
    }

    /// Every (z, x, y) the archive addresses, in tile-id order: for whole-archive tests.
    ///
    /// # Errors
    ///
    /// A leaf directory cannot be read, decompressed or decoded.
    pub fn all_tiles(&self) -> io::Result<Vec<(u8, u32, u32)>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for e in dir {
                if e.run_length == 0 {
                    stack.push(
                        Self::read_dir(
                            &self.source,
                            offset_in(self.header.leaf_offset, e.offset, "leaf directory")?,
                            u64::from(e.length),
                        )?
                        .0,
                    );
                } else {
                    let end = e
                        .tile_id
                        .checked_add(u64::from(e.run_length))
                        .ok_or_else(|| bad("tile id run overflows"))?;
                    for id in e.tile_id..end {
                        out.push(tile_id_to_zxy(id).ok_or_else(|| bad("tile id past zoom 31"))?);
                    }
                }
            }
        }
        out.sort_by_key(|&(z, x, y)| zxy_to_tile_id(z, x, y));
        Ok(out)
    }
}

/// A version name for an archive served without an ETag: FNV-1a of its header bytes.
pub fn header_version(head: &[u8]) -> String {
    let h = head.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("h{h:016x}")
}

/// The inverse of [`zxy_to_tile_id`], or `None` for an id past zoom 31, the deepest a `u64` id
/// reaches (such an id comes only from a corrupt directory).
pub fn tile_id_to_zxy(id: u64) -> Option<(u8, u32, u32)> {
    let mut acc = 0u64;
    let mut z = 0u8;
    loop {
        if z > 31 {
            return None;
        }
        // 4^z tiles at zoom z; acc + n stays below 2^63 up to zoom 31
        let n = 1u64 << (2 * u64::from(z));
        if acc + n > id {
            break;
        }
        acc += n;
        z += 1;
    }
    let pos = id - acc;
    let n = 1u64 << z;
    let (mut x, mut y, mut t) = (0u64, 0u64, pos);
    let mut s = 1u64;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        t /= 4;
        s *= 2;
    }
    Some((z, x as u32, y as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_match_the_spec_examples() {
        // from the PMTiles v3 spec
        assert_eq!(zxy_to_tile_id(0, 0, 0), 0);
        assert_eq!(zxy_to_tile_id(1, 0, 0), 1);
        assert_eq!(zxy_to_tile_id(1, 0, 1), 2);
        assert_eq!(zxy_to_tile_id(1, 1, 1), 3);
        assert_eq!(zxy_to_tile_id(1, 1, 0), 4);
        assert_eq!(zxy_to_tile_id(2, 0, 0), 5);
    }

    #[test]
    fn tile_id_round_trips() {
        for z in 0..=8u8 {
            let n = 1u32 << z;
            for x in (0..n).step_by(((n / 7).max(1)) as usize) {
                for y in (0..n).step_by(((n / 5).max(1)) as usize) {
                    assert_eq!(
                        tile_id_to_zxy(zxy_to_tile_id(z, x, y)),
                        Some((z, x, y)),
                        "{z}/{x}/{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn varints() {
        let b = [0x96, 0x01, 0x05, 0xff, 0xff, 0x03];
        let mut p = 0;
        assert_eq!(varint(&b, &mut p).unwrap(), 150);
        assert_eq!(varint(&b, &mut p).unwrap(), 5);
        assert_eq!(varint(&b, &mut p).unwrap(), 65535);
        assert!(varint(&[0x80], &mut 0).is_err());
    }

    #[test]
    fn directory_offsets_chain_when_zero() {
        // 2 entries: ids 5, 7; runs 1, 1; lengths 10, 20; offsets 100 (stored +1 = 101), then 0 = follows
        let d = [2, 5, 2, 1, 1, 10, 20, 101, 0];
        let e = parse_directory(&d).unwrap();
        assert_eq!(
            e[0],
            Entry {
                tile_id: 5,
                offset: 100,
                length: 10,
                run_length: 1
            }
        );
        assert_eq!(
            e[1],
            Entry {
                tile_id: 7,
                offset: 110,
                length: 20,
                run_length: 1
            }
        );
    }

    // ---- builders: hand-made archives, so these tests need no data file ----

    fn enc_varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// A directory as the spec lays it out. `offsets` are written as given (the stored +1 form).
    fn dir_raw(ids: &[u64], runs: &[u64], lens: &[u64], stored_offsets: &[u64]) -> Vec<u8> {
        let mut b = Vec::new();
        enc_varint(ids.len() as u64, &mut b);
        let mut last = 0;
        for &id in ids {
            enc_varint(id - last, &mut b);
            last = id;
        }
        for &v in runs.iter().chain(lens).chain(stored_offsets) {
            enc_varint(v, &mut b);
        }
        b
    }

    /// A gzip-compressed directory with explicit offsets for every entry.
    fn dir_gz(entries: &[Entry]) -> Vec<u8> {
        let ids: Vec<u64> = entries.iter().map(|e| e.tile_id).collect();
        let runs: Vec<u64> = entries.iter().map(|e| e.run_length.into()).collect();
        let lens: Vec<u64> = entries.iter().map(|e| e.length.into()).collect();
        let offs: Vec<u64> = entries.iter().map(|e| e.offset + 1).collect();
        gz(&dir_raw(&ids, &runs, &lens, &offs))
    }

    fn gz(b: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    fn header_bytes() -> Vec<u8> {
        let mut h = vec![0u8; 127];
        h[0..7].copy_from_slice(b"PMTiles");
        h[7] = 3;
        h
    }

    fn put_u64(h: &mut [u8], at: usize, v: u64) {
        h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn ent(tile_id: u64, offset: u64, length: u32, run_length: u32) -> Entry {
        Entry {
            tile_id,
            offset,
            length,
            run_length,
        }
    }

    /// Tile contents of the hand-built archive, by tile id.
    const T0: &[u8] = b"tile zero";
    const T12: &[u8] = b"tiles one and two";
    const T4: &[u8] = b"tile four (in the leaf)";
    const T5: &[u8] = b"tile five (in the leaf)";

    /// An archive: root = [id 0 run 1] [id 1 run 2] [id 3 run 0 -> leaf]; leaf = [id 4] [id 5].
    /// Id 3 (z1 1/1) is covered by the leaf entry but absent from the leaf. Every section starts
    /// at a different offset so a field read from the wrong place shows.
    fn archive_bytes() -> Vec<u8> {
        let tiles: Vec<Vec<u8>> = [T0, T12, T4, T5].iter().map(|t| gz(t)).collect();
        let mut data = Vec::new();
        let mut at = Vec::new();
        for t in &tiles {
            at.push((data.len() as u64, t.len() as u32));
            data.extend_from_slice(t);
        }
        let leaf = dir_gz(&[ent(4, at[2].0, at[2].1, 1), ent(5, at[3].0, at[3].1, 1)]);
        let root = dir_gz(&[
            ent(0, at[0].0, at[0].1, 1),
            ent(1, at[1].0, at[1].1, 2),
            ent(3, 0, leaf.len() as u32, 0),
        ]);
        let root_off = 127u64;
        let leaf_off = root_off + root.len() as u64 + 3; // 3 bytes of padding
        let data_off = leaf_off + leaf.len() as u64 + 5;
        let mut h = header_bytes();
        put_u64(&mut h, 8, root_off);
        put_u64(&mut h, 16, root.len() as u64);
        put_u64(&mut h, 40, leaf_off);
        put_u64(&mut h, 56, data_off);
        h[97] = 2;
        h[98] = 2;
        h[99] = 1;
        let mut out = h;
        out.extend_from_slice(&root);
        out.extend_from_slice(&[0xee; 3]);
        out.extend_from_slice(&leaf);
        out.extend_from_slice(&[0xee; 5]);
        out.extend_from_slice(&data);
        out
    }

    /// A scratch file under the workspace's logs/ (never /tmp), removed on drop.
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../logs");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(format!("vtile-test-{}-{name}.pmtiles", std::process::id()));
            std::fs::write(&p, bytes).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn err_text<T>(r: io::Result<T>) -> String {
        let e = r.err().expect("expected an error");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        e.to_string()
    }

    // ---- parse_header ----

    #[test]
    fn header_fields_are_read_from_their_spec_offsets() {
        let mut h = header_bytes();
        for (i, b) in h.iter_mut().enumerate().skip(8) {
            *b = i as u8; // every byte distinct, so a wrong offset reads a wrong value
        }
        put_u64(&mut h, 8, 0x1111);
        put_u64(&mut h, 16, 0x2222);
        put_u64(&mut h, 24, 0xdead); // metadata offset: not read
        put_u64(&mut h, 40, 0x4444);
        put_u64(&mut h, 48, 0xbeef); // leaf length: not read
        put_u64(&mut h, 56, 0x5555);
        h[102..106].copy_from_slice(&(-1_185_000_000i32).to_le_bytes());
        h[106..110].copy_from_slice(&(339_000_000i32).to_le_bytes());
        h[110..114].copy_from_slice(&(-1_180_000_000i32).to_le_bytes());
        h[114..118].copy_from_slice(&(342_500_000i32).to_le_bytes());
        let hd = parse_header(&h).unwrap();
        assert_eq!(hd.root_offset, 0x1111);
        assert_eq!(hd.root_length, 0x2222);
        assert_eq!(hd.leaf_offset, 0x4444);
        assert_eq!(hd.tile_data_offset, 0x5555);
        assert_eq!(
            (
                hd.internal_compression,
                hd.tile_compression,
                hd.tile_type,
                hd.min_zoom,
                hd.max_zoom
            ),
            (97, 98, 99, 100, 101)
        );
        assert_eq!(hd.bounds, [-118.5, 33.9, -118.0, 34.25]);
    }

    #[test]
    fn header_shorter_than_127_bytes_is_refused() {
        let h = header_bytes();
        assert_eq!(err_text(parse_header(&h[..126])), "not a PMTiles archive");
        assert!(parse_header(&h).is_ok()); // exactly 127 is enough
    }

    #[test]
    fn header_with_wrong_magic_is_refused() {
        let mut h = header_bytes();
        h[6] = b't'; // "PMTilet"
        assert_eq!(err_text(parse_header(&h)), "not a PMTiles archive");
    }

    #[test]
    fn header_of_spec_version_2_is_refused() {
        let mut h = header_bytes();
        h[7] = 2;
        assert_eq!(
            err_text(parse_header(&h)),
            "only PMTiles spec version 3 is supported"
        );
    }

    // ---- varint ----

    #[test]
    fn varint_of_ten_bytes_decodes_u64_max() {
        let b = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        let mut p = 0;
        assert_eq!(varint(&b, &mut p).unwrap(), u64::MAX);
        assert_eq!(p, 10);
    }

    #[test]
    fn varint_longer_than_ten_bytes_is_an_error() {
        let b = [0x80u8; 11];
        assert_eq!(err_text(varint(&b, &mut 0)), "varint longer than 10 bytes");
    }

    #[test]
    fn varint_cut_short_is_an_error() {
        assert_eq!(err_text(varint(&[0x96], &mut 0)), "truncated varint");
        assert_eq!(err_text(varint(&[], &mut 0)), "truncated varint");
    }

    // ---- parse_directory ----

    #[test]
    fn directory_entry_count_larger_than_the_buffer_is_refused() {
        assert_eq!(
            err_text(parse_directory(&[4, 0, 0])),
            "directory entry count exceeds its size"
        );
    }

    #[test]
    fn directory_entry_count_equal_to_the_buffer_passes_the_size_check() {
        // 3 entries in 3 bytes cannot hold, but the count check is `>`: the failure is the read
        assert_eq!(err_text(parse_directory(&[3, 0, 0])), "truncated varint");
    }

    #[test]
    fn directory_cut_short_in_the_offsets_is_an_error() {
        // 2 entries, the second offset missing
        assert_eq!(
            err_text(parse_directory(&[2, 1, 1, 1, 1, 9, 9, 1])),
            "truncated varint"
        );
    }

    #[test]
    fn directory_ids_are_delta_coded_and_explicit_offsets_drop_the_plus_one() {
        // ids 3, 3+4=7, 7+300=307; offsets stored 1 (=0, at i=0), 51 (=50), 1 (=0, explicit at i>0)
        let d = dir_raw(&[3, 7, 307], &[1, 0, 5], &[10, 20, 30], &[1, 51, 1]);
        assert_eq!(
            parse_directory(&d).unwrap(),
            vec![ent(3, 0, 10, 1), ent(7, 50, 20, 0), ent(307, 0, 30, 5)]
        );
    }

    #[test]
    fn directory_first_offset_stored_as_zero_is_offset_zero() {
        // offset 0 on the FIRST entry has no previous entry to follow
        let d = dir_raw(&[9], &[1], &[4], &[0]);
        assert_eq!(parse_directory(&d).unwrap(), vec![ent(9, 0, 4, 1)]);
    }

    #[test]
    fn empty_directory_is_no_entries() {
        assert_eq!(parse_directory(&[0]).unwrap(), vec![]);
    }

    #[test]
    fn directory_tile_id_overflow_is_an_error_not_a_panic() {
        // ids: delta u64::MAX, then delta 1
        let mut d = vec![2];
        enc_varint(u64::MAX, &mut d);
        d.extend_from_slice(&[1, 1, 1, 1, 1, 1, 1]);
        assert!(parse_directory(&d).is_err());
    }

    #[test]
    fn directory_offset_chain_overflow_is_an_error_not_a_panic() {
        // entry 0 offset u64::MAX-1 (stored u64::MAX), length 10; entry 1 offset 0 = follows it
        let mut d = vec![2, 1, 1, 1, 1, 10, 10];
        enc_varint(u64::MAX, &mut d);
        d.push(0);
        assert!(parse_directory(&d).is_err());
    }

    // ---- find ----

    #[test]
    fn find_on_an_empty_directory_is_none() {
        assert_eq!(find(&[], 0), None);
    }

    #[test]
    fn find_before_the_first_entry_is_none() {
        assert_eq!(find(&[ent(5, 0, 1, 1)], 4), None);
    }

    #[test]
    fn find_matches_an_exact_id() {
        let d = [ent(2, 0, 1, 1), ent(5, 1, 1, 1), ent(9, 2, 1, 1)];
        assert_eq!(find(&d, 5), Some(d[1]));
    }

    #[test]
    fn find_covers_a_run_up_to_but_not_including_its_end() {
        let d = [ent(10, 0, 1, 3), ent(20, 1, 1, 1)];
        assert_eq!(find(&d, 11), Some(d[0]));
        assert_eq!(find(&d, 12), Some(d[0])); // last id of the run 10..13
        assert_eq!(find(&d, 13), None); // one past the run
        assert_eq!(find(&d, 19), None);
    }

    #[test]
    fn find_returns_a_leaf_entry_for_any_id_after_it() {
        // run_length 0 = a leaf directory: it may hold any id up to the next entry
        let d = [ent(10, 0, 1, 0), ent(1000, 1, 1, 1)];
        assert_eq!(find(&d, 10), Some(d[0]));
        assert_eq!(find(&d, 999), Some(d[0]));
        assert_eq!(find(&d, 1000), Some(d[1]));
        assert_eq!(find(&d, 1001), None);
    }

    // ---- Archive over a hand-built file ----

    #[test]
    fn archive_reads_tiles_from_root_runs_and_a_leaf() {
        let f = Scratch::new("tiles", &archive_bytes());
        let a = Archive::open(&f.0).unwrap();
        assert_eq!(a.header.tile_type, 1);
        assert_eq!(a.tile(0, 0, 0).unwrap().as_deref(), Some(T0)); // id 0
        assert_eq!(a.tile(1, 0, 0).unwrap().as_deref(), Some(T12)); // id 1, run start
        assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12)); // id 2, inside the run
        assert_eq!(a.tile(1, 1, 1).unwrap(), None); // id 3: under the leaf entry, not in the leaf
        assert_eq!(a.tile(1, 1, 0).unwrap().as_deref(), Some(T4)); // id 4, in the leaf
        assert_eq!(a.tile(2, 0, 0).unwrap().as_deref(), Some(T5)); // id 5, in the leaf
        assert_eq!(a.tile(2, 0, 1).unwrap(), None); // id 6: past the leaf's last run
    }

    /// Offsets in the header and directories are the file's word: added together they may pass
    /// `u64::MAX`, which must be an error, not a debug-build panic or a read from a wrapped offset.
    /// Found by the archive fuzz target (fuzz/regressions/archive).
    #[test]
    fn offsets_that_overflow_are_an_error_not_a_panic() {
        let mut b = archive_bytes();
        put_u64(&mut b, 56, u64::MAX - 1); // tile data offset
        let f = Scratch::new("data-overflow", &b);
        let a = Archive::open(&f.0).unwrap();
        assert!(err_text(a.tile(1, 0, 0)).contains("overflows"));

        // the leaf's entry in the root has offset 0, so here the sum fits and the read itself
        // fails (past the end of any file): an error either way, and root tiles still read
        let mut b = archive_bytes();
        put_u64(&mut b, 40, u64::MAX - 1); // leaf directory offset
        let f = Scratch::new("leaf-far", &b);
        let a = Archive::open(&f.0).unwrap();
        assert_eq!(a.tile(0, 0, 0).unwrap().as_deref(), Some(T0));
        assert!(a.tile(1, 1, 0).is_err()); // id 4, in the leaf
        assert!(a.all_tiles().is_err());

        assert!(err_text(offset_in(u64::MAX - 1, 2, "leaf directory")).contains("overflows"));
        assert_eq!(
            offset_in(u64::MAX - 2, 2, "leaf directory").unwrap(),
            u64::MAX
        );
    }

    /// Zoom 31 is the deepest a u64 tile id reaches; ids past it come only from a corrupt
    /// directory (found by the `pmtiles_dir` fuzz target).
    #[test]
    fn tile_ids_past_the_last_zoom_have_no_tile() {
        let last = zxy_to_tile_id(31, (1 << 31) - 1, 0);
        assert_eq!(tile_id_to_zxy(last).map(|t| t.0), Some(31));
        assert_eq!(tile_id_to_zxy(u64::MAX), None);
    }

    #[test]
    fn a_run_reaching_past_the_last_tile_id_is_not_a_panic() {
        let run = [ent(u64::MAX - 1, 0, 1, 5)];
        assert_eq!(find(&run, u64::MAX), Some(run[0]));
        assert_eq!(find(&run, u64::MAX - 2), None);
    }

    #[test]
    fn archive_fetches_a_leaf_once_and_counts_every_round_trip() {
        let f = Scratch::new("reads", &archive_bytes());
        let a = Archive::open(&f.0).unwrap();
        let reads = || a.reads.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reads(), 0);
        a.tile(1, 1, 0).unwrap(); // leaf + tile
        assert_eq!(reads(), 2);
        a.tile(2, 0, 0).unwrap(); // tile only: the leaf is cached
        assert_eq!(reads(), 3);
        a.tile(0, 0, 0).unwrap(); // root tile
        assert_eq!(reads(), 4);
        assert_eq!(a.leaves.lock().unwrap().len(), 1);
    }

    #[test]
    fn archive_lists_every_addressed_tile_in_id_order() {
        let f = Scratch::new("all", &archive_bytes());
        let a = Archive::open(&f.0).unwrap();
        assert_eq!(
            a.all_tiles().unwrap(),
            vec![(0, 0, 0), (1, 0, 0), (1, 0, 1), (1, 1, 0), (2, 0, 0)]
        );
    }

    #[test]
    fn archive_with_uncompressed_directories_is_refused() {
        let mut b = archive_bytes();
        b[97] = 1; // internal compression: none
        let f = Scratch::new("nocomp", &b);
        assert_eq!(
            err_text(Archive::open(&f.0)),
            "this reader handles gzip-compressed archives only"
        );
        let mut b = archive_bytes();
        b[98] = 3; // tile compression: brotli
        let f = Scratch::new("brotli", &b);
        assert_eq!(
            err_text(Archive::open(&f.0)),
            "this reader handles gzip-compressed archives only"
        );
    }

    #[test]
    fn archive_with_leaves_nested_five_deep_stops_at_four() {
        // root -> leaf1 -> leaf2 -> leaf3 -> leaf4 (empty); laid out leaf4 first, each leaf entry
        // pointing at the one before it in the section
        let mut section = dir_gz(&[]);
        let mut prev = (0u64, section.len() as u32);
        for _ in 0..3 {
            let l = dir_gz(&[ent(0, prev.0, prev.1, 0)]);
            prev = (section.len() as u64, l.len() as u32);
            section.extend_from_slice(&l);
        }
        let root = dir_gz(&[ent(0, prev.0, prev.1, 0)]);
        let mut h = header_bytes();
        put_u64(&mut h, 8, 127);
        put_u64(&mut h, 16, root.len() as u64);
        put_u64(&mut h, 40, 127 + root.len() as u64);
        h[97] = 2;
        h[98] = 2;
        let mut b = h;
        b.extend_from_slice(&root);
        b.extend_from_slice(&section);
        let f = Scratch::new("deep", &b);
        let a = Archive::open(&f.0).unwrap();
        assert_eq!(err_text(a.tile(0, 0, 0)), "directory nesting deeper than 4");
    }

    #[test]
    fn archive_file_shorter_than_its_header_is_an_io_error() {
        let f = Scratch::new("short", b"PMTiles\x03");
        let e = Archive::open(&f.0)
            .err()
            .expect("a 8-byte file must not open");
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn real_basemap_every_addressed_tile_reads_and_decodes() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/basemap.pmtiles");
        if !p.exists() {
            eprintln!("skipped: no data/basemap.pmtiles (tools/fetch-data.sh)");
            return;
        }
        let a = Archive::open(&p).unwrap();
        assert_eq!((a.header.tile_type, a.header.tile_compression), (1, 2));
        let all = a.all_tiles().unwrap();
        assert!(!all.is_empty());
        for &(z, x, y) in &all {
            assert!(
                (a.header.min_zoom..=a.header.max_zoom).contains(&z),
                "{z}/{x}/{y}"
            );
            let t = a
                .tile(z, x, y)
                .unwrap()
                .unwrap_or_else(|| panic!("{z}/{x}/{y} listed but absent"));
            crate::mvt::decode(&t).unwrap_or_else(|e| panic!("{z}/{x}/{y}: {e}"));
        }
    }

    // ---- the HTTP source, against a loopback server with canned responses ----

    #[cfg(feature = "http")]
    mod http {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        /// The inclusive (start, end) of a `Range: bytes=a-b` header.
        fn range_of(head: &str) -> Option<(usize, usize)> {
            let line = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("range:"))?;
            let (a, b) = line.split_once('=')?.1.trim().split_once('-')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        }

        /// Serve on 127.0.0.1:0 forever, one request per connection; `respond` gets the request
        /// head and returns (status line, body). Returns the URL and the Range headers seen.
        fn serve(
            respond: impl Fn(&str) -> (&'static str, Vec<u8>) + Send + 'static,
        ) -> (String, Arc<Mutex<Vec<String>>>) {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/a.pmtiles", l.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { continue };
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        head.push_str(&line);
                    }
                    if let Some((a, b)) = range_of(&head) {
                        log.lock().unwrap().push(format!("bytes={a}-{b}"));
                    }
                    let (status, body) = respond(&head);
                    let _ = write!(
                        s,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(&body);
                }
            });
            (url, seen)
        }

        /// Answers each Range request with exactly the asked-for slice of `bytes`.
        fn ranged(bytes: Vec<u8>) -> impl Fn(&str) -> (&'static str, Vec<u8>) {
            move |head| match range_of(head) {
                Some((a, b)) => ("206 Partial Content", bytes[a..=b].to_vec()),
                None => ("200 OK", bytes.clone()),
            }
        }

        #[test]
        fn http_archive_reads_tiles_with_inclusive_range_requests() {
            let bytes = archive_bytes();
            let root_len = u64le(&bytes, 16) as usize;
            let (url, seen) = serve(ranged(bytes));
            let a = Archive::open_url(&url).unwrap();
            assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(a.tile(2, 0, 0).unwrap().as_deref(), Some(T5));
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0], "bytes=0-126"); // the header: 127 bytes, end inclusive
            assert_eq!(seen[1], format!("bytes=127-{}", 127 + root_len - 1));
            assert_eq!(seen.len(), 5); // header, root, tile, leaf, tile
        }

        /// Stored ranges by (offset, length).
        type Ranges = Arc<Mutex<HashMap<(u64, usize), Vec<u8>>>>;
        /// Each request's (Range, If-Range), as the test server saw it.
        type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

        /// A `RangeCache` in memory, shared with the test through clones; counts gets that hit.
        #[derive(Clone, Default)]
        struct MemCache {
            map: Ranges,
            hits: Arc<Mutex<usize>>,
        }

        impl RangeCache for MemCache {
            fn get(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
                let v = self.map.lock().unwrap().get(&(offset, len)).cloned();
                if v.is_some() {
                    *self.hits.lock().unwrap() += 1;
                }
                v
            }
            fn put(&self, offset: u64, bytes: &[u8]) {
                self.map
                    .lock()
                    .unwrap()
                    .insert((offset, bytes.len()), bytes.to_vec());
            }
        }

        /// Serves `bytes` like nginx: an ETag on every answer (None: no ETag), and a 200 with the
        /// whole body when a request's If-Range is not the current ETag. Change `etag` to replace
        /// the archive. Returns the URL and each request's (Range, If-Range).
        fn serve_versioned(bytes: Vec<u8>, etag: Arc<Mutex<Option<String>>>) -> (String, Requests) {
            serve_versioned_with(bytes, etag, true)
        }

        /// As `serve_versioned`; with `honour_if_range` false, a server that ignores If-Range and
        /// always answers 206 with its current ETag.
        fn serve_versioned_with(
            bytes: Vec<u8>,
            etag: Arc<Mutex<Option<String>>>,
            honour_if_range: bool,
        ) -> (String, Requests) {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/a.pmtiles", l.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { continue };
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        head.push_str(&line);
                    }
                    let if_range = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("if-range:"))
                        .map(|l| l.split_once(':').unwrap().1.trim().to_owned());
                    let (a, b) = range_of(&head).unwrap();
                    log.lock()
                        .unwrap()
                        .push((format!("bytes={a}-{b}"), if_range.clone()));
                    let tag = etag.lock().unwrap().clone();
                    let whole = honour_if_range && if_range.is_some() && if_range != tag;
                    let (status, body) = if whole {
                        ("200 OK", bytes.clone())
                    } else {
                        ("206 Partial Content", bytes[a..=b].to_vec())
                    };
                    let etag_line = tag.map(|t| format!("ETag: {t}\r\n")).unwrap_or_default();
                    let _ = write!(
                        s,
                        "HTTP/1.1 {status}\r\n{etag_line}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(&body);
                }
            });
            (url, seen)
        }

        fn tag(t: &str) -> Arc<Mutex<Option<String>>> {
            Arc::new(Mutex::new(Some(t.to_owned())))
        }

        fn with(
            cache: &MemCache,
            version: &Arc<Mutex<String>>,
        ) -> impl FnOnce(&str) -> io::Result<Box<dyn RangeCache>> {
            let (cache, version) = (cache.clone(), version.clone());
            move |v| {
                *version.lock().unwrap() = v.to_owned();
                Ok(Box::new(cache))
            }
        }

        #[test]
        fn cached_archive_reads_a_second_time_from_the_cache_with_only_the_header_from_the_network()
        {
            let etag = tag("\"v1\"");
            let (url, seen) = serve_versioned(archive_bytes(), etag);
            let cache = MemCache::default();
            let version = Arc::new(Mutex::new(String::new()));
            let a = Archive::open_url_cached(&url, with(&cache, &version)).unwrap();
            assert_eq!(*version.lock().unwrap(), "\"v1\"");
            assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(a.tile(2, 0, 0).unwrap().as_deref(), Some(T5));
            let first = seen.lock().unwrap().len();
            assert_eq!(first, 5); // header, root, tile, leaf, tile: as without a cache
            assert_eq!(a.cache_hits.load(std::sync::atomic::Ordering::Relaxed), 0);

            let b = Archive::open_url_cached(&url, with(&cache, &version)).unwrap();
            assert_eq!(b.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(b.tile(2, 0, 0).unwrap().as_deref(), Some(T5));
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), first + 1, "{seen:?}");
            assert_eq!(seen[first], ("bytes=0-126".to_owned(), None)); // the header asks without If-Range
            assert_eq!(b.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
            assert_eq!(b.cache_hits.load(std::sync::atomic::Ordering::Relaxed), 3);
            // tile, leaf, tile
        }

        #[test]
        fn cached_archive_sends_the_version_in_if_range_after_the_header() {
            let (url, seen) = serve_versioned(archive_bytes(), tag("\"v1\""));
            let cache = MemCache::default();
            let a = Archive::open_url_cached(&url, with(&cache, &Default::default())).unwrap();
            a.tile(0, 0, 0).unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0].1, None);
            assert!(
                seen[1..]
                    .iter()
                    .all(|(_, ir)| ir.as_deref() == Some("\"v1\"")),
                "{seen:?}"
            );
        }

        #[test]
        fn an_archive_replaced_mid_session_is_refused_not_mixed() {
            let etag = tag("\"v1\"");
            let (url, _) = serve_versioned(archive_bytes(), etag.clone());
            let a = Archive::open_url_cached(&url, with(&MemCache::default(), &Default::default()))
                .unwrap();
            *etag.lock().unwrap() = Some("\"v2\"".to_owned());
            let e = err_text(a.tile(1, 0, 1));
            assert!(
                e.starts_with("expected 206 Partial Content for bytes=")
                    && e.ends_with(", got 200 OK"),
                "{e}"
            );
        }

        #[test]
        fn a_server_that_ignores_if_range_cannot_mix_versions_into_the_cache() {
            let etag = tag("\"v1\"");
            let (url, _) = serve_versioned_with(archive_bytes(), etag.clone(), false);
            let cache = MemCache::default();
            let a = Archive::open_url_cached(&url, with(&cache, &Default::default())).unwrap();
            let stored = cache.map.lock().unwrap().len();
            *etag.lock().unwrap() = Some("\"v2\"".to_owned());
            assert_eq!(
                err_text(a.tile(1, 0, 1)),
                "the archive changed on the server (ETag \"v2\", opened as \"v1\")"
            );
            assert_eq!(
                cache.map.lock().unwrap().len(),
                stored,
                "nothing of v2 was cached"
            );
        }

        #[test]
        fn a_weak_etag_names_the_version_but_is_not_sent_in_if_range() {
            let (url, seen) = serve_versioned(archive_bytes(), tag("W/\"v1\""));
            let version = Arc::new(Mutex::new(String::new()));
            let a = Archive::open_url_cached(&url, with(&MemCache::default(), &version)).unwrap();
            assert_eq!(a.tile(2, 0, 0).unwrap().as_deref(), Some(T5));
            assert_eq!(*version.lock().unwrap(), "W/\"v1\"");
            assert!(seen.lock().unwrap().iter().all(|(_, ir)| ir.is_none()));
        }

        #[test]
        fn without_an_etag_the_version_is_the_header_hash_and_no_if_range_is_sent() {
            let bytes = archive_bytes();
            let want = header_version(&bytes[..127]);
            let (url, seen) = serve_versioned(bytes, Arc::new(Mutex::new(None)));
            let version = Arc::new(Mutex::new(String::new()));
            let a = Archive::open_url_cached(&url, with(&MemCache::default(), &version)).unwrap();
            assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(*version.lock().unwrap(), want);
            assert!(want.starts_with('h') && want.len() == 17, "{want}");
            assert!(seen.lock().unwrap().iter().all(|(_, ir)| ir.is_none()));
        }

        #[test]
        fn header_version_changes_with_any_header_byte() {
            let mut h = archive_bytes()[..127].to_vec();
            let v = header_version(&h);
            h[100] ^= 1;
            assert_ne!(header_version(&h), v);
        }

        #[test]
        fn offline_archive_reads_what_was_seen_and_nothing_else() {
            let (url, _) = serve_versioned(archive_bytes(), tag("\"v1\""));
            let cache = MemCache::default();
            let a = Archive::open_url_cached(&url, with(&cache, &Default::default())).unwrap();
            assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            drop(a);
            let off = Archive::open_cached(Box::new(cache.clone())).unwrap();
            assert!(off.offline());
            assert_eq!(off.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(off.tile(0, 0, 0).unwrap(), None); // in the archive, never fetched
            assert_eq!(off.tile(2, 0, 0).unwrap(), None); // its leaf never fetched either
            assert_eq!(off.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
        }

        #[test]
        fn offline_archive_with_an_empty_cache_does_not_open() {
            let e = Archive::open_cached(Box::new(MemCache::default()))
                .err()
                .unwrap();
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
            assert_eq!(e.to_string(), "127 bytes at 0 are not in the offline cache");
        }

        #[test]
        fn a_cached_range_of_the_wrong_length_is_fetched_again() {
            let (url, seen) = serve_versioned(archive_bytes(), tag("\"v1\""));
            let cache = MemCache::default();
            let a = Archive::open_url_cached(&url, with(&cache, &Default::default())).unwrap();
            a.tile(1, 0, 1).unwrap();
            // corrupt every stored range by one byte, keeping its key
            for v in cache.map.lock().unwrap().values_mut() {
                v.pop();
            }
            let n = seen.lock().unwrap().len();
            assert_eq!(a.tile(1, 0, 1).unwrap().as_deref(), Some(T12));
            assert_eq!(seen.lock().unwrap().len(), n + 1);
        }

        #[test]
        fn a_file_archive_counts_reads_not_cache_hits() {
            let s = Scratch::new("counts", &archive_bytes());
            let a = Archive::open(&s.0).unwrap();
            a.tile(2, 0, 0).unwrap();
            assert!(!a.offline());
            assert_eq!(a.reads.load(std::sync::atomic::Ordering::Relaxed), 2); // leaf, tile
            assert_eq!(a.cache_hits.load(std::sync::atomic::Ordering::Relaxed), 0);
        }

        #[test]
        fn http_refuses_a_200_with_the_whole_body() {
            let bytes = archive_bytes();
            let (url, _) = serve(move |_| ("200 OK", bytes.clone()));
            assert_eq!(
                err_text(Archive::open_url(&url)),
                "expected 206 Partial Content for bytes=0-126, got 200 OK"
            );
        }

        #[test]
        fn http_refuses_a_206_one_byte_short() {
            let bytes = archive_bytes();
            let (url, _) = serve(move |_| ("206 Partial Content", bytes[..126].to_vec()));
            assert_eq!(
                err_text(Archive::open_url(&url)),
                "asked for 127 bytes at 0, got 126"
            );
        }

        #[test]
        fn http_refuses_a_206_one_byte_long() {
            let bytes = archive_bytes();
            let (url, _) = serve(move |_| ("206 Partial Content", bytes[..128].to_vec()));
            assert_eq!(
                err_text(Archive::open_url(&url)),
                "asked for 127 bytes at 0, got 128"
            );
        }

        #[test]
        fn http_refuses_a_short_tile_after_a_good_open() {
            // header and directories served right; the tile range one byte short
            let bytes = archive_bytes();
            let data_off = u64le(&bytes, 56) as usize;
            let (url, _) = serve(move |head| {
                let (a, b) = range_of(head).unwrap();
                let end = if a >= data_off { b } else { b + 1 };
                ("206 Partial Content", bytes[a..end].to_vec())
            });
            let a = Archive::open_url(&url).unwrap();
            let e = err_text(a.tile(0, 0, 0));
            assert!(
                e.starts_with(&format!(
                    "asked for {} bytes at {data_off}, got ",
                    gz(T0).len()
                )),
                "{e}"
            );
        }

        #[test]
        fn http_body_cut_off_by_a_closed_connection_is_an_error() {
            // Content-Length says 127, the server sends 50 and hangs up: fail, never a short Ok
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/a.pmtiles", l.local_addr().unwrap());
            let bytes = archive_bytes();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { continue };
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                        line.clear();
                    }
                    let _ = write!(
                        s,
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: 127\r\nConnection: close\r\n\r\n"
                    );
                    let _ = s.write_all(&bytes[..50]);
                }
            });
            assert!(Archive::open_url(&url).is_err());
        }

        #[test]
        fn http_unreachable_server_is_an_error() {
            // bind then drop: nothing listens on the port
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            assert!(Archive::open_url(&format!("http://127.0.0.1:{port}/a.pmtiles")).is_err());
        }
    }
}
