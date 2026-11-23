//! Byte-level and bit-level input primitives.
//!
//! * [`ByteReader`] reads little- and big-endian integers and untrusted
//!   length-prefixed blocks from any [`Read`] source while tracking the byte
//!   position for error reports.
//! * [`ScopedReader`] restricts a seekable source to a byte range.
//! * [`BitReaderMsb`] and [`BitReaderLsb`] read bit fields from an in-memory
//!   packet.

mod bits;
mod scoped;

use core::fmt;
use std::io::{self, Read, Seek, SeekFrom};

use crate::error::{Error, ErrorKind};

pub use bits::{BitReaderLsb, BitReaderMsb};
#[cfg(test)]
pub(crate) use scoped::CrcMonitorReader;
pub use scoped::ScopedReader;

/// A seekable byte source that can be moved to another thread.
///
/// Implemented for every `T: Read + Seek + Send`, including
/// [`std::fs::File`], [`std::io::BufReader<File>`](std::io::BufReader) and
/// [`std::io::Cursor`].
pub trait MediaSource: Read + Seek + Send {}

impl<T: Read + Seek + Send + ?Sized> MediaSource for T {}

/// Four-character code identifying a chunk, atom or box (`RIFF`, `fmt `,
/// `moov`).
///
/// `Debug` and `Display` print printable ASCII as is and other bytes as
/// `\xNN`.
///
/// ```
/// use akousma_core::io::FourCc;
///
/// const FMT: FourCc = FourCc::new(*b"fmt ");
/// assert_eq!(FMT.to_string(), "fmt ");
/// assert_eq!(FourCc::new([b'a', 0, 0xFF, b'z']).to_string(), "a\\x00\\xFFz");
/// ```
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    /// Creates a code from its four bytes in stream order.
    #[must_use]
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    /// Returns the four bytes in stream order.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &byte in &self.0 {
            if byte == b' ' || byte.is_ascii_graphic() {
                write!(f, "{}", char::from(byte))?;
            } else {
                write!(f, "\\x{byte:02X}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCc(\"{self}\")")
    }
}

/// Size of the first reservation made by [`ByteReader::read_bounded_vec`].
/// Each further step doubles the step size, so a truncated stream never makes
/// the reader reserve much more memory than the bytes it actually delivered.
const BOUNDED_VEC_FIRST_STEP: usize = 8 * 1024;

/// Reader of fixed-size integers and untrusted blocks with byte-position
/// tracking.
///
/// The reader does not buffer: in-memory sources such as
/// [`Cursor<&[u8]>`](std::io::Cursor) are read without an extra copy, and file
/// sources should be wrapped in [`std::io::BufReader`] by the caller.
///
/// Every error returned by a method carries, in
/// [`Error::byte_offset`], the position at which that method call started.
/// After an error the position still counts every byte that was consumed.
///
/// ```
/// use akousma_core::{ErrorKind, io::ByteReader};
/// use std::io::Cursor;
///
/// let mut reader = ByteReader::new(Cursor::new([0x52, 0x49, 0x46, 0x46, 0x24, 0x08, 0x00, 0x00]));
/// assert_eq!(reader.read_fourcc()?.to_string(), "RIFF");
/// assert_eq!(reader.read_u32_le()?, 0x0824);
/// assert_eq!(reader.position(), 8);
///
/// let err = reader.read_u8().unwrap_err();
/// assert_eq!(err.kind, ErrorKind::UnexpectedEof);
/// assert_eq!(err.byte_offset, Some(8));
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Debug)]
pub struct ByteReader<R> {
    inner: R,
    pos: u64,
}

impl<R> ByteReader<R> {
    /// Wraps `inner`, assuming it is positioned at byte 0.
    #[must_use]
    pub const fn new(inner: R) -> Self {
        Self { inner, pos: 0 }
    }

    /// Wraps `inner`, which is currently positioned at byte `offset` of its
    /// stream.
    #[must_use]
    pub const fn with_offset(inner: R, offset: u64) -> Self {
        Self { inner, pos: offset }
    }

    /// Returns the current byte position.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Returns a shared reference to the wrapped source.
    #[must_use]
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the wrapped source.
    ///
    /// Reading or seeking through this reference bypasses position tracking.
    pub const fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Unwraps the source.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> ByteReader<R> {
    /// Fills `buf` completely.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if the source ends first, or
    /// [`ErrorKind::Io`] if the source fails.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        let start = self.pos;
        self.fill(buf, start)
    }

    /// Reads `N` bytes into an array.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut buf = [0u8; N];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let [byte] = self.read_array()?;
        Ok(byte)
    }

    /// Reads a little-endian `u16`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u16_le(&mut self) -> Result<u16, Error> {
        self.read_array().map(u16::from_le_bytes)
    }

    /// Reads a big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u16_be(&mut self) -> Result<u16, Error> {
        self.read_array().map(u16::from_be_bytes)
    }

    /// Reads a little-endian 24-bit unsigned integer.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u24_le(&mut self) -> Result<u32, Error> {
        let [b0, b1, b2] = self.read_array()?;
        Ok(u32::from_le_bytes([b0, b1, b2, 0]))
    }

    /// Reads a big-endian 24-bit unsigned integer.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u24_be(&mut self) -> Result<u32, Error> {
        let [b0, b1, b2] = self.read_array()?;
        Ok(u32::from_be_bytes([0, b0, b1, b2]))
    }

    /// Reads a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u32_le(&mut self) -> Result<u32, Error> {
        self.read_array().map(u32::from_le_bytes)
    }

    /// Reads a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u32_be(&mut self) -> Result<u32, Error> {
        self.read_array().map(u32::from_be_bytes)
    }

    /// Reads a little-endian `u64`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u64_le(&mut self) -> Result<u64, Error> {
        self.read_array().map(u64::from_le_bytes)
    }

    /// Reads a big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_u64_be(&mut self) -> Result<u64, Error> {
        self.read_array().map(u64::from_be_bytes)
    }

    /// Reads a [`FourCc`].
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn read_fourcc(&mut self) -> Result<FourCc, Error> {
        self.read_array().map(FourCc)
    }

    /// Discards the next `count` bytes by reading them, so it also works on
    /// sources that cannot seek.
    ///
    /// # Errors
    ///
    /// Same as [`ByteReader::read_exact`].
    pub fn skip(&mut self, count: u64) -> Result<(), Error> {
        let start = self.pos;
        let mut limited = (&mut self.inner).take(count);
        let copied = io::copy(&mut limited, &mut io::sink());
        // `take` never yields more than `count` bytes, whatever `copy` reports.
        let consumed = count - limited.limit();
        self.pos = self.pos.saturating_add(consumed);
        match copied {
            Err(err) => Err(Error::at(ErrorKind::from(err.kind()), start)),
            Ok(_) if consumed < count => Err(Error::at(ErrorKind::UnexpectedEof, start)),
            Ok(_) => Ok(()),
        }
    }

    /// Reads a block of `len` bytes whose length comes from the stream.
    ///
    /// The length is checked against `max_limit` before any memory is
    /// reserved, and memory is reserved in doubling steps starting at 8 KiB as
    /// data actually arrives. A 50-byte truncated file that declares a 16 MiB
    /// block therefore fails with [`ErrorKind::UnexpectedEof`] after reserving
    /// 8 KiB. A `u64` length that does not fit in `usize` (on 32-bit targets)
    /// is rejected instead of being truncated.
    ///
    /// ```
    /// use akousma_core::{ErrorKind, FormatLimits, io::ByteReader};
    /// use std::io::Cursor;
    ///
    /// let limits = FormatLimits::new();
    /// let mut reader = ByteReader::new(Cursor::new(vec![7u8; 50]));
    /// assert_eq!(reader.read_bounded_vec(10, limits.max_packet_bytes)?, [7u8; 10]);
    ///
    /// let err = reader.read_bounded_vec(16 << 20, limits.max_packet_bytes).unwrap_err();
    /// assert_eq!(err.kind, ErrorKind::UnexpectedEof);
    ///
    /// let err = reader.read_bounded_vec(u64::MAX, limits.max_packet_bytes).unwrap_err();
    /// assert_eq!(err.kind, ErrorKind::LimitExceeded);
    /// # Ok::<(), akousma_core::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if `len > max_limit`, if `len` does not
    /// fit in `usize` or if the allocator refuses a reservation; otherwise the
    /// errors of [`ByteReader::read_exact`].
    pub fn read_bounded_vec(&mut self, len: u64, max_limit: usize) -> Result<Vec<u8>, Error> {
        let start = self.pos;
        let limit = u64::try_from(max_limit).unwrap_or(u64::MAX);
        if len > limit {
            return Err(Error::at(ErrorKind::LimitExceeded, start));
        }
        let target =
            usize::try_from(len).map_err(|_| Error::at(ErrorKind::LimitExceeded, start))?;

        let mut buf = Vec::new();
        let mut step = BOUNDED_VEC_FIRST_STEP;
        while buf.len() < target {
            let filled = buf.len();
            let want = step.min(target - filled);
            buf.try_reserve_exact(want)
                .map_err(|_| Error::at(ErrorKind::LimitExceeded, start))?;
            buf.resize(filled + want, 0);
            self.fill(&mut buf[filled..], start)?;
            step = step.saturating_mul(2);
        }
        Ok(buf)
    }

    /// Fills `buf`, reporting errors at `start`.
    fn fill(&mut self, mut buf: &mut [u8], start: u64) -> Result<(), Error> {
        while !buf.is_empty() {
            match self.inner.read(buf) {
                Ok(0) => return Err(Error::at(ErrorKind::UnexpectedEof, start)),
                Ok(n) => {
                    // A misbehaving `Read` may claim more than it was offered.
                    let n = n.min(buf.len());
                    self.pos = self.pos.saturating_add(n as u64);
                    buf = &mut buf[n..];
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(Error::at(ErrorKind::from(err.kind()), start)),
            }
        }
        Ok(())
    }
}

impl<R: Seek> ByteReader<R> {
    /// Seeks the wrapped source and returns the new byte position.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Io`] if the source rejects the seek (a seek before byte 0
    /// is reported as [`IoErrorKind::InvalidInput`](crate::IoErrorKind::InvalidInput)
    /// by standard sources).
    pub fn seek(&mut self, target: SeekFrom) -> Result<u64, Error> {
        let start = self.pos;
        let pos = self
            .inner
            .seek(target)
            .map_err(|err| Error::at(ErrorKind::from(err.kind()), start))?;
        self.pos = pos;
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::IoErrorKind;
    use crate::options::FormatLimits;
    use std::io::Cursor;

    /// Delivers `available` bytes of `0xAB`, then end of stream, and records
    /// the largest destination buffer offered by the caller.
    struct TruncatedSource {
        available: usize,
        largest_request: usize,
    }

    impl Read for TruncatedSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.largest_request = self.largest_request.max(buf.len());
            let n = buf.len().min(self.available);
            buf[..n].fill(0xAB);
            self.available -= n;
            Ok(n)
        }
    }

    #[test]
    fn bounded_vec_truncated_16mib() {
        let declared = 16 * 1024 * 1024;
        let limit = FormatLimits::DEFAULT_MAX_PACKET_BYTES;
        let mut reader = ByteReader::new(TruncatedSource {
            available: 50,
            largest_request: 0,
        });
        let err = reader.read_bounded_vec(declared, limit).unwrap_err();
        assert_eq!(err, Error::at(ErrorKind::UnexpectedEof, 0));
        assert_eq!(reader.position(), 50);
        // The zero-filled destination offered to `read` equals the reserved
        // capacity, so this bounds the memory reserved for the truncated block.
        assert_eq!(reader.get_ref().largest_request, BOUNDED_VEC_FIRST_STEP);

        // The same 50 bytes from a cursor.
        let mut reader = ByteReader::new(Cursor::new([0u8; 50]));
        let err = reader.read_bounded_vec(declared, limit).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnexpectedEof);

        // Limits are checked before any read.
        let mut reader = ByteReader::with_offset(Cursor::new([0u8; 50]), 1000);
        let err = reader
            .read_bounded_vec(limit as u64 + 1, limit)
            .unwrap_err();
        assert_eq!(err, Error::at(ErrorKind::LimitExceeded, 1000));
        let err = reader
            .read_bounded_vec(u64::from(u32::MAX), limit)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::LimitExceeded);
        assert_eq!(reader.position(), 1000);

        // Without a practical limit, a `u64::MAX` length is rejected where it
        // does not fit in `usize`, and otherwise fails at the end of the data
        // after the first reservation step.
        let mut reader = ByteReader::new(TruncatedSource {
            available: 50,
            largest_request: 0,
        });
        let err = reader.read_bounded_vec(u64::MAX, usize::MAX).unwrap_err();
        if usize::try_from(u64::MAX).is_ok() {
            assert_eq!(err.kind, ErrorKind::UnexpectedEof);
            assert_eq!(reader.get_ref().largest_request, BOUNDED_VEC_FIRST_STEP);
        } else {
            assert_eq!(err.kind, ErrorKind::LimitExceeded);
        }

        // Empty and multi-step blocks are returned in full and in order.
        assert!(reader.read_bounded_vec(0, 0).unwrap().is_empty());
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let mut reader = ByteReader::new(Cursor::new(&data));
        let block = reader.read_bounded_vec(data.len() as u64, limit).unwrap();
        assert_eq!(block, data);
        assert_eq!(reader.position(), 100_000);
    }

    #[test]
    fn byte_reader_integers_and_positions() {
        let bytes = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C,
            0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28,
        ];
        let mut r = ByteReader::new(Cursor::new(bytes));
        assert_eq!(r.read_u8().unwrap(), 0x01);
        assert_eq!(r.read_u16_le().unwrap(), 0x0302);
        assert_eq!(r.read_u16_be().unwrap(), 0x0405);
        assert_eq!(r.read_u24_le().unwrap(), 0x08_0706);
        assert_eq!(r.read_u24_be().unwrap(), 0x09_0A0B);
        assert_eq!(r.read_u32_le().unwrap(), 0x0F0E_0D0C);
        assert_eq!(r.read_u32_be().unwrap(), 0x1011_1213);
        assert_eq!(r.read_u64_le().unwrap(), 0x1B1A_1918_1716_1514);
        assert_eq!(r.read_u64_be().unwrap(), 0x1C1D_1E1F_2021_2223);
        assert_eq!(r.position(), 35);
        r.skip(1).unwrap();
        assert_eq!(
            r.read_fourcc().unwrap(),
            FourCc::new([0x25, 0x26, 0x27, 0x28])
        );
        assert_eq!(r.position(), 40);
        assert_eq!(r.seek(SeekFrom::Start(1)).unwrap(), 1);
        assert_eq!(r.read_u8().unwrap(), 0x02);
        let err = r.seek(SeekFrom::Current(-10)).unwrap_err();
        assert_eq!(err, Error::at(ErrorKind::Io(IoErrorKind::InvalidInput), 2));

        // A truncated field reports its own start and counts consumed bytes.
        let mut r = ByteReader::with_offset(Cursor::new([0xAA, 0xBB, 0xCC]), 100);
        assert_eq!(r.read_u8().unwrap(), 0xAA);
        let err = r.read_u32_be().unwrap_err();
        assert_eq!(err, Error::at(ErrorKind::UnexpectedEof, 101));
        assert_eq!(r.position(), 103);
        let err = r.skip(1).unwrap_err();
        assert_eq!(err, Error::at(ErrorKind::UnexpectedEof, 103));
    }

    #[test]
    fn fourcc_formatting() {
        assert_eq!(format!("{:?}", FourCc::new(*b"data")), "FourCc(\"data\")");
        assert_eq!(
            FourCc::new([0x00, b'A', 0x7F, 0xFE]).to_string(),
            "\\x00A\\x7F\\xFE"
        );
    }
}
