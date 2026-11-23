//! Checking tags through a fixed buffer, without reading them into memory.
//!
//! Without [`DemuxOptions::read_metadata`](akousma_core::DemuxOptions) the
//! scanner of [`anchors`](crate::anchors) still has to know whether an
//! APEv2 or a Lyrics3 v2.00 tag is well-formed, because only a well-formed
//! tag ends the audio. [`TagStream`] gives the checks the bytes of such a
//! tag through a buffer of the caller, takes every read from the scanned
//! bytes of the [`MetadataBudget`] and seeks over the bytes they skip;
//! [`Utf8Check`] validates UTF-8 text given in pieces.

use std::io::{Read, Seek, SeekFrom};

use akousma_core::io::ByteReader;
use akousma_core::meta::MetadataBudget;
use akousma_core::{Error, ErrorKind};

/// Length of the buffer through which the scanner checks a tag.
pub(crate) const BUFFER_LEN: usize = 8192;

/// The bytes `start..end` of a source, read in order through a buffer.
///
/// A read fills as much of the buffer as the range has left (read-ahead), so
/// the bytes read never exceed the range and none is read twice. Each read
/// is first taken from [`MetadataBudget::take_scan_bytes`]; running out is
/// [`ErrorKind::LimitExceeded`] at the start of the tag. Errors of the
/// source are returned as they are.
pub(crate) struct TagStream<'a, R> {
    reader: &'a mut ByteReader<R>,
    budget: &'a mut MetadataBudget,
    /// First byte of the tag, where an exhausted budget is reported.
    tag_start: u64,
    buf: &'a mut [u8],
    /// Next unused byte of `buf`.
    next: usize,
    /// End of the bytes read into `buf`.
    filled: usize,
    /// Position in the source of the next byte given out.
    pos: u64,
    /// One past the last byte of the range.
    end: u64,
}

impl<'a, R: Read + Seek> TagStream<'a, R> {
    /// The bytes `start..end` of `reader`, read through `buf`, which must not
    /// be empty. Nothing is read yet.
    pub(crate) fn new(
        reader: &'a mut ByteReader<R>,
        budget: &'a mut MetadataBudget,
        tag_start: u64,
        start: u64,
        end: u64,
        buf: &'a mut [u8],
    ) -> Self {
        debug_assert!(!buf.is_empty(), "a tag stream needs a buffer");
        Self {
            reader,
            budget,
            tag_start,
            buf,
            next: 0,
            filled: 0,
            pos: start,
            end: end.max(start),
        }
    }

    /// Position in the source of the next byte.
    pub(crate) const fn position(&self) -> u64 {
        self.pos
    }

    /// Number of bytes left in the range.
    pub(crate) const fn remaining(&self) -> u64 {
        self.end - self.pos
    }

    /// The next bytes of the range, at most `max` of them and at least one
    /// if `max` and [`TagStream::remaining`] are not 0, without using them up.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] at the start of the tag if the scanned
    /// bytes of the budget run out, and the errors of the source.
    pub(crate) fn peek(&mut self, max: u64) -> Result<&[u8], Error> {
        if max == 0 || self.remaining() == 0 {
            return Ok(&[]);
        }
        if self.next == self.filled {
            self.refill()?;
        }
        let available = self.filled - self.next;
        let len = usize::try_from(max).map_or(available, |max| max.min(available));
        Ok(&self.buf[self.next..self.next + len])
    }

    /// Uses up `len` bytes given by [`TagStream::peek`].
    pub(crate) fn consume(&mut self, len: usize) {
        debug_assert!(len <= self.filled - self.next);
        let len = len.min(self.filled - self.next);
        self.next += len;
        self.pos += len as u64;
    }

    /// Fills `out` from the range. The caller checks that it has room.
    ///
    /// # Errors
    ///
    /// As [`TagStream::peek`].
    pub(crate) fn read_exact(&mut self, mut out: &mut [u8]) -> Result<(), Error> {
        while !out.is_empty() {
            let chunk = self.peek(out.len() as u64)?;
            let len = chunk.len();
            if len == 0 {
                return Err(Error::at(ErrorKind::UnexpectedEof, self.pos));
            }
            out[..len].copy_from_slice(chunk);
            self.consume(len);
            out = &mut out[len..];
        }
        Ok(())
    }

    /// Skips `len` bytes, at most [`TagStream::remaining`]: the bytes already
    /// in the buffer are used up, the others are not read.
    pub(crate) fn skip(&mut self, len: u64) {
        let len = len.min(self.remaining());
        let buffered = (self.filled - self.next) as u64;
        if len <= buffered {
            // `len` fits in the buffer, hence in `usize`.
            self.consume(len as usize);
        } else {
            self.next = 0;
            self.filled = 0;
            self.pos += len;
        }
    }

    /// Reads the next bytes of the range into the empty buffer.
    fn refill(&mut self) -> Result<(), Error> {
        let len = usize::try_from(self.remaining())
            .map_or(self.buf.len(), |left| left.min(self.buf.len()));
        self.budget
            .take_scan_bytes(len as u64)
            .map_err(|kind| Error::at(kind, self.tag_start))?;
        if self.reader.position() != self.pos {
            self.reader.seek(SeekFrom::Start(self.pos))?;
        }
        self.reader.read_exact(&mut self.buf[..len])?;
        self.next = 0;
        self.filled = len;
        Ok(())
    }
}

/// Validates UTF-8 text given in pieces, cut anywhere: the result is that of
/// [`core::str::from_utf8`] on the whole text.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Utf8Check {
    /// The start of a character cut by the end of the last piece.
    pending: [u8; 4],
    pending_len: usize,
    valid: bool,
}

impl Utf8Check {
    /// A check of empty text, which is valid.
    pub(crate) const fn new() -> Self {
        Self {
            pending: [0; 4],
            pending_len: 0,
            valid: true,
        }
    }

    /// Adds the next piece of the text.
    pub(crate) fn push(&mut self, mut bytes: &[u8]) {
        // Complete the character cut by the last piece, one byte at a time.
        while self.valid && self.pending_len > 0 {
            let Some((&byte, rest)) = bytes.split_first() else {
                return;
            };
            bytes = rest;
            // A cut character has at most 3 bytes, so this one fits.
            self.pending[self.pending_len] = byte;
            self.pending_len += 1;
            match core::str::from_utf8(&self.pending[..self.pending_len]) {
                Ok(_) => self.pending_len = 0,
                Err(err) if err.error_len().is_some() => self.valid = false,
                Err(_) => {}
            }
        }
        if !self.valid {
            return;
        }
        if let Err(err) = core::str::from_utf8(bytes) {
            if err.error_len().is_some() {
                self.valid = false;
            } else {
                // A character cut by the end of `bytes`: at most 3 bytes.
                let tail = &bytes[err.valid_up_to()..];
                self.pending[..tail.len()].copy_from_slice(tail);
                self.pending_len = tail.len();
            }
        }
    }

    /// Returns `true` if the whole text is valid UTF-8.
    pub(crate) const fn is_valid(&self) -> bool {
        self.valid && self.pending_len == 0
    }
}

#[cfg(test)]
mod tests;
