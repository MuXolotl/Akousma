//! Reader adapters: byte-range restriction and on-the-fly checksumming.

use std::io::{self, Read, Seek, SeekFrom};

use crate::checksum::Checksum;

/// Restricts a seekable source to the half-open byte range `start..end`.
///
/// Positions are local to the range: local position `0` is byte `start` of the
/// source. Reads at or past the local end return `Ok(0)`, so a demuxer cannot
/// run into trailing tags (APEv2, ID3v1) located after `end`. Seeking before
/// local position `0` fails with [`io::ErrorKind::InvalidInput`]; seeking past
/// the end is allowed and subsequent reads return `Ok(0)`.
///
/// The adapter assumes exclusive use of the source: it keeps the source
/// positioned at `start` plus the local position.
///
/// ```
/// use akousma_core::io::ScopedReader;
/// use std::io::{Cursor, Read, Seek, SeekFrom};
///
/// let file = Cursor::new(b"ID3audioAPETAGEX".to_vec());
/// let mut audio = ScopedReader::new(file, 3, 8)?;
/// let mut bytes = Vec::new();
/// audio.read_to_end(&mut bytes)?;
/// assert_eq!(bytes, b"audio");
///
/// assert_eq!(audio.seek(SeekFrom::End(-2))?, 3);
/// assert!(audio.seek(SeekFrom::Current(-4)).is_err());
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct ScopedReader<R> {
    inner: R,
    start: u64,
    end: u64,
    pos: u64,
}

impl<R: Seek> ScopedReader<R> {
    /// Restricts `inner` to `start..end` and seeks it to `start`.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] if `start > end`, or any error of the
    /// initial seek.
    pub fn new(mut inner: R, start: u64, end: u64) -> io::Result<Self> {
        if start > end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scope start is after scope end",
            ));
        }
        inner.seek(SeekFrom::Start(start))?;
        Ok(Self {
            inner,
            start,
            end,
            pos: 0,
        })
    }
}

impl<R> ScopedReader<R> {
    /// Source position of local position `0`.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Source position one past the last readable byte.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    /// Returns a shared reference to the wrapped source.
    #[must_use]
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Unwraps the source. Its position is `start` plus the local position.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    const fn scope_len(&self) -> u64 {
        self.end - self.start
    }
}

impl<R: Read> Read for ScopedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.scope_len().saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let max = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        let n = self.inner.read(&mut buf[..max])?;
        // A misbehaving `Read` may claim more than it was offered.
        let n = n.min(max);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for ScopedReader<R> {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let local = match target {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.scope_len().checked_add_signed(delta),
        };
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside the representable scope range",
            )
        };
        let local = local.ok_or_else(invalid)?;
        let absolute = self.start.checked_add(local).ok_or_else(invalid)?;
        self.inner.seek(SeekFrom::Start(absolute))?;
        self.pos = local;
        Ok(local)
    }
}

/// Passes every byte read from the source through a running [`Checksum`].
///
/// Used to verify frame header and footer CRCs while the fields are parsed,
/// without a second pass over the bytes. Only bytes actually returned by
/// [`Read::read`] are fed to the checksum. Internal until the formats that
/// need it (FLAC, MPEG) are supported.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CrcMonitorReader<R, C> {
    inner: R,
    checksum: C,
}

#[allow(dead_code)]
impl<R, C> CrcMonitorReader<R, C> {
    /// Wraps `inner`, feeding read bytes into `checksum`.
    #[must_use]
    pub const fn new(inner: R, checksum: C) -> Self {
        Self { inner, checksum }
    }

    /// Returns the running checksum.
    #[must_use]
    pub const fn checksum(&self) -> &C {
        &self.checksum
    }

    /// Replaces the running checksum and returns the previous one.
    pub fn replace_checksum(&mut self, checksum: C) -> C {
        core::mem::replace(&mut self.checksum, checksum)
    }

    /// Returns a shared reference to the wrapped source.
    #[must_use]
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Unwraps the source and the running checksum.
    #[must_use]
    pub fn into_parts(self) -> (R, C) {
        (self.inner, self.checksum)
    }
}

impl<R: Read, C: Checksum> Read for CrcMonitorReader<R, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let n = n.min(buf.len());
        self.checksum.update(&buf[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FILE: &[u8] = b"ID3hdrAUDIODATAAPETAGEX-trailer";
    const AUDIO_START: u64 = 6;
    const AUDIO_END: u64 = 15;

    fn read_all<R: Read>(reader: &mut R) -> Vec<u8> {
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn scoped_reader_boundaries() {
        let mut r = ScopedReader::new(Cursor::new(FILE), AUDIO_START, AUDIO_END).unwrap();
        assert_eq!(read_all(&mut r), b"AUDIODATA");
        // At the end: clean EOF even though `APETAGEX` follows in the source.
        let mut byte = [0u8; 1];
        assert_eq!(r.read(&mut byte).unwrap(), 0);
        assert_eq!(r.stream_position().unwrap(), 9);

        assert_eq!(r.seek(SeekFrom::Start(0)).unwrap(), 0);
        let mut five = [0u8; 5];
        r.read_exact(&mut five).unwrap();
        assert_eq!(&five, b"AUDIO");

        // Relative seeks resolve against the scope, not the source.
        assert_eq!(r.seek(SeekFrom::End(-4)).unwrap(), 5);
        assert_eq!(read_all(&mut r), b"DATA");
        assert_eq!(r.seek(SeekFrom::Current(-9)).unwrap(), 0);

        // Left of `start` is rejected and the position is unchanged.
        let err = r.seek(SeekFrom::Current(-1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = r.seek(SeekFrom::End(-10)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(r.read(&mut byte).unwrap(), 1);
        assert_eq!(&byte, b"A");

        // Past the end: the seek succeeds, reads return nothing.
        assert_eq!(r.seek(SeekFrom::Start(100)).unwrap(), 100);
        assert_eq!(r.read(&mut byte).unwrap(), 0);
        assert_eq!(r.seek(SeekFrom::End(0)).unwrap(), 9);
        assert_eq!(r.read(&mut byte).unwrap(), 0);

        // Arithmetic overflow is an error, never a wrap-around.
        assert_eq!(
            r.seek(SeekFrom::Start(u64::MAX - AUDIO_START)).unwrap(),
            u64::MAX - AUDIO_START
        );
        let err = r.seek(SeekFrom::Current(i64::MAX)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = r.seek(SeekFrom::Start(u64::MAX)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // The source sits at `start` + local position.
        r.seek(SeekFrom::Start(2)).unwrap();
        let mut inner = r.into_inner();
        assert_eq!(inner.stream_position().unwrap(), AUDIO_START + 2);
        assert_eq!(read_all(&mut inner), b"DIODATAAPETAGEX-trailer");

        // Empty and inverted scopes.
        let mut empty = ScopedReader::new(Cursor::new(FILE), 4, 4).unwrap();
        assert!(read_all(&mut empty).is_empty());
        let err = ScopedReader::new(Cursor::new(FILE), 5, 4).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // A scope that extends past the end of the source stops at the source end.
        let mut tail = ScopedReader::new(Cursor::new(FILE), 23, 1000).unwrap();
        assert_eq!(read_all(&mut tail), b"-trailer");
    }
}
