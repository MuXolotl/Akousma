//! Read-only parsers of audio tags for the Akousma audio decoding library.
//!
//! Every parser returns a [`MetadataRevision`] of the `akousma-core` metadata
//! model: each field keeps its stored key and value
//! next to its normalized meaning, if it has one.
//!
//! | Module | Format | Input |
//! | :--- | :--- | :--- |
//! | [`encoding`] | Latin-1, Windows-1252, UTF-8 and UTF-16 text | byte slice |
//! | [`id3v1`] | ID3v1, ID3v1.1 and the `TAG+` extension, genre list | fixed-size array |
//! | [`ape`] | APEv1 and APEv2 tags | byte slice, or a seekable source |
//! | [`id3v2`] | ID3v2.2, ID3v2.3 and ID3v2.4 tags, with pictures and chapters | byte slice |
//! | [`lyrics3`] | Lyrics3 v2.00 tags; location of Lyrics3 v1.00 tags | byte slice, or a seekable source |
//! | [`riff`] | `LIST`/`INFO` chunk of RIFF files | byte slice |
//! | [`anchors`] | the tags around the audio data of a file | seekable source |
//!
//! Errors of the slice parsers carry byte offsets relative to the start of
//! the slice. The readers of [`ape`] locate a tag that ends at a given
//! position of a seekable source, read it within the limits of
//! [`FormatLimits`](akousma_core::FormatLimits) and report absolute offsets.
//! The readers of [`lyrics3`] work the same way. Without
//! [`DemuxOptions::read_metadata`](akousma_core::DemuxOptions), the scanner
//! of [`anchors`] checks these tags by the same rules through a fixed buffer
//! instead of reading them.
//!
//! Besides the byte limits, every parser bounds the memory spent on tiny
//! fields: a tag block with more than 65 536 fields (tags and pictures
//! together), or a field with more than 65 536 values, fails with
//! [`ErrorKind::LimitExceeded`].
//!
//! Normalized values are derived only from well-formed text: a track field
//! `"A1"` or a date `"May 2001"` is kept as stored, without a normalized
//! meaning.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::result_large_err)]

use akousma_core::ErrorKind;
use akousma_core::meta::{MetadataRevision, RawTag, RawValue, StandardTag, Tag};

pub mod anchors;
pub mod ape;
pub mod encoding;
pub mod id3v1;
pub mod id3v2;
pub mod lyrics3;
pub mod riff;
mod stream;

/// Byte order of multi-byte values.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// A tag that ends at a known position of a source, such as the end of a
/// file or the start of the tag that follows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrailingTag {
    /// Position of the first byte of the tag, including its header if it has
    /// one. Audio data or another tag ends here.
    pub start: u64,
    /// Fields of the tag.
    pub revision: MetadataRevision,
}

/// A tag that starts at a known position of a source, such as the start of
/// a file or the end of the tag before it.
#[derive(Clone, Debug, PartialEq)]
pub struct LeadingTag {
    /// Position one past the last byte of the tag. Audio data or another tag
    /// starts here.
    pub end: u64,
    /// Fields of the tag.
    pub revision: MetadataRevision,
}

/// What a reader of [`ape`] or [`lyrics3`] found at a position of a source,
/// once every byte it needed was read.
///
/// The errors of reading the source itself (an [`ErrorKind::Io`] failure,
/// or the source ending inside the length it reported) are not a `Found`:
/// they are returned as errors, and the scanner of [`anchors`] fails with
/// them instead of taking the bytes for a damaged tag.
#[derive(Debug, PartialEq)]
enum Found<T> {
    /// No tag starts or ends here.
    Absent,
    /// A well-formed tag.
    Tag(T),
    /// A tag whose bytes were all read and do not form a valid tag, such as
    /// an item that does not fit in the tag.
    Damaged(akousma_core::Error),
}

impl<T> Found<T> {
    /// The tag, if one was found and is well-formed.
    fn tag(self) -> Option<T> {
        match self {
            Self::Tag(tag) => Some(tag),
            Self::Absent | Self::Damaged(_) => None,
        }
    }

    /// The result of the public readers, which do not tell a damaged tag
    /// from an error of the source.
    fn into_result(self) -> Result<Option<T>, akousma_core::Error> {
        match self {
            Self::Absent => Ok(None),
            Self::Tag(tag) => Ok(Some(tag)),
            Self::Damaged(err) => Err(err),
        }
    }

    /// Applies `f` to the tag.
    fn map<U>(self, f: impl FnOnce(T) -> U) -> Found<U> {
        match self {
            Self::Absent => Found::Absent,
            Self::Tag(tag) => Found::Tag(f(tag)),
            Self::Damaged(err) => Found::Damaged(err),
        }
    }
}

/// What a reader of [`ape`] or [`lyrics3`] does with the fields of a tag it
/// found.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Items {
    /// Read the whole tag, within the byte limit of the budget, and parse it.
    Parse,
    /// Check it by the same rules through a fixed buffer, within the scanned
    /// bytes of the budget, and keep nothing: the scan without
    /// [`DemuxOptions::read_metadata`](akousma_core::DemuxOptions).
    Check,
}

/// Sorts the error of parsing bytes that were all read: an exhausted limit
/// is returned, any other error makes the tag [`Found::Damaged`].
fn parsed<T>(result: Result<T, akousma_core::Error>) -> Result<Found<T>, akousma_core::Error> {
    match result {
        Ok(tag) => Ok(Found::Tag(tag)),
        Err(err) if err.kind == ErrorKind::LimitExceeded => Err(err),
        Err(err) => Ok(Found::Damaged(err)),
    }
}

/// Constructor of a text [`StandardTag`], such as [`StandardTag::Artist`].
type TextTag = fn(String) -> StandardTag;

/// Largest number of fields (tags and pictures together) of one revision.
const MAX_REVISION_FIELDS: usize = 65_536;

/// Largest number of values of one [`RawValue::MultiText`].
const MAX_FIELD_VALUES: usize = 65_536;

/// Fails with [`ErrorKind::LimitExceeded`] if `revision` cannot take one
/// more field.
fn check_room(revision: &MetadataRevision) -> Result<(), ErrorKind> {
    if revision.tags.len() + revision.visuals.len() >= MAX_REVISION_FIELDS {
        Err(ErrorKind::LimitExceeded)
    } else {
        Ok(())
    }
}

/// Splits `text` at every `separator` into a [`RawValue::MultiText`], or
/// returns it as one [`RawValue::Text`] if it has no separator.
///
/// Fails with [`ErrorKind::LimitExceeded`] for more than
/// [`MAX_FIELD_VALUES`] values, before allocating any of them.
fn split_values(text: &str, separator: char) -> Result<RawValue, ErrorKind> {
    let separators = text.matches(separator).count();
    if separators == 0 {
        Ok(RawValue::Text(text.to_owned()))
    } else if separators >= MAX_FIELD_VALUES {
        Err(ErrorKind::LimitExceeded)
    } else {
        Ok(RawValue::MultiText(
            text.split(separator).map(str::to_owned).collect(),
        ))
    }
}

/// Builds a [`Tag`].
fn new_tag(key: &str, value: RawValue, standard: Option<StandardTag>) -> Tag {
    Tag {
        raw: RawTag {
            key: key.to_owned(),
            value,
        },
        standard,
    }
}

/// Parses a position field such as a track number: `"3"` or `"3/12"`, with
/// optional surrounding ASCII whitespace. Returns the position only; the
/// total stays in the stored value.
fn parse_position(text: &str) -> Option<u32> {
    let number = match text.split_once('/') {
        Some((number, _total)) => number,
        None => text,
    };
    parse_count(number)
}

/// Parses a decimal count with optional surrounding ASCII whitespace.
fn parse_count(text: &str) -> Option<u32> {
    let digits = text.trim_ascii();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Parses a finite decimal number with optional surrounding ASCII whitespace
/// and a decimal comma instead of a point, as some writers store
/// ReplayGain values.
fn parse_float(text: &str) -> Option<f32> {
    let text = text.trim_ascii();
    let value = if text.matches(',').count() == 1 && !text.contains('.') {
        text.replacen(',', ".", 1).parse::<f32>()
    } else {
        text.parse::<f32>()
    };
    // `f32::from_str` also accepts `inf` and `NaN`, which are no gains.
    value.ok().filter(|value| value.is_finite())
}

/// Returns `text` if it matches a supported date form: `YYYY`, `YYYY-MM`,
/// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS`. It checks the field layout and
/// broad ranges (month `1..=12`, day `1..=31`, and valid clock fields), but
/// does not check the number of days in a particular month or leap years.
fn iso_date(text: &str) -> Option<String> {
    /// Value of two ASCII digits.
    fn pair(digits: &[u8]) -> Option<u8> {
        match digits {
            [tens @ b'0'..=b'9', ones @ b'0'..=b'9'] => Some((tens - b'0') * 10 + (ones - b'0')),
            _ => None,
        }
    }

    let bytes = text.as_bytes();
    let year = bytes.get(..4)?;
    if !year.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let valid = match bytes.len() {
        4 => true,
        7 | 10 | 19 => {
            let month = pair(&bytes[5..7]);
            let mut ok = bytes[4] == b'-' && matches!(month, Some(1..=12));
            if bytes.len() >= 10 {
                ok &= bytes[7] == b'-' && matches!(pair(&bytes[8..10]), Some(1..=31));
            }
            if bytes.len() == 19 {
                ok &= bytes[10] == b'T'
                    && bytes[13] == b':'
                    && bytes[16] == b':'
                    && matches!(pair(&bytes[11..13]), Some(0..=23))
                    && matches!(pair(&bytes[14..16]), Some(0..=59))
                    && matches!(pair(&bytes[17..19]), Some(0..=59));
            }
            ok
        }
        _ => false,
    };
    valid.then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_and_counts() {
        assert_eq!(parse_position("3"), Some(3));
        assert_eq!(parse_position(" 03/12 "), Some(3));
        assert_eq!(parse_position("7/"), Some(7));
        assert_eq!(parse_position("4294967295"), Some(u32::MAX));
        assert_eq!(parse_position("4294967296"), None);
        assert_eq!(parse_position("A1"), None);
        assert_eq!(parse_position("+3"), None);
        assert_eq!(parse_position("/12"), None);
        assert_eq!(parse_position(""), None);
        assert_eq!(parse_count("12"), Some(12));
        assert_eq!(parse_count("1 2"), None);
    }

    #[test]
    fn floats_with_point_or_comma() {
        assert_eq!(parse_float(" -6.42 "), Some(-6.42));
        assert_eq!(parse_float("-6,42"), Some(-6.42));
        assert_eq!(parse_float("+3"), Some(3.0));
        assert_eq!(parse_float("1,000.5"), None);
        assert_eq!(parse_float("1,2,3"), None);
        assert_eq!(parse_float("inf"), None);
        assert_eq!(parse_float("NaN"), None);
        assert_eq!(parse_float(""), None);
    }

    #[test]
    fn field_and_value_limits() {
        let mut revision = MetadataRevision::default();
        let tag = new_tag("K", RawValue::Flag(true), None);
        revision.tags = vec![tag; MAX_REVISION_FIELDS - 1];
        assert_eq!(check_room(&revision), Ok(()));
        revision.tags.push(new_tag("K", RawValue::Flag(true), None));
        assert_eq!(check_room(&revision), Err(ErrorKind::LimitExceeded));

        assert_eq!(split_values("a", '\0'), Ok(RawValue::Text("a".into())));
        assert_eq!(
            split_values("a\0\0b", '\0'),
            Ok(RawValue::MultiText(vec![
                "a".into(),
                String::new(),
                "b".into()
            ]))
        );
        let most = "\0".repeat(MAX_FIELD_VALUES - 1);
        assert!(matches!(
            split_values(&most, '\0'),
            Ok(RawValue::MultiText(values)) if values.len() == MAX_FIELD_VALUES
        ));
        let too_many = "\0".repeat(MAX_FIELD_VALUES);
        assert_eq!(split_values(&too_many, '\0'), Err(ErrorKind::LimitExceeded));
    }

    #[test]
    fn iso_dates() {
        for valid in [
            "2003",
            "2003-05",
            "2003-05-31",
            "1999-12-31T23:59:59",
            "0000",
        ] {
            assert_eq!(iso_date(valid).as_deref(), Some(valid));
        }
        for invalid in [
            "",
            "99",
            "20031",
            "2003-13",
            "2003-00",
            "2003-05-32",
            "2003/05/01",
            "2003-05-01 10:00:00",
            "2003-05-01T24:00:00",
            "2003-05-01T10:60:00",
            "May 2003",
            "\u{661}\u{662}\u{663}\u{664}",
        ] {
            assert_eq!(iso_date(invalid), None, "{invalid:?}");
        }
    }
}
