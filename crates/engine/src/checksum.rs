//! BLAKE3 hashing helpers used for streaming integrity checks and the
//! post-copy `--verify` pass.
//!
//! Two flavors:
//!  - [`hash_reader`]: one-shot BLAKE3 over a `Read`.
//!  - [`HashingWriter`]: a `Write` adapter that updates a BLAKE3 hasher as
//!    bytes flow through it. The transfer loop uses this to checksum
//!    while it copies, with zero extra passes over the data.

use std::io::{Read, Write};

use blake3::Hasher;

const CHUNK: usize = 64 * 1024;

/// Hash a reader end-to-end and return the lowercase hex digest.
pub fn hash_reader<R: Read>(mut reader: R) -> std::io::Result<String> {
    let mut hasher = Hasher::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// A `Write` adapter that feeds every byte through a BLAKE3 hasher.
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Hasher,
    bytes: u64,
}

impl<W: Write> std::fmt::Debug for HashingWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HashingWriter")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Hasher::new(),
            bytes: 0,
        }
    }

    pub fn bytes_through(&self) -> u64 {
        self.bytes
    }

    pub fn finalize_hex(self) -> (W, String) {
        let hex = self.hasher.finalize().to_hex().to_string();
        (self.inner, hex)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hash_matches_known_vector() {
        // BLAKE3 of "abc"
        let want = "6437b3ac38465133ffb63b75273a89dbd548d4e1a7f6f5d2eb8aed8f1c3a5a8d";
        // sanity: actually use blake3 directly to make sure the crate version
        // is hooked up.
        let direct = blake3::hash(b"abc").to_hex().to_string();
        assert_eq!(direct, want);

        let got = hash_reader(Cursor::new(b"abc".to_vec())).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn hashing_writer_agrees_with_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i & 0xff) as u8).collect();
        let one_shot = hash_reader(Cursor::new(data.clone())).unwrap();

        let mut sink = Vec::new();
        let mut hw = HashingWriter::new(&mut sink);
        hw.write_all(&data).unwrap();
        let (_, streamed) = hw.finalize_hex();

        assert_eq!(one_shot, streamed);
        assert_eq!(sink.len(), data.len());
    }
}
