//! PMTiles v3: a 127-byte header, a root directory, optional leaf directories, then tile data.
//! Directories are varint-encoded and (here) gzip-compressed; tiles are addressed by a Hilbert-curve
//! tile id. Spec: https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use flate2::read::GzDecoder;

#[derive(Debug, Clone)]
pub struct Header {
    pub root_offset: u64,
    pub root_length: u64,
    pub leaf_offset: u64,
    pub tile_data_offset: u64,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub tile_type: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub bounds: [f64; 4], // min lon, min lat, max lon, max lat
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
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

/// A decompressed directory: entry count, then ids (delta), run lengths, lengths, offsets (0 means
/// "right after the previous entry").
pub fn parse_directory(b: &[u8]) -> io::Result<Vec<Entry>> {
    let mut p = 0;
    let n = varint(b, &mut p)? as usize;
    if n > b.len() {
        return Err(bad("directory entry count exceeds its size"));
    }
    let mut e = vec![Entry { tile_id: 0, offset: 0, length: 0, run_length: 0 }; n];
    let mut last = 0u64;
    for x in e.iter_mut() {
        last += varint(b, &mut p)?;
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
        e[i].offset = if v == 0 && i > 0 { e[i - 1].offset + u64::from(e[i - 1].length) } else { v.saturating_sub(1) };
    }
    Ok(e)
}

/// The Hilbert tile id of z/x/y, as the spec defines it.
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

/// The entry covering `id`: an exact match, or a run that started before it.
pub fn find(entries: &[Entry], id: u64) -> Option<Entry> {
    let i = entries.partition_point(|e| e.tile_id <= id);
    let e = *entries.get(i.checked_sub(1)?)?;
    if e.tile_id == id || (e.run_length > 0 && id < e.tile_id + u64::from(e.run_length)) || e.run_length == 0 {
        // run_length 0 marks a leaf directory, which may hold the id
        Some(e)
    } else {
        None
    }
}

pub fn gunzip(b: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
    out.clear();
    GzDecoder::new(b).read_to_end(out)?;
    Ok(())
}

/// Where an archive's bytes come from: a local file (pread), or a URL (HTTP Range requests over one
/// reused connection pool).
enum Source {
    File(File),
    #[cfg(feature = "http")]
    Http { agent: ureq::Agent, url: String },
}

impl Source {
    fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        match self {
            Source::File(f) => {
                let mut buf = vec![0u8; len];
                f.read_exact_at(&mut buf, offset)?;
                Ok(buf)
            }
            #[cfg(feature = "http")]
            Source::Http { agent, url } => {
                if len == 0 {
                    return Ok(Vec::new());
                }
                let range = format!("bytes={}-{}", offset, offset + len as u64 - 1);
                let mut res = agent.get(url).header("Range", &range).call().map_err(io::Error::other)?;
                if res.status().as_u16() != 206 {
                    // a 200 here would be the whole planet: refuse rather than read it
                    return Err(bad(&format!("expected 206 Partial Content for {range}, got {}", res.status())));
                }
                let buf = res.body_mut().with_config().limit(64 << 20).read_to_vec().map_err(io::Error::other)?;
                if buf.len() != len {
                    return Err(bad(&format!("asked for {len} bytes at {offset}, got {}", buf.len())));
                }
                Ok(buf)
            }
        }
    }
}

/// Leaf directories read so far, by (offset, length). A planet archive has many; each costs a round
/// trip over the network, so a region's leaf is fetched once and kept.
const LEAF_CACHE: usize = 512;

pub struct Archive {
    source: Source,
    pub header: Header,
    root: Vec<Entry>,
    leaves: Mutex<HashMap<(u64, u64), Arc<Vec<Entry>>>>,
    /// round trips made (header/root, leaves, tiles): for measuring a remote archive
    pub reads: std::sync::atomic::AtomicU64,
}

impl Archive {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_source(Source::File(File::open(path)?))
    }

    /// An archive served over HTTP(S) with Range support, e.g. a planet on the LAN.
    #[cfg(feature = "http")]
    pub fn open_url(url: &str) -> io::Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false) // a non-206 is reported by Source::read
            .build()
            .into();
        Self::from_source(Source::Http { agent, url: url.to_owned() })
    }

    fn from_source(source: Source) -> io::Result<Self> {
        let head = source.read(0, 127)?;
        let header = parse_header(&head)?;
        if header.internal_compression != 2 || header.tile_compression != 2 {
            return Err(bad("this reader handles gzip-compressed archives only"));
        }
        let root = Self::read_dir(&source, header.root_offset, header.root_length)?;
        Ok(Archive { source, header, root, leaves: Mutex::new(HashMap::new()), reads: Default::default() })
    }

    fn read_dir(source: &Source, offset: u64, length: u64) -> io::Result<Vec<Entry>> {
        let raw = source.read(offset, length as usize)?;
        let mut dec = Vec::new();
        gunzip(&raw, &mut dec)?;
        parse_directory(&dec)
    }

    fn leaf(&self, offset: u64, length: u64) -> io::Result<Arc<Vec<Entry>>> {
        if let Some(l) = self.leaves.lock().expect("leaf cache").get(&(offset, length)) {
            return Ok(l.clone());
        }
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = Arc::new(Self::read_dir(&self.source, self.header.leaf_offset + offset, length)?);
        let mut c = self.leaves.lock().expect("leaf cache");
        if c.len() >= LEAF_CACHE {
            c.clear(); // crude, but bounded: refill on demand
        }
        c.insert((offset, length), dir.clone());
        Ok(dir)
    }

    /// The decompressed tile at z/x/y, or None if the archive does not have it.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> io::Result<Option<Vec<u8>>> {
        let id = zxy_to_tile_id(z, x, y);
        let mut leaf: Option<Arc<Vec<Entry>>> = None;
        for _depth in 0..4 {
            let dir: &[Entry] = leaf.as_deref().map_or(&self.root, |v| v);
            let Some(e) = find(dir, id) else { return Ok(None) };
            if e.run_length == 0 {
                leaf = Some(self.leaf(e.offset, u64::from(e.length))?);
                continue;
            }
            if id >= e.tile_id + u64::from(e.run_length) {
                return Ok(None);
            }
            self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let raw = self.source.read(self.header.tile_data_offset + e.offset, e.length as usize)?;
            let mut out = Vec::new();
            gunzip(&raw, &mut out)?;
            return Ok(Some(out));
        }
        Err(bad("directory nesting deeper than 4"))
    }

    /// Every (z, x, y) the archive addresses, in tile-id order: for whole-archive tests.
    pub fn all_tiles(&self) -> io::Result<Vec<(u8, u32, u32)>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for e in dir {
                if e.run_length == 0 {
                    stack.push(Self::read_dir(&self.source, self.header.leaf_offset + e.offset, u64::from(e.length))?);
                } else {
                    for id in e.tile_id..e.tile_id + u64::from(e.run_length) {
                        out.push(tile_id_to_zxy(id));
                    }
                }
            }
        }
        out.sort_by_key(|&(z, x, y)| zxy_to_tile_id(z, x, y));
        Ok(out)
    }
}

/// The inverse of [`zxy_to_tile_id`].
pub fn tile_id_to_zxy(id: u64) -> (u8, u32, u32) {
    let mut acc = 0u64;
    let mut z = 0u8;
    loop {
        let n = 1u64 << (2 * z as u64);
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
    (z, x as u32, y as u32)
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
                    assert_eq!(tile_id_to_zxy(zxy_to_tile_id(z, x, y)), (z, x, y), "{z}/{x}/{y}");
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
        assert_eq!(e[0], Entry { tile_id: 5, offset: 100, length: 10, run_length: 1 });
        assert_eq!(e[1], Entry { tile_id: 7, offset: 110, length: 20, run_length: 1 });
    }
}
