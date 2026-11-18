//! A compressed radar frame, inflated under the app's size cap.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(out) = wx::radar::gunzip(data, 64 << 20) {
        assert!(out.len() <= 64 << 20);
    }
});
