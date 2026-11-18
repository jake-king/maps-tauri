//! A vector tile decoded and meshed, the native renderer's whole path for a tile it did not make.
//! The first byte picks the zoom.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vtile::{mesh::Mesher, mvt};

fuzz_target!(|data: &[u8]| {
    let Some((&z, tile)) = data.split_first() else {
        return;
    };
    if let Ok(layers) = mvt::decode(tile) {
        let _ = Mesher::default().build(&layers, z % 16);
    }
});
