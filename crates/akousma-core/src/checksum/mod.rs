//! Outcome of a whole-stream integrity check.
//!
//! The CRC-8, CRC-16, CRC-32 and MD5 implementations are internal to the
//! crate and are not part of the public API until the formats that need them
//! (FLAC, MPEG) are supported.

#[allow(dead_code)]
mod crc;
#[allow(dead_code)]
mod md5;

/// A checksum that consumes bytes incrementally.
///
/// Feeding a message in several calls gives the same result as one call with
/// the whole message. `CrcMonitorReader` uses
/// this trait to checksum bytes as they are read.
#[allow(dead_code)]
pub(crate) trait Checksum {
    /// Feeds `bytes` into the checksum.
    fn update(&mut self, bytes: &[u8]);
}

/// Outcome of a whole-stream integrity check that did not fail.
///
/// A mismatch is reported as
/// [`ErrorKind::ChecksumMismatch`](crate::ErrorKind::ChecksumMismatch) instead.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum IntegrityStatus {
    /// The stream stores a checksum and the decoded data matches it.
    VerifiedOk,
    /// The format or this particular stream stores no whole-stream checksum.
    NotPresent,
}
