//! Text of ID3v2 frames: encodings, terminators and genre references.

use akousma_core::ErrorKind;
use akousma_core::meta::RawValue;

use crate::encoding::{latin1, split_nul, split_nul16, utf16, utf16_bom};
use crate::{Endian, MAX_FIELD_VALUES, id3v1, parse_count};

/// Encoding byte of a frame.
#[derive(Copy, Clone)]
pub(super) enum Encoding {
    Latin1,
    Utf16,
    Utf16Be,
    Utf8,
}

impl Encoding {
    pub(super) fn new(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Latin1,
            1 => Self::Utf16,
            2 => Self::Utf16Be,
            3 => Self::Utf8,
            _ => return None,
        })
    }

    /// Splits `bytes` at the first terminator of this encoding.
    fn split(self, bytes: &[u8]) -> (&[u8], Option<&[u8]>) {
        match self {
            Self::Latin1 | Self::Utf8 => split_nul(bytes),
            Self::Utf16 | Self::Utf16Be => split_nul16(bytes),
        }
    }

    /// Decodes one string; `order` carries the byte order of the previous
    /// UTF-16 string of the frame, for strings without a byte order mark.
    fn decode(self, bytes: &[u8], order: &mut Option<Endian>) -> String {
        match self {
            Self::Latin1 => latin1(bytes).into_owned(),
            Self::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Self::Utf16Be => utf16(bytes, Endian::Big),
            Self::Utf16 => {
                let (text, used) = utf16_bom(bytes, *order);
                *order = Some(used);
                text
            }
        }
    }
}

/// Reads a terminated string from the start of `bytes`; returns it and the
/// bytes after the terminator (empty if there is none).
pub(super) fn string<'a>(
    encoding: Encoding,
    bytes: &'a [u8],
    order: &mut Option<Endian>,
) -> (String, &'a [u8]) {
    let (text, rest) = encoding.split(bytes);
    (encoding.decode(text, order), rest.unwrap_or_default())
}

/// Decodes the terminator-separated strings of `bytes`. A terminator at the
/// very end does not start another string.
pub(super) fn strings(
    encoding: Encoding,
    mut bytes: &[u8],
    order: &mut Option<Endian>,
) -> Result<Vec<String>, ErrorKind> {
    let mut values = Vec::new();
    loop {
        if values.len() >= MAX_FIELD_VALUES {
            return Err(ErrorKind::LimitExceeded);
        }
        let (text, rest) = encoding.split(bytes);
        values.push(encoding.decode(text, order));
        match rest {
            Some(rest) if !rest.is_empty() => bytes = rest,
            _ => return Ok(values),
        }
    }
}

/// Raw value of a list of strings.
pub(super) fn raw_value(mut values: Vec<String>) -> RawValue {
    if values.len() == 1 {
        RawValue::Text(values.pop().unwrap_or_default())
    } else {
        RawValue::MultiText(values)
    }
}

/// Genre name of a `TCON` value: ID3v1 genre numbers (`"17"`, `"(17)"`) are
/// replaced by their names, `RX` and `CR` by `Remix` and `Cover`, and a
/// refinement after the numbers (`"(17)Hard Rock"`) wins. `((` starts a name
/// that begins with `(`.
pub(super) fn genre(value: &str) -> Option<String> {
    let reference = |code: &str| match code {
        "RX" => Some("Remix"),
        "CR" => Some("Cover"),
        _ => parse_count(code)
            .and_then(|index| u8::try_from(index).ok())
            .and_then(id3v1::genre_name),
    };
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        // A number without a genre, such as 255 ("none"), has no name.
        return reference(value).map(str::to_owned);
    }
    if let Some(escaped) = value.strip_prefix("((") {
        return Some(format!("({escaped}"));
    }
    if let Some(name) = reference(value) {
        return Some(name.to_owned());
    }
    let mut rest = value;
    let mut first = None;
    while let Some(inner) = rest.strip_prefix('(') {
        if inner.starts_with('(') {
            break;
        }
        let Some((code, after)) = inner.split_once(')') else {
            break;
        };
        first = first.or(reference(code));
        rest = after;
    }
    if rest.is_empty() {
        first.map(str::to_owned)
    } else if let Some(escaped) = rest.strip_prefix("((") {
        Some(format!("({escaped}"))
    } else {
        Some(rest.to_owned())
    }
}

/// Key of a frame with a description: `ID:description`, or `ID` alone.
pub(super) fn described_key(id: &str, description: &str) -> String {
    if description.is_empty() {
        id.to_owned()
    } else {
        format!("{id}:{description}")
    }
}
