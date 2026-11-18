//! A PMTiles directory as the reader sees it after decompression: the archive target cannot reach
//! here, since nearly every change to a gzipped directory fails its checksum before it is parsed.
//! Each entry found is then looked up the way a tile request would be.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vtile::pmtiles::{find, parse_directory, tile_id_to_zxy};

fuzz_target!(|data: &[u8]| {
    let Ok(entries) = parse_directory(data) else {
        return;
    };
    for e in &entries {
        let _ = find(&entries, e.tile_id);
        let _ = find(&entries, e.tile_id.saturating_add(u64::from(e.run_length)));
        let _ = tile_id_to_zxy(e.tile_id);
    }
});
