//! A PMTiles archive from untrusted bytes (a planet read over HTTP is someone else's file): open it,
//! then read tiles, through the same header, directory, leaf and gzip path the app uses.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vtile::pmtiles::{Archive, RangeCache};

struct Bytes(Vec<u8>);

impl RangeCache for Bytes {
    fn get(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(offset).ok()?;
        self.0
            .get(start..start.checked_add(len)?)
            .map(<[u8]>::to_vec)
    }
    fn put(&self, _: u64, _: &[u8]) {}
}

fuzz_target!(|data: &[u8]| {
    let Ok(a) = Archive::open_cached(Box::new(Bytes(data.to_vec()))) else {
        return;
    };
    // the tiles a viewer asks for first, and a few deeper ones
    for (z, x, y) in [
        (0, 0, 0),
        (1, 0, 1),
        (2, 1, 1),
        (8, 44, 102),
        (14, 2800, 6540),
    ] {
        let _ = a.tile(z, x, y);
    }
});
