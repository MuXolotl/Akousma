//! Decoding of text stored in tags.
//!
//! | Function | Encoding | Invalid input |
//! | :--- | :--- | :--- |
//! | [`latin1`] | ISO-8859-1: each byte maps to the Unicode code point with the same value | none |
//! | [`windows1252`] | Windows-1252 (WHATWG index) | none |
//! | [`utf8_or_windows1252`] | UTF-8, or Windows-1252 if not valid UTF-8 | none |
//! | [`utf16`] | UTF-16 of a known byte order | lone surrogates become `U+FFFD` |
//! | [`utf16_bom`] | UTF-16 with an optional byte order mark | lone surrogates become `U+FFFD` |
//!
//! Single-byte decoders return borrowed text when the input is ASCII.
//! [`split_nul`] and [`split_nul16`] find the terminator of a string inside a
//! larger field; the 16-bit variant only accepts a terminator that starts at
//! an even offset, so the byte pair `0x00 0x00` formed by two adjacent code
//! units (`'A'` `U+0100` in UTF-16LE is `41 00 00 01`) does not cut a string.

use std::borrow::Cow;

use crate::Endian;

/// Windows-1252 characters for bytes `0x80..=0x9F`, from the WHATWG
/// Encoding Standard index (`index-windows-1252.txt`). The five bytes without
/// a character in Windows-1252 map to the C1 control of the same value, as in
/// that index. Bytes `0xA0..=0xFF` are the same as in ISO-8859-1.
const WINDOWS_1252_80_9F: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Returns `bytes` as text if they are ASCII.
fn ascii(bytes: &[u8]) -> Option<&str> {
    if bytes.is_ascii() {
        core::str::from_utf8(bytes).ok()
    } else {
        None
    }
}

/// Decodes ISO-8859-1 (Latin-1) text: every byte is the Unicode scalar of the
/// same value.
///
/// ```
/// use akousma_meta::encoding::latin1;
/// use std::borrow::Cow;
///
/// assert!(matches!(latin1(b"Blues"), Cow::Borrowed("Blues")));
/// assert_eq!(latin1(&[0x43, 0x61, 0x66, 0xE9]), "Caf\u{E9}");
/// assert_eq!(latin1(&[0x80]), "\u{80}");
/// ```
#[must_use]
pub fn latin1(bytes: &[u8]) -> Cow<'_, str> {
    match ascii(bytes) {
        Some(text) => Cow::Borrowed(text),
        None => Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

/// Decodes Windows-1252 text.
///
/// Differs from [`latin1`] only for bytes `0x80..=0x9F`, which hold
/// typographic characters (`0x80` is `'€'`, `0x93` is `'“'`) instead of C1
/// controls.
///
/// ```
/// use akousma_meta::encoding::windows1252;
///
/// assert_eq!(windows1252(&[0x93, 0x48, 0x69, 0x94, 0x20, 0x80, 0xA9]), "\u{201C}Hi\u{201D} \u{20AC}\u{A9}");
/// ```
#[must_use]
pub fn windows1252(bytes: &[u8]) -> Cow<'_, str> {
    match ascii(bytes) {
        Some(text) => Cow::Borrowed(text),
        None => Cow::Owned(
            bytes
                .iter()
                .map(|&b| match b {
                    0x80..=0x9F => WINDOWS_1252_80_9F[usize::from(b - 0x80)],
                    _ => char::from(b),
                })
                .collect(),
        ),
    }
}

/// Decodes UTF-8 text, or Windows-1252 text if `bytes` are not valid UTF-8.
///
/// Used for fields without a declared encoding, such as RIFF `INFO` strings
/// written in the system code page by old Windows software.
///
/// ```
/// use akousma_meta::encoding::utf8_or_windows1252;
///
/// assert_eq!(utf8_or_windows1252("Bj\u{F6}rk".as_bytes()), "Bj\u{F6}rk");
/// assert_eq!(utf8_or_windows1252(&[0x42, 0x6A, 0xF6, 0x72, 0x6B]), "Bj\u{F6}rk");
/// ```
#[must_use]
pub fn utf8_or_windows1252(bytes: &[u8]) -> Cow<'_, str> {
    match core::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => windows1252(bytes),
    }
}

/// Decodes UTF-16 text of the given byte order.
///
/// A trailing odd byte is ignored and unpaired surrogates become
/// `U+FFFD REPLACEMENT CHARACTER`.
///
/// ```
/// use akousma_meta::{Endian, encoding::utf16};
///
/// assert_eq!(utf16(&[0x41, 0x00, 0x00, 0x01], Endian::Little), "A\u{100}");
/// assert_eq!(utf16(&[0x00, 0x41, 0xD8], Endian::Big), "A");
/// assert_eq!(utf16(&[0x00, 0xD8, 0x42, 0x00], Endian::Little), "\u{FFFD}B");
/// ```
#[must_use]
pub fn utf16(bytes: &[u8], order: Endian) -> String {
    let (units, _odd) = bytes.as_chunks::<2>();
    let units = units.iter().map(|&pair| match order {
        Endian::Little => u16::from_le_bytes(pair),
        Endian::Big => u16::from_be_bytes(pair),
    });
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Decodes UTF-16 text that may start with a byte order mark, and returns
/// the text and the byte order used.
///
/// The mark `FF FE` selects little endian and `FE FF` big endian; it is not
/// part of the text. Without a mark the order is `default` if given (for
/// example the order of the previous string of the same field, as in ID3v2
/// frames that write a mark only once), otherwise it is guessed from the
/// first two bytes: little endian if only the second one is zero, as in
/// little-endian ASCII text, and big endian in every other case.
///
/// ```
/// use akousma_meta::{Endian, encoding::utf16_bom};
///
/// assert_eq!(utf16_bom(&[0xFE, 0xFF, 0x00, 0x41], None), ("A".to_owned(), Endian::Big));
/// // No mark: the order of the previous string is kept.
/// assert_eq!(utf16_bom(&[0x00, 0x42], Some(Endian::Big)), ("B".to_owned(), Endian::Big));
/// // No mark and no previous string: guessed from the bytes.
/// assert_eq!(utf16_bom(&[0x43, 0x00], None), ("C".to_owned(), Endian::Little));
/// assert_eq!(utf16_bom(&[], None), (String::new(), Endian::Big));
/// ```
#[must_use]
pub fn utf16_bom(bytes: &[u8], default: Option<Endian>) -> (String, Endian) {
    let (order, text) = match bytes {
        [0xFF, 0xFE, text @ ..] => (Endian::Little, text),
        [0xFE, 0xFF, text @ ..] => (Endian::Big, text),
        [first, 0, ..] if default.is_none() && *first != 0 => (Endian::Little, bytes),
        _ => (default.unwrap_or(Endian::Big), bytes),
    };
    (utf16(text, order), order)
}

/// Splits `bytes` at the first `0x00`.
///
/// Returns the bytes before the terminator and the bytes after it, or all
/// bytes and `None` if there is no terminator.
///
/// ```
/// use akousma_meta::encoding::split_nul;
///
/// assert_eq!(split_nul(b"desc\0value"), (&b"desc"[..], Some(&b"value"[..])));
/// assert_eq!(split_nul(b"text"), (&b"text"[..], None));
/// ```
#[must_use]
pub fn split_nul(bytes: &[u8]) -> (&[u8], Option<&[u8]>) {
    match bytes.iter().position(|&b| b == 0) {
        Some(end) => (&bytes[..end], Some(&bytes[end + 1..])),
        None => (bytes, None),
    }
}

/// Splits UTF-16 `bytes` at the first `0x00 0x00` code unit that starts at an
/// even offset.
///
/// Returns the bytes before the terminator and the bytes after it, or all
/// bytes and `None` if there is no terminator.
///
/// ```
/// use akousma_meta::encoding::split_nul16;
///
/// // `'A'` and `U+0100` in UTF-16LE contain `00 00` at an odd offset.
/// let field = [0x41, 0x00, 0x00, 0x01, 0x00, 0x00, 0x42, 0x00];
/// assert_eq!(split_nul16(&field), (&field[..4], Some(&field[6..])));
/// assert_eq!(split_nul16(&field[..4]), (&field[..4], None));
/// ```
#[must_use]
pub fn split_nul16(bytes: &[u8]) -> (&[u8], Option<&[u8]>) {
    let (units, _odd) = bytes.as_chunks::<2>();
    match units.iter().position(|&unit| unit == [0, 0]) {
        Some(index) => {
            let end = index * 2;
            (&bytes[..end], Some(&bytes[end + 2..]))
        }
        None => (bytes, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows1252_table_matches_whatwg_index() {
        // Pointers 0..=31 of `index-windows-1252.txt` (WHATWG, 2024-09-18).
        let expected: [u32; 32] = [
            0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
            0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
            0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
        ];
        for (byte, code) in (0x80u8..=0x9F).zip(expected) {
            let bytes = [byte];
            let text = windows1252(&bytes);
            assert_eq!(
                text.chars().collect::<Vec<_>>(),
                [char::from_u32(code).unwrap()]
            );
        }
        for byte in (0x00u8..0x80).chain(0xA0..=0xFF) {
            assert_eq!(windows1252(&[byte]), latin1(&[byte]));
            assert_eq!(latin1(&[byte]).chars().next(), Some(char::from(byte)));
        }
    }

    #[test]
    fn single_byte_text_is_borrowed_when_ascii() {
        assert!(matches!(latin1(b""), Cow::Borrowed("")));
        assert!(matches!(windows1252(b"Hi"), Cow::Borrowed("Hi")));
        assert!(matches!(
            utf8_or_windows1252("\u{E9}".as_bytes()),
            Cow::Borrowed("\u{E9}")
        ));
        // Valid UTF-8 bytes are still decoded as Latin-1 by `latin1`.
        assert_eq!(latin1("\u{E9}".as_bytes()), "\u{C3}\u{A9}");
        // Invalid UTF-8 falls back to Windows-1252.
        assert_eq!(utf8_or_windows1252(&[0xA9, 0x20, 0x99]), "\u{A9} \u{2122}");
    }

    #[test]
    fn utf16_even_alignment_and_single_bom() {
        // The terminator search must not stop at the odd-offset `00 00`.
        let field = [0x41, 0x00, 0x00, 0x01];
        assert_eq!(split_nul16(&field), (&field[..], None));
        assert_eq!(utf16(&field, Endian::Little), "A\u{100}");
        let mut with_terminator = field.to_vec();
        with_terminator.extend_from_slice(&[0x00, 0x00]);
        assert_eq!(split_nul16(&with_terminator), (&field[..], Some(&[][..])));
        // A terminator at the end of an odd-length field is still found.
        assert_eq!(
            split_nul16(&[0x41, 0x00, 0x00, 0x00, 0x07]),
            (&[0x41, 0x00][..], Some(&[0x07][..]))
        );

        // Empty input and input without a mark.
        assert_eq!(utf16_bom(&[], None), (String::new(), Endian::Big));
        assert_eq!(utf16_bom(&[0x41], None), (String::new(), Endian::Big));
        assert_eq!(
            utf16_bom(&[0xFF, 0xFE], None),
            (String::new(), Endian::Little)
        );
        assert_eq!(
            utf16_bom(&[0x48, 0x00, 0x69, 0x00], None),
            ("Hi".to_owned(), Endian::Little)
        );
        assert_eq!(
            utf16_bom(&[0x00, 0x48, 0x00, 0x69], None),
            ("Hi".to_owned(), Endian::Big)
        );
        // A given default order wins over the guess.
        assert_eq!(
            utf16_bom(&[0x01, 0x00], Some(Endian::Big)),
            ("\u{100}".to_owned(), Endian::Big)
        );

        // A description with a mark followed by a value without one, as in
        // ID3v2 `COMM` frames written by some taggers.
        let frame = [0xFF, 0xFE, 0x64, 0x00, 0x00, 0x00, 0x76, 0x00, 0x00, 0x01];
        let (desc, rest) = split_nul16(&frame);
        let (desc, order) = utf16_bom(desc, None);
        assert_eq!((desc.as_str(), order), ("d", Endian::Little));
        let (value, order) = utf16_bom(rest.unwrap(), Some(order));
        assert_eq!((value.as_str(), order), ("v\u{100}", Endian::Little));

        // An odd trailing byte is ignored and a lone surrogate is replaced.
        assert_eq!(
            utf16(&[0x3D, 0xD8, 0x00, 0xDE, 0x41], Endian::Little),
            "\u{1F600}"
        );
        assert_eq!(utf16(&[0xDC, 0x00], Endian::Big), "\u{FFFD}");
    }

    #[test]
    fn split_nul_edges() {
        assert_eq!(split_nul(b""), (&b""[..], None));
        assert_eq!(split_nul(b"\0"), (&b""[..], Some(&b""[..])));
        assert_eq!(split_nul(b"a\0b\0c"), (&b"a"[..], Some(&b"b\0c"[..])));
        assert_eq!(split_nul16(&[0x00]), (&[0x00][..], None));
        assert_eq!(split_nul16(&[0x00, 0x00]), (&[][..], Some(&[][..])));
    }
}
