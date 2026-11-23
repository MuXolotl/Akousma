//! Compact, allocation-free error type shared by every Akousma crate.

use core::fmt;
use std::io;

/// I/O failure category, a `Copy` subset of [`std::io::ErrorKind`].
///
/// [`std::io::Error`] is not `Copy` and may own a boxed payload, so only its
/// kind is kept. Kinds without a dedicated variant map to [`IoErrorKind::Other`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IoErrorKind {
    /// The source (for example a file) was not found.
    NotFound,
    /// The operation lacked the necessary privileges.
    PermissionDenied,
    /// The operation was interrupted and may be retried.
    Interrupted,
    /// The operation would block on a non-blocking source.
    WouldBlock,
    /// A parameter passed to the source was invalid (for example a seek before
    /// the start of a stream).
    InvalidInput,
    /// The source reported invalid data.
    InvalidData,
    /// The operation timed out.
    TimedOut,
    /// The other end of a pipe or socket was closed.
    BrokenPipe,
    /// The source does not support the operation (for example seeking).
    Unsupported,
    /// The source failed to allocate memory.
    OutOfMemory,
    /// Any other I/O failure.
    Other,
}

impl From<io::ErrorKind> for IoErrorKind {
    fn from(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::Interrupted => Self::Interrupted,
            io::ErrorKind::WouldBlock => Self::WouldBlock,
            io::ErrorKind::InvalidInput => Self::InvalidInput,
            io::ErrorKind::InvalidData => Self::InvalidData,
            io::ErrorKind::TimedOut => Self::TimedOut,
            io::ErrorKind::BrokenPipe => Self::BrokenPipe,
            io::ErrorKind::Unsupported => Self::Unsupported,
            io::ErrorKind::OutOfMemory => Self::OutOfMemory,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for IoErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "not found",
            Self::PermissionDenied => "permission denied",
            Self::Interrupted => "interrupted",
            Self::WouldBlock => "operation would block",
            Self::InvalidInput => "invalid input",
            Self::InvalidData => "invalid data",
            Self::TimedOut => "timed out",
            Self::BrokenPipe => "broken pipe",
            Self::Unsupported => "unsupported operation",
            Self::OutOfMemory => "out of memory",
            Self::Other => "other error",
        })
    }
}

/// Category of an [`Error`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The underlying source failed.
    Io(IoErrorKind),
    /// The stream ended inside a structure that requires more bytes or bits.
    UnexpectedEof,
    /// A container or stream header is malformed.
    InvalidHeader,
    /// A field holds a value that the specification does not allow.
    InvalidData,
    /// A compressed frame or packet cannot be decoded.
    CorruptFrame,
    /// A stored checksum does not match the computed one.
    ChecksumMismatch,
    /// The container format is not recognized or not supported.
    UnsupportedFormat,
    /// The codec is recognized but not supported.
    UnsupportedCodec,
    /// The codec or container uses an optional feature that is not supported.
    UnsupportedFeature,
    /// A size declared by the stream exceeds a configured limit.
    LimitExceeded,
    /// No track has the requested identifier.
    TrackNotFound,
    /// The stream contains no decodable audio track.
    NoAudioTrack,
    /// The stream changed its parameters (for example a new logical stream in
    /// a chained file); the decoder must be reset before continuing.
    ResetRequired,
}

impl From<io::ErrorKind> for ErrorKind {
    /// Maps [`io::ErrorKind::UnexpectedEof`] to [`ErrorKind::UnexpectedEof`] so
    /// that a truncated stream reports the same kind regardless of the layer
    /// that noticed it. Every other kind becomes [`ErrorKind::Io`].
    fn from(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            other => Self::Io(other.into()),
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "I/O error: {kind}"),
            Self::UnexpectedEof => f.write_str("unexpected end of stream"),
            Self::InvalidHeader => f.write_str("invalid header"),
            Self::InvalidData => f.write_str("invalid data"),
            Self::CorruptFrame => f.write_str("corrupt frame"),
            Self::ChecksumMismatch => f.write_str("checksum mismatch"),
            Self::UnsupportedFormat => f.write_str("unsupported container format"),
            Self::UnsupportedCodec => f.write_str("unsupported codec"),
            Self::UnsupportedFeature => f.write_str("unsupported feature"),
            Self::LimitExceeded => f.write_str("configured limit exceeded"),
            Self::TrackNotFound => f.write_str("track not found"),
            Self::NoAudioTrack => f.write_str("no audio track"),
            Self::ResetRequired => f.write_str("decoder reset required"),
        }
    }
}

/// Error returned by every fallible Akousma operation.
///
/// The type is `Copy`, at most 24 bytes large and never allocates.
///
/// ```
/// use akousma_core::{Error, ErrorKind};
///
/// let err = Error::at(ErrorKind::InvalidHeader, 44);
/// assert_eq!(err.kind, ErrorKind::InvalidHeader);
/// assert_eq!(err.byte_offset, Some(44));
/// assert_eq!(err.to_string(), "invalid header at byte offset 44");
///
/// let err: Error = ErrorKind::UnexpectedEof.into();
/// assert_eq!(err.byte_offset, None);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Error {
    /// Error category.
    pub kind: ErrorKind,
    /// Byte position in the stream where the error was detected, when known.
    ///
    /// Offsets are measured from the start of the stream the reader was
    /// created with (see [`ByteReader::with_offset`](crate::io::ByteReader::with_offset)).
    pub byte_offset: Option<u64>,
}

impl Error {
    /// Creates an error of `kind` detected at byte position `byte_offset`.
    #[must_use]
    pub const fn at(kind: ErrorKind, byte_offset: u64) -> Self {
        Self {
            kind,
            byte_offset: Some(byte_offset),
        }
    }

    /// Returns the same error at byte position `byte_offset`, unless it
    /// already carries a position, which is kept.
    ///
    /// A caller that knows where in the stream a nested parser started can
    /// place the errors of that parser:
    ///
    /// ```
    /// use akousma_core::{Error, ErrorKind};
    ///
    /// let err = Error::from(ErrorKind::InvalidHeader).with_offset(20);
    /// assert_eq!(err, Error::at(ErrorKind::InvalidHeader, 20));
    ///
    /// let err = Error::at(ErrorKind::UnexpectedEof, 36).with_offset(20);
    /// assert_eq!(err.byte_offset, Some(36));
    /// ```
    #[must_use]
    pub const fn with_offset(self, byte_offset: u64) -> Self {
        match self.byte_offset {
            Some(_) => self,
            None => Self::at(self.kind, byte_offset),
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self {
            kind,
            byte_offset: None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        ErrorKind::from(err.kind()).into()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.byte_offset {
            Some(offset) => write!(f, "{} at byte offset {offset}", self.kind),
            None => fmt::Display::fmt(&self.kind, f),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_copy_send_sync<T: Copy + Send + Sync + 'static>() {}
    fn assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn error_size_and_copy() {
        assert!(size_of::<Error>() <= 24, "{}", size_of::<Error>());
        assert_copy_send_sync::<Error>();
        assert_copy_send_sync::<ErrorKind>();
        assert_send_sync::<crate::Probe>();
        assert_send_sync::<crate::CodecRegistry>();

        let original = Error::at(ErrorKind::ResetRequired, u64::MAX);
        let copy = original;
        assert_eq!(original, copy);
        assert_eq!(copy.byte_offset, Some(u64::MAX));

        let eof: Error = io::Error::from(io::ErrorKind::UnexpectedEof).into();
        assert_eq!(eof.kind, ErrorKind::UnexpectedEof);
        let not_found: Error = io::Error::new(io::ErrorKind::NotFound, "boxed payload").into();
        assert_eq!(not_found.kind, ErrorKind::Io(IoErrorKind::NotFound));
        let other: Error = io::Error::from(io::ErrorKind::WriteZero).into();
        assert_eq!(other.kind, ErrorKind::Io(IoErrorKind::Other));

        assert_eq!(
            Error::from(ErrorKind::Io(IoErrorKind::Unsupported)).to_string(),
            "I/O error: unsupported operation"
        );
    }
}
