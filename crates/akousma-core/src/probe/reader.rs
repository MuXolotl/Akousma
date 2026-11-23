//! Bounded reads of the audio range while probing.

use std::io::SeekFrom;

use super::PROBE_WINDOW;
use crate::error::{Error, ErrorKind};
use crate::io::MediaSource;

/// Length of a container as its header declares it; see
/// [`FormatDescriptor::extent`](super::FormatDescriptor::extent).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Extent {
    /// The container is this many bytes long, from the first byte of the
    /// audio range.
    Bounded(u64),
    /// The header declares no length, for example a stream written without
    /// seeking back to fill in its sizes.
    Unbounded,
}

/// Reads the audio range of a source within the probe budget of
/// [`FormatLimits::max_probe_bytes`](crate::FormatLimits::max_probe_bytes).
///
/// [`ProbeReader::new`] reads the window given to the format signatures.
/// [`ProbeReader::read_exact_at`] serves reads inside the window from it, and
/// reads the others from the source, counting them against what is left of
/// the budget. Seeking over bytes that are not read costs nothing, so a
/// format can follow chunk headers through a long file with a few reads.
///
/// Offsets are relative to the first byte of the audio range.
///
/// ```
/// use akousma_core::ErrorKind;
/// use akousma_core::probe::ProbeReader;
/// use std::io::Cursor;
///
/// let mut source = Cursor::new(vec![7u8; 100_000]);
/// let mut reader = ProbeReader::new(&mut source, 0, 100_000, 16_400)?;
/// assert_eq!(reader.window().len(), 16_384); // the whole window
/// let mut header = [0u8; 8];
/// reader.read_exact_at(8, &mut header)?; // inside the window: free
/// reader.read_exact_at(90_000, &mut header)?; // read from the source
/// assert_eq!(reader.remaining_budget(), 8);
/// let mut more = [0u8; 9];
/// let err = reader.read_exact_at(50_000, &mut more).unwrap_err();
/// assert_eq!((err.kind, err.byte_offset), (ErrorKind::LimitExceeded, Some(50_000)));
/// let err = reader.read_exact_at(99_999, &mut header).unwrap_err();
/// assert_eq!(err.kind, ErrorKind::UnexpectedEof);
/// # Ok::<(), akousma_core::Error>(())
/// ```
pub struct ProbeReader<'a> {
    source: &'a mut dyn MediaSource,
    start: u64,
    len: u64,
    window: Vec<u8>,
    budget: usize,
}

impl core::fmt::Debug for ProbeReader<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProbeReader")
            .field("start", &self.start)
            .field("len", &self.len)
            .field("window", &self.window.len())
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// Error of the source at `offset`.
fn source_error(offset: u64) -> impl Fn(std::io::Error) -> Error {
    move |err| Error::at(ErrorKind::from(err.kind()), offset)
}

impl<'a> ProbeReader<'a> {
    /// Reads the window of the range of `len` bytes that starts at byte
    /// `start` of `source`: its first [`PROBE_WINDOW`] bytes, fewer if the
    /// range or `budget` is shorter. The bytes read are taken from `budget`.
    ///
    /// # Errors
    ///
    /// The errors of the source, with offsets relative to `start`.
    pub fn new(
        source: &'a mut dyn MediaSource,
        start: u64,
        len: u64,
        budget: usize,
    ) -> Result<Self, Error> {
        let window_len = usize::try_from(len)
            .map_or(PROBE_WINDOW, |len| len.min(PROBE_WINDOW))
            .min(budget);
        let mut window = vec![0; window_len];
        source
            .seek(SeekFrom::Start(start))
            .map_err(source_error(0))?;
        source.read_exact(&mut window).map_err(source_error(0))?;
        Ok(Self {
            source,
            start,
            len,
            window,
            budget: budget - window_len,
        })
    }

    /// The first bytes of the range, read by [`ProbeReader::new`].
    #[must_use]
    pub fn window(&self) -> &[u8] {
        &self.window
    }

    /// Length of the range.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns `true` if the range is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes that can still be read from the source.
    #[must_use]
    pub const fn remaining_budget(&self) -> usize {
        self.budget
    }

    /// Fills `buf` with the bytes of the range at `offset`.
    ///
    /// A read that lies within the window is served from it. Any other read
    /// is made from the source and takes `buf.len()` bytes from the budget.
    ///
    /// # Errors
    ///
    /// At `offset`: [`ErrorKind::UnexpectedEof`] if the bytes reach past the
    /// end of the range, [`ErrorKind::LimitExceeded`] if they are not in the
    /// window and more than the budget, and the errors of the source.
    pub fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let end = u64::try_from(buf.len())
            .ok()
            .and_then(|len| offset.checked_add(len))
            .filter(|&end| end <= self.len)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, offset))?;
        let in_window = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(from, to)| self.window.get(from..to));
        if let Some(bytes) = in_window {
            buf.copy_from_slice(bytes);
            return Ok(());
        }
        self.budget = self
            .budget
            .checked_sub(buf.len())
            .ok_or(Error::at(ErrorKind::LimitExceeded, offset))?;
        let position = self
            .start
            .checked_add(offset)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, offset))?;
        self.source
            .seek(SeekFrom::Start(position))
            .map_err(source_error(offset))?;
        self.source.read_exact(buf).map_err(source_error(offset))
    }
}
