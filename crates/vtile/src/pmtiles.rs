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
    if e.tile_id == id
        || (e.run_length > 0 && id < e.tile_id + u64::from(e.run_length))
        || e.run_length == 0
    {
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
    Http {
        agent: ureq::Agent,
        url: String,
    },
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
                let mut res = agent
                    .get(url)
                    .header("Range", &range)
                    .call()
                    .map_err(io::Error::other)?;
                if res.status().as_u16() != 206 {
                    // a 200 here would be the whole planet: refuse rather than read it
                    return Err(bad(&format!(
                        "expected 206 Partial Content for {range}, got {}",
                        res.status()
                    )));
                }
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
                Ok(buf)
            }
        }
    }
}

/// Leaf directories read so far, by (offset, length). A planet archive has many; each costs a round
/// trip over the network, so a region's leaf is fetched once and kept.
const LEAF_CACHE: usize = 512;

/// Leaf directories by (offset, length) within the leaf section.
type LeafCache = HashMap<(u64, u64), Arc<Vec<Entry>>>;

pub struct Archive {
    source: Source,
    pub header: Header,
    root: Vec<Entry>,
    leaves: Mutex<LeafCache>,
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
        Self::from_source(Source::Http {
            agent,
            url: url.to_owned(),
        })
    }

    fn from_source(source: Source) -> io::Result<Self> {
        let head = source.read(0, 127)?;
        let header = parse_header(&head)?;
        if header.internal_compression != 2 || header.tile_compression != 2 {
            return Err(bad("this reader handles gzip-compressed archives only"));
        }
        let root = Self::read_dir(&source, header.root_offset, header.root_length)?;
        Ok(Archive {
            source,
            header,
            root,
            leaves: Mutex::new(HashMap::new()),
            reads: Default::default(),
        })
    }

    fn read_dir(source: &Source, offset: u64, length: u64) -> io::Result<Vec<Entry>> {
        let raw = source.read(offset, length as usize)?;
        let mut dec = Vec::new();
        gunzip(&raw, &mut dec)?;
        parse_directory(&dec)
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
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = Arc::new(Self::read_dir(
            &self.source,
            self.header.leaf_offset + offset,
            length,
        )?);
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
            let Some(e) = find(dir, id) else {
                return Ok(None);
            };
            if e.run_length == 0 {
                leaf = Some(self.leaf(e.offset, u64::from(e.length))?);
                continue;
            }
            if id >= e.tile_id + u64::from(e.run_length) {
                return Ok(None);
            }
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let raw = self
                .source
                .read(self.header.tile_data_offset + e.offset, e.length as usize)?;
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
                    stack.push(Self::read_dir(
                        &self.source,
                        self.header.leaf_offset + e.offset,
                        u64::from(e.length),
                    )?);
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
                    assert_eq!(
                        tile_id_to_zxy(zxy_to_tile_id(z, x, y)),
                        (z, x, y),
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
