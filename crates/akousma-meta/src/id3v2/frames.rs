//! Content of ID3v2 frames: text, URLs, comments, pictures, volume
//! adjustment and chapters.

use core::time::Duration;

use akousma_core::ErrorKind;
use akousma_core::meta::{Chapter, RawValue, StandardTag, Tag, Visual, VisualKind};

use super::text::{Encoding, described_key, genre, raw_value, string, strings};
use super::{Format, Frames, MAX_SUBFRAME_DEPTH, parse_frames};
use crate::encoding::{latin1, split_nul};
use crate::{TextTag, iso_date, new_tag, parse_float, parse_position};

/// A frame after its header and optional fields.
pub(super) struct Frame<'a> {
    /// Identifier (ID3v2.4, or three characters if it has no equivalent).
    pub(super) id: &'a str,
    /// The identifier was stored with three characters (ID3v2.2 layout).
    pub(super) short_id: bool,
    /// Content.
    pub(super) payload: &'a [u8],
}

/// Three-character ID3v2.2 identifiers and their four-character ID3v2.3 /
/// ID3v2.4 equivalents.
const UPGRADES: [(&[u8; 3], &[u8; 4]); 62] = [
    (b"BUF", b"RBUF"),
    (b"CNT", b"PCNT"),
    (b"COM", b"COMM"),
    (b"CRA", b"AENC"),
    (b"ETC", b"ETCO"),
    (b"EQU", b"EQUA"),
    (b"GEO", b"GEOB"),
    (b"IPL", b"IPLS"),
    (b"LNK", b"LINK"),
    (b"MCI", b"MCDI"),
    (b"MLL", b"MLLT"),
    (b"PIC", b"APIC"),
    (b"POP", b"POPM"),
    (b"REV", b"RVRB"),
    (b"RVA", b"RVAD"),
    (b"SLT", b"SYLT"),
    (b"STC", b"SYTC"),
    (b"TAL", b"TALB"),
    (b"TBP", b"TBPM"),
    (b"TCM", b"TCOM"),
    (b"TCO", b"TCON"),
    (b"TCR", b"TCOP"),
    (b"TDA", b"TDAT"),
    (b"TDY", b"TDLY"),
    (b"TEN", b"TENC"),
    (b"TFT", b"TFLT"),
    (b"TIM", b"TIME"),
    (b"TKE", b"TKEY"),
    (b"TLA", b"TLAN"),
    (b"TLE", b"TLEN"),
    (b"TMT", b"TMED"),
    (b"TOA", b"TOPE"),
    (b"TOF", b"TOFN"),
    (b"TOL", b"TOLY"),
    (b"TOR", b"TORY"),
    (b"TOT", b"TOAL"),
    (b"TP1", b"TPE1"),
    (b"TP2", b"TPE2"),
    (b"TP3", b"TPE3"),
    (b"TP4", b"TPE4"),
    (b"TPA", b"TPOS"),
    (b"TPB", b"TPUB"),
    (b"TRC", b"TSRC"),
    (b"TRD", b"TRDA"),
    (b"TRK", b"TRCK"),
    (b"TSI", b"TSIZ"),
    (b"TSS", b"TSSE"),
    (b"TT1", b"TIT1"),
    (b"TT2", b"TIT2"),
    (b"TT3", b"TIT3"),
    (b"TXT", b"TEXT"),
    (b"TXX", b"TXXX"),
    (b"TYE", b"TYER"),
    (b"UFI", b"UFID"),
    (b"ULT", b"USLT"),
    (b"WAF", b"WOAF"),
    (b"WAR", b"WOAR"),
    (b"WAS", b"WOAS"),
    (b"WCM", b"WCOM"),
    (b"WCP", b"WCOP"),
    (b"WPB", b"WPUB"),
    (b"WXX", b"WXXX"),
];

/// Text frames normalized to a text [`StandardTag`].
const TEXT_FRAMES: [(&str, TextTag); 16] = [
    ("TIT2", StandardTag::TrackTitle),
    ("TIT3", StandardTag::TrackSubtitle),
    ("TALB", StandardTag::Album),
    ("TPE1", StandardTag::Artist),
    ("TPE2", StandardTag::AlbumArtist),
    ("TPE3", StandardTag::Conductor),
    ("TCOM", StandardTag::Composer),
    ("TEXT", StandardTag::Lyricist),
    ("TPUB", StandardTag::Publisher),
    ("TCOP", StandardTag::Copyright),
    ("TENC", StandardTag::EncodedBy),
    ("TSSE", StandardTag::EncoderSettings),
    ("TSRC", StandardTag::Isrc),
    ("TLAN", StandardTag::Language),
    ("TMOO", StandardTag::Mood),
    ("MVNM", StandardTag::MovementName),
];

/// `TXXX` descriptions normalized to a text [`StandardTag`].
const TXXX_TEXT: [(&str, TextTag); 4] = [
    ("ALBUMARTIST", StandardTag::AlbumArtist),
    ("WORK", StandardTag::Work),
    ("BARCODE", StandardTag::Barcode),
    ("CATALOGNUMBER", StandardTag::CatalogNumber),
];

/// Value of a byte offset field of `CHAP` that means "not set".
const NO_OFFSET: u32 = u32::MAX;

/// ID3v2.4 equivalent of an ID3v2.2 identifier.
pub(super) fn upgrade_id(id: [u8; 3]) -> Option<[u8; 4]> {
    UPGRADES
        .iter()
        .find(|(old, _)| **old == id)
        .map(|(_, new)| **new)
}

/// Result of reading the content of a frame: `None` if it is malformed,
/// otherwise the result of adding it.
type Added = Option<Result<(), ErrorKind>>;

/// Adds the content of `frame` to `out`.
pub(super) fn add_frame(
    frame: &Frame<'_>,
    format: Format,
    depth: usize,
    out: &mut Frames,
) -> Result<(), ErrorKind> {
    let added = match frame.id {
        "CHAP" | "CTOC" if depth >= MAX_SUBFRAME_DEPTH => return Ok(()),
        "CHAP" => chapter(frame.payload, format, depth, out),
        "CTOC" => table_of_contents(frame.payload, out),
        "APIC" => picture(frame, out),
        "TXXX" => user_text(frame.payload, out),
        "WXXX" => user_url(frame.payload, out),
        "COMM" | "USLT" => comment(frame.id, frame.payload, out),
        "RVA2" => volume(frame.payload, out),
        id if id.starts_with('T') => text_frame(id, frame.payload, out),
        id if id.starts_with('W') => {
            let url = latin1(split_nul(frame.payload).0).into_owned();
            Some(push(out, new_tag(id, RawValue::Text(url), None)))
        }
        _ => None,
    };
    match added {
        Some(result) => result,
        None => {
            // Unknown or malformed content is kept as it is.
            let value = RawValue::Binary(frame.payload.to_vec());
            push(out, new_tag(frame.id, value, None))
        }
    }
}

/// Adds `tag` to the revision of `out`.
fn push(out: &mut Frames, tag: Tag) -> Result<(), ErrorKind> {
    out.take_entry()?;
    out.revision.tags.push(tag);
    Ok(())
}

/// A text frame other than `TXXX`.
fn text_frame(id: &str, payload: &[u8], out: &mut Frames) -> Added {
    let (&encoding, text) = payload.split_first()?;
    let encoding = Encoding::new(encoding)?;
    let values = match strings(encoding, text, &mut None) {
        Ok(values) => values,
        Err(kind) => return Some(Err(kind)),
    };
    let first = values[0].as_str();
    let standard = match id {
        "TRCK" => parse_position(first).map(StandardTag::TrackNumber),
        "TPOS" => parse_position(first).map(StandardTag::DiscNumber),
        "MVIN" => parse_position(first).map(StandardTag::MovementNumber),
        "TBPM" => bpm(first).map(StandardTag::Bpm),
        "TCMP" => match first {
            "1" => Some(StandardTag::Compilation(true)),
            "0" => Some(StandardTag::Compilation(false)),
            _ => None,
        },
        "TYER" | "TDRC" => iso_date(first).map(StandardTag::RecordingDate),
        "TDRL" => iso_date(first).map(StandardTag::ReleaseDate),
        "TCON" => genre(first).map(StandardTag::Genre),
        _ => TEXT_FRAMES
            .iter()
            .find(|(key, _)| *key == id)
            .filter(|_| !first.is_empty())
            .map(|(_, make)| make(first.to_owned())),
    };
    Some(push(out, new_tag(id, raw_value(values), standard)))
}

/// A positive, finite tempo such as `"128"` or `"128.5"`.
fn bpm(text: &str) -> Option<f32> {
    let value = text.trim_ascii().parse::<f64>().ok()?;
    let bpm = value as f32;
    (value > 0.0 && bpm.is_finite()).then_some(bpm)
}

/// `TXXX`: description and value(s).
fn user_text(payload: &[u8], out: &mut Frames) -> Added {
    let (&encoding, rest) = payload.split_first()?;
    let encoding = Encoding::new(encoding)?;
    let mut order = None;
    let (description, rest) = string(encoding, rest, &mut order);
    let values = match strings(encoding, rest, &mut order) {
        Ok(values) => values,
        Err(kind) => return Some(Err(kind)),
    };
    let key = described_key("TXXX", &description);
    let first = values[0].as_str();
    if description.eq_ignore_ascii_case("iTunSMPB") {
        let first = values[0].clone();
        return Some(gapless(&key, raw_value(values), &first, out));
    }
    let standard = match description.to_ascii_uppercase().as_str() {
        "REPLAYGAIN_TRACK_GAIN" => gain(first).map(StandardTag::ReplayGainTrackGainDb),
        "REPLAYGAIN_ALBUM_GAIN" => gain(first).map(StandardTag::ReplayGainAlbumGainDb),
        "REPLAYGAIN_TRACK_PEAK" => parse_float(first).map(StandardTag::ReplayGainTrackPeak),
        "REPLAYGAIN_ALBUM_PEAK" => parse_float(first).map(StandardTag::ReplayGainAlbumPeak),
        upper => TXXX_TEXT
            .iter()
            .find(|(key, _)| *key == upper)
            .filter(|_| !first.is_empty())
            .map(|(_, make)| make(first.to_owned())),
    };
    Some(push(out, new_tag(&key, raw_value(values), standard)))
}

/// A ReplayGain value such as `"-6.42 dB"`, `"+3.21dB"` or `"-6,42 dB"`.
fn gain(text: &str) -> Option<f32> {
    let text = text.trim_ascii();
    let number = match text.len().checked_sub(2) {
        Some(at) if text.is_char_boundary(at) && text[at..].eq_ignore_ascii_case("dB") => {
            &text[..at]
        }
        _ => text,
    };
    parse_float(number)
}

/// Adds the two tags of an `iTunSMPB` value, or one raw tag if it is not
/// well formed.
fn gapless(key: &str, raw: RawValue, text: &str, out: &mut Frames) -> Result<(), ErrorKind> {
    let mut words = text.split_ascii_whitespace();
    let word = |word: Option<&str>| {
        word.filter(|w| w.len() == 8)
            .and_then(|w| u32::from_str_radix(w, 16).ok())
    };
    let _reserved = words.next();
    let delay = word(words.next());
    let padding = word(words.next());
    match delay.zip(padding) {
        Some((delay, padding)) => {
            push(
                out,
                new_tag(key, raw.clone(), Some(StandardTag::EncoderDelay(delay))),
            )?;
            push(
                out,
                new_tag(key, raw, Some(StandardTag::EncoderPadding(padding))),
            )
        }
        None => push(out, new_tag(key, raw, None)),
    }
}

/// `WXXX`: description and a Latin-1 URL.
fn user_url(payload: &[u8], out: &mut Frames) -> Added {
    let (&encoding, rest) = payload.split_first()?;
    let encoding = Encoding::new(encoding)?;
    let (description, url) = string(encoding, rest, &mut None);
    let url = latin1(split_nul(url).0).into_owned();
    let key = described_key("WXXX", &description);
    Some(push(out, new_tag(&key, RawValue::Text(url), None)))
}

/// `COMM` and `USLT`: language, description and text. A byte order mark of
/// the description also applies to the text.
fn comment(id: &str, payload: &[u8], out: &mut Frames) -> Added {
    let (&encoding, rest) = payload.split_first()?;
    let encoding = Encoding::new(encoding)?;
    let rest = rest.get(3..)?; // language
    let mut order = None;
    let (description, rest) = string(encoding, rest, &mut order);
    let (text, _) = string(encoding, rest, &mut order);
    let key = described_key(id, &description);
    if id == "COMM" && description.eq_ignore_ascii_case("iTunSMPB") {
        return Some(gapless(&key, RawValue::Text(text.clone()), &text, out));
    }
    let standard = match id {
        "USLT" => Some(StandardTag::Lyrics(text.clone())),
        _ if description.is_empty() => Some(StandardTag::Comment(text.clone())),
        _ => None,
    };
    Some(push(out, new_tag(&key, RawValue::Text(text), standard)))
}

/// `RVA2`: identification and per-channel adjustments; the master volume
/// channel (type 1) gives the ReplayGain gain.
fn volume(payload: &[u8], out: &mut Frames) -> Added {
    let (identification, rest) = split_nul(payload);
    let identification = latin1(identification).into_owned();
    let mut rest = rest?;
    let mut master = None;
    while let [channel, gain_hi, gain_lo, peak_bits, tail @ ..] = rest {
        let peak_len = usize::from(*peak_bits).div_ceil(8);
        let gain = f32::from(i16::from_be_bytes([*gain_hi, *gain_lo])) / 512.0;
        if *channel == 1 && master.is_none() {
            master = Some(gain);
        }
        rest = tail.get(peak_len..)?;
    }
    let standard = master.map(|gain| {
        if identification.eq_ignore_ascii_case("album") {
            StandardTag::ReplayGainAlbumGainDb(gain)
        } else {
            StandardTag::ReplayGainTrackGainDb(gain)
        }
    });
    let key = described_key("RVA2", &identification);
    let value = RawValue::Binary(payload.to_vec());
    Some(push(out, new_tag(&key, value, standard)))
}

/// `APIC` (and `PIC` with a three-character image format).
fn picture(frame: &Frame<'_>, out: &mut Frames) -> Added {
    let (&encoding, rest) = frame.payload.split_first()?;
    let encoding = Encoding::new(encoding)?;
    let (media_type, rest) = if frame.short_id {
        let (format, rest) = rest.split_first_chunk::<3>()?;
        let media_type = match &format.map(|b| b.to_ascii_uppercase()) {
            b"JPG" => "image/jpeg".to_owned(),
            b"PNG" => "image/png".to_owned(),
            b"-->" => "-->".to_owned(),
            other => format!("image/{}", latin1(other).to_lowercase()),
        };
        (media_type, rest)
    } else {
        let (mime, rest) = split_nul(rest);
        (media_type(&latin1(mime)), rest?)
    };
    let (&kind, rest) = rest.split_first()?;
    let (description, data) = string(encoding, rest, &mut None);
    if let Err(kind) = out.take_entry() {
        return Some(Err(kind));
    }
    out.revision.visuals.push(Visual {
        kind: VisualKind::from_code(u32::from(kind)),
        media_type,
        description,
        width: None,
        height: None,
        color_depth: None,
        indexed_colors: None,
        data: data.to_vec(),
    });
    Some(Ok(()))
}

/// Lowercase media type of an `APIC` frame; `image/jpg` and the bare `jpg`
/// and `png` of some writers become `image/jpeg` and `image/png`.
fn media_type(mime: &str) -> String {
    let lower = mime.trim_ascii().to_ascii_lowercase();
    match lower.as_str() {
        "jpg" | "jpeg" | "image/jpg" => "image/jpeg".to_owned(),
        "png" => "image/png".to_owned(),
        _ => lower,
    }
}

/// Reads a `0x00`-terminated Latin-1 element identifier.
fn element_id(bytes: &[u8]) -> Option<(String, &[u8])> {
    let (id, rest) = split_nul(bytes);
    Some((latin1(id).into_owned(), rest?))
}

/// `CHAP`: a chapter with its times, byte offsets and sub-frames. The
/// chapter and its sub-frames take their entries from the room of `out`.
fn chapter(payload: &[u8], format: Format, depth: usize, out: &mut Frames) -> Added {
    let (id, rest) = element_id(payload)?;
    let (times, sub_frames) = rest.split_first_chunk::<16>()?;
    let field = |index: usize| {
        let bytes = times.as_chunks::<4>().0[index];
        u32::from_be_bytes(bytes)
    };
    let offset = |value: u32| (value != NO_OFFSET).then_some(u64::from(value));
    if let Err(kind) = out.take_entry() {
        return Some(Err(kind));
    }
    let mut inner = Frames::new(out.room);
    let parsed = parse_frames(sub_frames, format, depth + 1, &mut inner);
    out.room = inner.room;
    if let Err(kind) = parsed {
        return Some(Err(kind));
    }
    out.chapters.push(Chapter {
        id,
        start: Duration::from_millis(field(0).into()),
        end: Some(Duration::from_millis(field(1).into())),
        byte_offset_start: offset(field(2)),
        byte_offset_end: offset(field(3)),
        tags: inner.revision.tags,
        visuals: inner.revision.visuals,
    });
    Some(Ok(()))
}

/// `CTOC`: remembers the order of the first top-level ordered table.
fn table_of_contents(payload: &[u8], out: &mut Frames) -> Added {
    const TOP_LEVEL: u8 = 0x02;
    const ORDERED: u8 = 0x01;
    let (_, rest) = element_id(payload)?;
    let (&[flags, count], mut rest) = rest.split_first_chunk::<2>()?;
    let mut children = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let (child, after) = element_id(rest)?;
        children.push(child);
        rest = after;
    }
    if flags & (TOP_LEVEL | ORDERED) == TOP_LEVEL | ORDERED && out.order.is_none() {
        out.order = Some(children);
    }
    Some(Ok(()))
}

/// `chapters` in the order of `order`; chapters it does not list follow in
/// their original order.
pub(super) fn ordered(mut chapters: Vec<Chapter>, order: &[String]) -> Vec<Chapter> {
    let mut sorted = Vec::with_capacity(chapters.len());
    for id in order {
        if let Some(index) = chapters.iter().position(|chapter| chapter.id == *id) {
            sorted.push(chapters.remove(index));
        }
    }
    sorted.append(&mut chapters);
    sorted
}
