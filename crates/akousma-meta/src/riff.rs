//! The `LIST` chunk of type `INFO` in RIFF files (WAV, AVI).
//!
//! The chunk data is the list type `"INFO"` followed by sub-chunks: a
//! four-character identifier, a little-endian `u32` size, the text and one
//! pad byte if the size is odd. The text normally ends with `0x00`; trailing
//! `0x00` bytes are removed. Text that is valid UTF-8 is read as UTF-8,
//! other text as Windows-1252, the code page of most software that writes
//! these chunks.
//!
//! The raw key is the identifier. Unknown identifiers are kept without a
//! normalized meaning.
//!
//! | Identifier | Content | Normalized |
//! | :--- | :--- | :--- |
//! | `INAM` | title | [`TrackTitle`](StandardTag::TrackTitle) |
//! | `IART` | artist | [`Artist`](StandardTag::Artist) |
//! | `IPRD` | product (album) | [`Album`](StandardTag::Album) |
//! | `ICRD` | creation date | [`ReleaseDate`](StandardTag::ReleaseDate), if it matches a supported date form |
//! | `ICMT` | comment | [`Comment`](StandardTag::Comment) |
//! | `IGNR` | genre | [`Genre`](StandardTag::Genre) |
//! | `ITRK`, `IPRT` | track number (`n` or `n/total`) | [`TrackNumber`](StandardTag::TrackNumber) |
//! | `IFRM` | number of tracks | [`TrackTotal`](StandardTag::TrackTotal) |
//! | `ISFT` | software package used to create the file | [`EncoderSettings`](StandardTag::EncoderSettings) |
//! | `ITCH` | technician who digitized the content | [`EncodedBy`](StandardTag::EncodedBy) |
//! | `ICOP` | copyright | [`Copyright`](StandardTag::Copyright) |
//! | `ISRC` | source: the person or organization that supplied the content | [`Isrc`](StandardTag::Isrc), only if the text has the form of an International Standard Recording Code |
//! | `IENG` and others | | none |

use akousma_core::io::ByteReader;
use akousma_core::meta::{MetadataRevision, RawValue, StandardTag};
use akousma_core::{Error, ErrorKind};

use crate::encoding::{latin1, utf8_or_windows1252};
use crate::{TextTag, check_room, iso_date, new_tag, parse_count, parse_position};

/// List type of an `INFO` list.
pub const LIST_TYPE: [u8; 4] = *b"INFO";

/// Length of a sub-chunk identifier and size.
const SUBCHUNK_HEADER_LEN: usize = 8;

/// Length of an International Standard Recording Code without hyphens
/// (ISO 3901): country (2 letters), registrant (3 letters or digits), year
/// (2 digits), designation (5 digits).
const ISRC_LEN: usize = 12;

/// Length of an International Standard Recording Code written with hyphens,
/// `CC-XXX-YY-NNNNN`.
const ISRC_HYPHENATED_LEN: usize = ISRC_LEN + 3;

/// Parses the data of a `LIST` chunk of type `INFO`.
///
/// `list` starts with the list type [`LIST_TYPE`] and ends at the end of the
/// chunk data, before the pad byte of the `LIST` chunk itself.
///
/// ```
/// use akousma_core::meta::StandardTag;
/// use akousma_meta::riff;
///
/// let mut list = b"INFO".to_vec();
/// list.extend_from_slice(b"INAM\x05\0\0\0Song\0\0"); // odd size, pad byte
/// list.extend_from_slice(b"IART\x04\0\0\0Bj\xF6r"); // Windows-1252
///
/// let revision = riff::parse_list_info(&list)?;
/// assert_eq!(revision.tags[0].standard, Some(StandardTag::TrackTitle("Song".into())));
/// assert_eq!(revision.tags[1].standard, Some(StandardTag::Artist("Bj\u{F6}r".into())));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] at offset 0 if `list` does not start with
///   [`LIST_TYPE`].
/// - [`ErrorKind::UnexpectedEof`] if a sub-chunk header or its text does not
///   fit in the list; a missing pad byte after the last sub-chunk is
///   accepted.
/// - [`ErrorKind::LimitExceeded`] for more than 65 536 sub-chunks.
pub fn parse_list_info(list: &[u8]) -> Result<MetadataRevision, Error> {
    if !list.starts_with(&LIST_TYPE) {
        return Err(Error::at(ErrorKind::InvalidHeader, 0));
    }
    let mut revision = MetadataRevision::default();
    let mut pos = LIST_TYPE.len();
    while pos < list.len() {
        let header = list
            .get(pos..pos + SUBCHUNK_HEADER_LEN)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, pos as u64))?;
        check_room(&revision).map_err(|kind| Error::at(kind, pos as u64))?;
        let mut reader = ByteReader::with_offset(header, pos as u64);
        let id = reader.read_fourcc()?;
        let size = reader.read_u32_le()?;
        pos += SUBCHUNK_HEADER_LEN;

        let text = usize::try_from(size)
            .ok()
            .and_then(|size| list.get(pos..pos.checked_add(size)?))
            .ok_or(Error::at(ErrorKind::UnexpectedEof, pos as u64))?;
        // The pad byte of an odd-sized sub-chunk, unless the list ends first.
        pos = (pos + text.len() + text.len() % 2).min(list.len());

        let end = text
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |last| last + 1);
        let text = utf8_or_windows1252(&text[..end]).into_owned();
        let key = latin1(id.as_bytes()).into_owned();
        let standard = standard_tag(id.as_bytes(), &text);
        revision
            .tags
            .push(new_tag(&key, RawValue::Text(text), standard));
    }
    Ok(revision)
}

/// Normalized meaning of an `INFO` sub-chunk.
fn standard_tag(id: &[u8; 4], text: &str) -> Option<StandardTag> {
    if text.is_empty() {
        return None;
    }
    let text_tag: TextTag = match id {
        b"INAM" => StandardTag::TrackTitle,
        b"IART" => StandardTag::Artist,
        b"IPRD" => StandardTag::Album,
        b"ICMT" => StandardTag::Comment,
        b"IGNR" => StandardTag::Genre,
        b"ISFT" => StandardTag::EncoderSettings,
        b"ITCH" => StandardTag::EncodedBy,
        b"ICOP" => StandardTag::Copyright,
        b"ICRD" => return iso_date(text).map(StandardTag::ReleaseDate),
        b"ITRK" | b"IPRT" => return parse_position(text).map(StandardTag::TrackNumber),
        b"IFRM" => return parse_count(text).map(StandardTag::TrackTotal),
        b"ISRC" => return is_isrc(text).then(|| StandardTag::Isrc(text.to_owned())),
        _ => return None,
    };
    Some(text_tag(text.to_owned()))
}

/// Returns `true` if `text` has the form of an International Standard
/// Recording Code, `CC-XXX-YY-NNNNN` with or without the hyphens.
///
/// The RIFF `ISRC` sub-chunk was defined as the source of the content (a
/// name), and some software stores the recording code in it. Only the second
/// use gets a normalized meaning.
fn is_isrc(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut code = [0u8; ISRC_LEN];
    match bytes.len() {
        ISRC_LEN => code.copy_from_slice(bytes),
        ISRC_HYPHENATED_LEN if bytes[2] == b'-' && bytes[6] == b'-' && bytes[9] == b'-' => {
            code[..2].copy_from_slice(&bytes[..2]);
            code[2..5].copy_from_slice(&bytes[3..6]);
            code[5..7].copy_from_slice(&bytes[7..9]);
            code[7..].copy_from_slice(&bytes[10..]);
        }
        _ => return false,
    }
    code[..2].iter().all(u8::is_ascii_uppercase)
        && code[2..5]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && code[5..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subchunk(id: &[u8; 4], text: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
        bytes.extend_from_slice(text);
        if text.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }

    fn list(subchunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut bytes = LIST_TYPE.to_vec();
        for (id, text) in subchunks {
            bytes.extend(subchunk(id, text));
        }
        bytes
    }

    fn summary(revision: &MetadataRevision) -> Vec<(&str, &RawValue, Option<StandardTag>)> {
        revision
            .tags
            .iter()
            .map(|t| (t.raw.key.as_str(), &t.raw.value, t.standard.clone()))
            .collect()
    }

    #[test]
    fn riff_info() {
        let data = list(&[
            (b"INAM", b"Title\0"),
            (b"IART", b"Artist\0"),
            (b"IPRD", b"Album\0"),
            // Windows-1252 text of odd size 9, followed by a pad byte.
            (b"ICMT", b"\x93Hi\x94 \x80 \x99\0"),
            // UTF-8 text with several trailing NULs.
            (b"IGNR", "Bj\u{F6}rk\0\0\0".as_bytes()),
            (b"ICRD", b"2003-05-01\0"),
            (b"ITRK", b"3/12\0"),
            (b"IPRT", b"4\0"),
            (b"IFRM", b"12\0"),
            (b"ISFT", b"Lavf\0"),
            (b"ICOP", b"\xA9 2003\0"),
            (b"ISRC", b"USRC17607839\0"),
            (b"ISRC", b"US-RC1-76-07839"),
            (b"ISRC", b"Studio tapes\0"),
            (b"IENG", b"Engineer\0"),
            (b"ITCH", b"Technician\0"),
            (b"ICRD", b"Wednesday\0"),
            (b"IABC", b""),
        ]);
        let revision = parse_list_info(&data).unwrap();
        let text = |s: &str| RawValue::Text(s.into());
        let values: Vec<RawValue> = [
            "Title",
            "Artist",
            "Album",
            "\u{201C}Hi\u{201D} \u{20AC} \u{2122}",
            "Bj\u{F6}rk",
            "2003-05-01",
            "3/12",
            "4",
            "12",
            "Lavf",
            "\u{A9} 2003",
            "USRC17607839",
            "US-RC1-76-07839",
            "Studio tapes",
            "Engineer",
            "Technician",
            "Wednesday",
            "",
        ]
        .map(text)
        .to_vec();
        let expected = [
            ("INAM", Some(StandardTag::TrackTitle("Title".into()))),
            ("IART", Some(StandardTag::Artist("Artist".into()))),
            ("IPRD", Some(StandardTag::Album("Album".into()))),
            (
                "ICMT",
                Some(StandardTag::Comment(
                    "\u{201C}Hi\u{201D} \u{20AC} \u{2122}".into(),
                )),
            ),
            ("IGNR", Some(StandardTag::Genre("Bj\u{F6}rk".into()))),
            ("ICRD", Some(StandardTag::ReleaseDate("2003-05-01".into()))),
            ("ITRK", Some(StandardTag::TrackNumber(3))),
            ("IPRT", Some(StandardTag::TrackNumber(4))),
            ("IFRM", Some(StandardTag::TrackTotal(12))),
            ("ISFT", Some(StandardTag::EncoderSettings("Lavf".into()))),
            ("ICOP", Some(StandardTag::Copyright("\u{A9} 2003".into()))),
            ("ISRC", Some(StandardTag::Isrc("USRC17607839".into()))),
            ("ISRC", Some(StandardTag::Isrc("US-RC1-76-07839".into()))),
            ("ISRC", None),
            ("IENG", None),
            ("ITCH", Some(StandardTag::EncodedBy("Technician".into()))),
            ("ICRD", None),
            ("IABC", None),
        ];
        let expected: Vec<_> = expected
            .iter()
            .zip(&values)
            .map(|((key, standard), value)| (*key, value, standard.clone()))
            .collect();
        assert_eq!(summary(&revision), expected);
    }

    #[test]
    fn riff_info_pad_bytes() {
        // Odd size 5: the next sub-chunk starts after one pad byte.
        let data = list(&[(b"INAM", b"Song\0"), (b"IART", b"Band")]);
        assert_eq!(data.len(), 4 + 8 + 5 + 1 + 8 + 4);
        let revision = parse_list_info(&data).unwrap();
        assert_eq!(
            revision.tags[1].standard,
            Some(StandardTag::Artist("Band".into()))
        );

        // The last sub-chunk may lack its pad byte.
        let mut data = list(&[(b"INAM", b"Song\0")]);
        data.pop();
        assert_eq!(summary(&parse_list_info(&data).unwrap()).len(), 1);

        // An empty list and a list type alone.
        assert!(parse_list_info(b"INFO").unwrap().tags.is_empty());
        let err = parse_list_info(b"adtl").unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(0))
        );
        assert_eq!(
            parse_list_info(b"").unwrap_err().kind,
            ErrorKind::InvalidHeader
        );
    }

    #[test]
    fn riff_info_truncated() {
        // A sub-chunk header cut short.
        let mut data = list(&[(b"INAM", b"Song")]);
        data.extend_from_slice(b"IAR");
        let err = parse_list_info(&data).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::UnexpectedEof, Some(16))
        );

        // Text longer than the list.
        let mut data = list(&[(b"INAM", b"Song")]);
        data.truncate(data.len() - 1);
        let err = parse_list_info(&data).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::UnexpectedEof, Some(12))
        );
        let mut data = LIST_TYPE.to_vec();
        data.extend_from_slice(b"INAM");
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        let err = parse_list_info(&data).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::UnexpectedEof, Some(12))
        );
    }

    #[test]
    fn riff_info_field_limit() {
        let mut data = LIST_TYPE.to_vec();
        data.extend(subchunk(b"IABC", b"").repeat(65_536));
        assert_eq!(parse_list_info(&data).unwrap().tags.len(), 65_536);
        data.extend(subchunk(b"IABC", b""));
        let err = parse_list_info(&data).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(4 + 8 * 65_536))
        );
    }

    #[test]
    fn isrc_form() {
        for code in [
            "USRC17607839",
            "GBAYE6500123",
            "US-RC1-76-07839",
            "FR6V81200001",
        ] {
            assert!(is_isrc(code), "{code}");
        }
        for text in [
            "usrc17607839",
            "USRC1760783",
            "USRC176078390",
            "1SRC17607839",
            "USRC1760783X",
            "US-RC17-6-07839",
            "USRC-17607839",
        ] {
            assert!(!is_isrc(text), "{text}");
        }
    }
}
