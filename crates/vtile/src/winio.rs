//! Positional file reads on Windows (see `pmtiles::read_exact_at` for Unix). Built only for
//! Windows, so Linux's mutation testing cannot reach it (`.cargo/mutants.toml`); `platforms.yml`
//! runs the archive tests on Windows instead.

use std::fs::File;
use std::io;

/// Fill `buf` from `offset` without moving a shared cursor, so threads can read one file at once:
/// `pread` on Unix, `ReadFile` with an explicit offset on Windows.
pub(crate) fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    // as pread does on Unix (off_t is signed), refuse an offset no file can reach
    let too_far = || io::Error::new(io::ErrorKind::InvalidInput, "read offset out of range");
    if i64::try_from(offset).is_err() {
        return Err(too_far());
    }
    while !buf.is_empty() {
        match f.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset = offset
                    .checked_add(n as u64)
                    .filter(|o| i64::try_from(*o).is_ok())
                    .ok_or_else(too_far)?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
