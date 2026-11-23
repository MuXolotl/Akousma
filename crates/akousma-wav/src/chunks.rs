//! Metadata chunks: `LIST`, `id3 `, `bext`, `cue `, `smpl`, `acid`, `inst`,
//! `iXML`, `axml` and `cart`.
//!
//! Raw keys of the fields read from these chunks:
//!
//! | Chunk | Raw keys | Normalized |
//! | :--- | :--- | :--- |
//! | `LIST` of type `INFO` | the sub-chunk identifiers (`INAM`, `IART`, ...) | see `akousma_meta::riff` |
//! | `id3 `, `ID3 `, `id32` | the ID3v2 frame identifiers | see `akousma_meta::id3v2` |
//! | `bext` | `BEXT:DESCRIPTION`, `BEXT:ORIGINATOR`, `BEXT:ORIGINATOR_REFERENCE`, `BEXT:ORIGINATION_DATE`, `BEXT:ORIGINATION_TIME`, `BEXT:TIME_REFERENCE`, `BEXT:VERSION`, `BEXT:UMID`, `BEXT:LOUDNESS_VALUE`, `BEXT:LOUDNESS_RANGE`, `BEXT:MAX_TRUE_PEAK_LEVEL`, `BEXT:MAX_MOMENTARY_LOUDNESS`, `BEXT:MAX_SHORT_TERM_LOUDNESS`, `BEXT:CODING_HISTORY` | description, originator as encoder |
//! | `smpl` | `SMPL:MIDI_UNITY_NOTE`, `SMPL:LOOP_{i}_START`, `_END`, `_TYPE`, `_PLAY_COUNT` | none |
//! | `acid` | `ACID:TEMPO_BPM`, `ACID:BEATS`, `ACID:METER` | tempo |
//! | `inst` | `INST:UNSHIFTED_NOTE`, `INST:FINE_TUNE`, `INST:GAIN`, `INST:LOW_NOTE`, `INST:HIGH_NOTE`, `INST:LOW_VELOCITY`, `INST:HIGH_VELOCITY` | none |
//! | `iXML`, `axml` | `IXML`, `AXML` (text) | none |
//! | `cart` | `CART` (bytes) | none |
//!
//! Cue points of the `data` chunk become chapters; the `labl`, `note` and
//! `ltxt` entries of a `LIST` of type `adtl` give their titles, notes and
//! lengths. Of several cue points with the same identifier only the first
//! is kept, and of several entries of one kind for the same identifier only
//! the first, so a chapter has at most one title, one note and one region.
//!
//! **Order.** The revisions follow the chunks: the fields of consecutive
//! container chunks (all but `id3 `) form one revision, those of
//! consecutive `id3 ` chunks another, and a chunk of the other kind starts a
//! new revision. `INFO`, `id3 `, `INFO` thus gives three revisions, in this
//! order. An `id3 ` chunk gives a revision even when it has no fields.
//!
//! **Budget.** Every field, picture and chapter kept, every label and every
//! cue point takes one entry from the [`MetadataBudget`] shared with the
//! tags around the container (an `ltxt` entry counts once, also when its
//! text is empty); the demuxer takes the bytes of each chunk read. More
//! than the budget is [`ErrorKind::LimitExceeded`].

use std::collections::{HashMap, HashSet};

use akousma_core::meta::{
    Chapter, MetadataBudget, MetadataRevision, RawTag, RawValue, StandardTag, Tag,
};
use akousma_core::{Error, ErrorKind, TimeBase};
use akousma_meta::encoding::utf8_or_windows1252;
use akousma_meta::{id3v2, riff};

/// Length of a `bext` chunk without the coding history.
const BEXT_LEN: usize = 602;
/// Length of a cue point.
const CUE_POINT_LEN: usize = 24;
/// Fixed part of a `smpl` chunk and length of one loop.
const SMPL_LEN: usize = 36;
const SMPL_LOOP_LEN: usize = 24;
/// Length of an `acid` chunk.
const ACID_LEN: usize = 24;
/// Length of an `inst` chunk.
const INST_LEN: usize = 7;
/// Header of an `ltxt` entry before its text.
const LTXT_HEADER_LEN: usize = 20;
/// Loudness value marking "not measured" in `bext` version 2.
const LOUDNESS_UNSET: i16 = 0x7FFF;

/// A cue point of the `data` chunk.
#[derive(Clone, Debug)]
struct CuePoint {
    id: u32,
    frame: u64,
}

/// The first `labl`, `note` and `ltxt` entries of a `LIST` of type `adtl`
/// for one cue point identifier.
#[derive(Clone, Debug, Default)]
struct Labels {
    title: Option<String>,
    note: Option<String>,
    /// Length in frames, and the `LTXT` text if the entry has one.
    region: Option<(u32, String)>,
}

/// Kind of chunk a revision comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Source {
    /// Container chunks: `LIST`, `bext`, `smpl` and the others.
    Container,
    /// `id3 ` chunks.
    Id3,
}

/// Metadata collected while scanning the chunks.
#[derive(Debug)]
pub(crate) struct ChunkMeta<'b> {
    budget: &'b mut MetadataBudget,
    /// Revisions in chunk order, with the kind of their chunks.
    revisions: Vec<(Source, MetadataRevision)>,
    id3_chapters: Vec<Chapter>,
    /// Cue points in file order, the first one of each identifier.
    cues: Vec<CuePoint>,
    cue_ids: HashSet<u32>,
    labels: HashMap<u32, Labels>,
}

/// Reads `u16` and `u32` fields in the container's byte order.
#[derive(Copy, Clone)]
struct Order {
    big_endian: bool,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> u16 {
        let b = [b[at], b[at + 1]];
        if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    }

    fn u32(self, b: &[u8], at: usize) -> u32 {
        let b = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    }
}

/// Text without trailing `0x00` bytes and spaces, or `None` if empty.
fn text(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().rposition(|&b| b != 0 && b != b' ')? + 1;
    Some(utf8_or_windows1252(&bytes[..end]).into_owned())
}

impl<'b> ChunkMeta<'b> {
    /// Collects metadata within `budget`.
    pub(crate) fn new(budget: &'b mut MetadataBudget) -> Self {
        Self {
            budget,
            revisions: Vec::new(),
            id3_chapters: Vec::new(),
            cues: Vec::new(),
            cue_ids: HashSet::new(),
            labels: HashMap::new(),
        }
    }

    /// The budget shared by the metadata of the source.
    pub(crate) const fn budget(&mut self) -> &mut MetadataBudget {
        self.budget
    }

    /// The revision that the fields of a chunk of kind `source` go to.
    fn revision(&mut self, source: Source) -> &mut MetadataRevision {
        if self
            .revisions
            .last()
            .is_none_or(|(last, _)| *last != source)
        {
            self.revisions.push((source, MetadataRevision::default()));
        }
        // Not empty: a revision was pushed if there was none.
        let last = self.revisions.len() - 1;
        &mut self.revisions[last].1
    }

    fn push(
        &mut self,
        key: String,
        value: RawValue,
        standard: Option<StandardTag>,
    ) -> Result<(), Error> {
        self.budget.take_entries(1)?;
        self.revision(Source::Container).tags.push(Tag {
            raw: RawTag { key, value },
            standard,
        });
        Ok(())
    }

    fn push_int(&mut self, key: String, value: impl Into<i64>) -> Result<(), Error> {
        self.push(key, RawValue::Int(value.into()), None)
    }

    /// A `LIST` chunk: `INFO` fields or `adtl` labels. Other list types are
    /// ignored.
    pub(crate) fn list(&mut self, payload: &[u8], big_endian: bool) -> Result<(), Error> {
        match payload.first_chunk::<4>() {
            Some(&riff::LIST_TYPE) => {
                let revision = if big_endian {
                    riff::parse_list_info(&little_endian_sizes(payload))?
                } else {
                    riff::parse_list_info(payload)?
                };
                self.budget.take_revision(&revision)?;
                let container = self.revision(Source::Container);
                container.tags.extend(revision.tags);
                container.visuals.extend(revision.visuals);
                Ok(())
            }
            Some(b"adtl") => self.adtl(&payload[4..], Order { big_endian }),
            _ => Ok(()),
        }
    }

    fn adtl(&mut self, mut body: &[u8], order: Order) -> Result<(), Error> {
        while let Some((header, rest)) = body.split_first_chunk::<8>() {
            let size = order.u32(header, 4) as usize;
            let Some(entry) = rest.get(..size) else { break };
            body = rest.get(size + (size & 1)..).unwrap_or_default();
            let kind = &header[..4];
            let known = matches!((kind, entry.len()), (b"labl" | b"note", 4..))
                || matches!((kind, entry.len()), (b"ltxt", LTXT_HEADER_LEN..));
            if !known {
                continue;
            }
            let id = order.u32(entry, 0);
            let labels = self.labels.entry(id).or_default();
            // Only the first entry of each kind counts; later ones are not
            // even decoded.
            let free = match kind {
                b"labl" => labels.title.is_none(),
                b"note" => labels.note.is_none(),
                _ => labels.region.is_none(),
            };
            if !free {
                continue;
            }
            self.budget.take_entries(1)?;
            match kind {
                b"labl" => labels.title = text(&entry[4..]),
                b"note" => labels.note = text(&entry[4..]),
                _ => {
                    let name = text(&entry[LTXT_HEADER_LEN..]).unwrap_or_default();
                    labels.region = Some((order.u32(entry, 4), name));
                }
            }
        }
        Ok(())
    }

    /// An `id3 ` chunk (or a leading ID3v2 tag). The fields of consecutive
    /// `id3 ` chunks are merged; their chapters are all kept.
    pub(crate) fn id3(&mut self, payload: &[u8]) -> Result<(), Error> {
        let tag = id3v2::parse_with_budget(payload, self.budget)?;
        let revision = self.revision(Source::Id3);
        revision.tags.extend(tag.revision.tags);
        revision.visuals.extend(tag.revision.visuals);
        self.id3_chapters.extend(tag.chapters);
        Ok(())
    }

    /// A `bext` chunk (EBU Tech 3285, versions 0 to 2). Shorter chunks are
    /// ignored.
    pub(crate) fn bext(&mut self, b: &[u8]) -> Result<(), Error> {
        if b.len() < BEXT_LEN {
            return Ok(());
        }
        let le16 = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let fields: [(&str, core::ops::Range<usize>); 5] = [
            ("DESCRIPTION", 0..256),
            ("ORIGINATOR", 256..288),
            ("ORIGINATOR_REFERENCE", 288..320),
            ("ORIGINATION_DATE", 320..330),
            ("ORIGINATION_TIME", 330..338),
        ];
        for (name, range) in fields {
            if let Some(value) = text(&b[range]) {
                let standard = match name {
                    "DESCRIPTION" => Some(StandardTag::Description(value.clone())),
                    "ORIGINATOR" => Some(StandardTag::EncodedBy(value.clone())),
                    _ => None,
                };
                self.push(format!("BEXT:{name}"), RawValue::Text(value), standard)?;
            }
        }
        let time_reference = u64::from_le_bytes(
            *b[338..346]
                .first_chunk::<8>()
                .ok_or(ErrorKind::InvalidData)?,
        );
        if let Ok(time_reference) = i64::try_from(time_reference) {
            self.push_int("BEXT:TIME_REFERENCE".into(), time_reference)?;
        }
        let version = le16(346);
        self.push_int("BEXT:VERSION".into(), version)?;
        if version >= 1 && b[348..412].iter().any(|&x| x != 0) {
            let umid = b[348..412].iter().map(|x| format!("{x:02X}")).collect();
            self.push("BEXT:UMID".into(), RawValue::Text(umid), None)?;
        }
        if version >= 2 {
            let loudness = [
                "LOUDNESS_VALUE",
                "LOUDNESS_RANGE",
                "MAX_TRUE_PEAK_LEVEL",
                "MAX_MOMENTARY_LOUDNESS",
                "MAX_SHORT_TERM_LOUDNESS",
            ];
            for (i, name) in loudness.into_iter().enumerate() {
                let value = le16(412 + 2 * i) as i16;
                let min = if name == "LOUDNESS_RANGE" { 0 } else { -9999 };
                if value != LOUDNESS_UNSET && (min..=9999).contains(&value) {
                    self.push(
                        format!("BEXT:{name}"),
                        RawValue::Float(f64::from(value) / 100.0),
                        None,
                    )?;
                }
            }
        }
        if let Some(history) = text(&b[BEXT_LEN..]) {
            self.push("BEXT:CODING_HISTORY".into(), RawValue::Text(history), None)?;
        }
        Ok(())
    }

    /// A `cue ` chunk: the points that refer to the `data` chunk.
    pub(crate) fn cue(&mut self, b: &[u8], big_endian: bool) -> Result<(), Error> {
        let order = Order { big_endian };
        let Some(points) = b.get(4..) else {
            return Ok(());
        };
        let declared = order.u32(b, 0) as usize;
        // Padded chunks hold more room than points, truncated ones fewer.
        for point in points.as_chunks::<CUE_POINT_LEN>().0.iter().take(declared) {
            let id = order.u32(point, 0);
            if &point[8..12] != b"data" || self.cue_ids.contains(&id) {
                continue;
            }
            self.budget.take_entries(1)?;
            self.cue_ids.insert(id);
            self.cues.push(CuePoint {
                id,
                frame: u64::from(order.u32(point, 20)),
            });
        }
        Ok(())
    }

    /// A `smpl` chunk: the MIDI unity note and the loops.
    pub(crate) fn smpl(&mut self, b: &[u8], big_endian: bool) -> Result<(), Error> {
        let order = Order { big_endian };
        if b.len() < SMPL_LEN {
            return Ok(());
        }
        self.push_int("SMPL:MIDI_UNITY_NOTE".into(), order.u32(b, 12))?;
        let declared = order.u32(b, 28) as usize;
        for (i, lp) in b[SMPL_LEN..]
            .as_chunks::<SMPL_LOOP_LEN>()
            .0
            .iter()
            .take(declared)
            .enumerate()
        {
            self.push_int(format!("SMPL:LOOP_{i}_TYPE"), order.u32(lp, 4))?;
            self.push_int(format!("SMPL:LOOP_{i}_START"), order.u32(lp, 8))?;
            self.push_int(format!("SMPL:LOOP_{i}_END"), order.u32(lp, 12))?;
            self.push_int(format!("SMPL:LOOP_{i}_PLAY_COUNT"), order.u32(lp, 20))?;
        }
        Ok(())
    }

    /// An `acid` chunk: tempo, number of beats and meter.
    pub(crate) fn acid(&mut self, b: &[u8], big_endian: bool) -> Result<(), Error> {
        let order = Order { big_endian };
        if b.len() < ACID_LEN {
            return Ok(());
        }
        let tempo = f32::from_bits(order.u32(b, 20));
        if tempo.is_finite() && tempo > 0.0 {
            self.push(
                "ACID:TEMPO_BPM".into(),
                RawValue::Float(f64::from(tempo)),
                Some(StandardTag::Bpm(tempo)),
            )?;
        }
        self.push_int("ACID:BEATS".into(), order.u32(b, 12))?;
        let meter = format!("{}/{}", order.u16(b, 18), order.u16(b, 16));
        self.push("ACID:METER".into(), RawValue::Text(meter), None)
    }

    /// An `inst` chunk: the instrument parameters.
    pub(crate) fn inst(&mut self, b: &[u8]) -> Result<(), Error> {
        let Some(
            &[
                note,
                fine_tune,
                gain,
                low_note,
                high_note,
                low_velocity,
                high_velocity,
            ],
        ) = b.first_chunk::<INST_LEN>()
        else {
            return Ok(());
        };
        self.push_int("INST:UNSHIFTED_NOTE".into(), note)?;
        self.push_int("INST:FINE_TUNE".into(), fine_tune as i8)?;
        self.push_int("INST:GAIN".into(), gain as i8)?;
        self.push_int("INST:LOW_NOTE".into(), low_note)?;
        self.push_int("INST:HIGH_NOTE".into(), high_note)?;
        self.push_int("INST:LOW_VELOCITY".into(), low_velocity)?;
        self.push_int("INST:HIGH_VELOCITY".into(), high_velocity)
    }

    /// An `iXML` or `axml` chunk, kept as text under `key`.
    pub(crate) fn xml(&mut self, key: &str, b: &[u8]) -> Result<(), Error> {
        match text(b) {
            Some(value) => self.push(key.into(), RawValue::Text(value), None),
            None => Ok(()),
        }
    }

    /// A `cart` chunk, kept as bytes: its fixed fields mix text and numbers.
    pub(crate) fn cart(&mut self, b: &[u8]) -> Result<(), Error> {
        self.push("CART".into(), RawValue::Binary(b.to_vec()), None)
    }

    /// Returns the revisions in chunk order and the chapters sorted by start
    /// time.
    pub(crate) fn finish(
        self,
        time_base: TimeBase,
        total_frames: u64,
    ) -> Result<(Vec<MetadataRevision>, Vec<Chapter>), Error> {
        let revisions = self.revisions.into_iter().map(|(_, r)| r).collect();

        let mut cues = self.cues;
        cues.sort_by_key(|cue| cue.frame);
        let mut labels = self.labels;
        let mut chapters = self.id3_chapters;
        chapters
            .try_reserve(cues.len())
            .map_err(|_| ErrorKind::LimitExceeded)?;
        for (i, cue) in cues.iter().enumerate() {
            let mut end = cues
                .get(i + 1)
                .map_or(total_frames, |next| next.frame)
                .max(cue.frame);
            let mut tags = Vec::new();
            // Each identifier has one cue point, so its labels are moved out.
            let Labels {
                title,
                note,
                region,
            } = labels.remove(&cue.id).unwrap_or_default();
            let text_tag = |key: &str, value: String, standard| Tag {
                raw: RawTag {
                    key: key.into(),
                    value: RawValue::Text(value),
                },
                standard,
            };
            if let Some(title) = title {
                let standard = Some(StandardTag::TrackTitle(title.clone()));
                tags.push(text_tag("LABL", title, standard));
            }
            if let Some(note) = note {
                let standard = Some(StandardTag::Comment(note.clone()));
                tags.push(text_tag("NOTE", note, standard));
            }
            if let Some((length, name)) = region {
                end = cue.frame.saturating_add(u64::from(length));
                if !name.is_empty() {
                    tags.push(text_tag("LTXT", name, None));
                }
            }
            chapters.push(Chapter {
                id: cue.id.to_string(),
                start: time_base.ts_to_duration(cue.frame),
                end: Some(time_base.ts_to_duration(end)),
                byte_offset_start: None,
                byte_offset_end: None,
                tags,
                visuals: Vec::new(),
            });
        }
        chapters.sort_by_key(|chapter| chapter.start);
        Ok((revisions, chapters))
    }
}

/// Copy of a big-endian `LIST`/`INFO` payload with little-endian sub-chunk
/// sizes, the byte order of the `INFO` parser.
fn little_endian_sizes(payload: &[u8]) -> Vec<u8> {
    let mut out = payload.to_vec();
    let mut pos = 4;
    while let Some(size) = out.get_mut(pos + 4..pos + 8) {
        size.reverse();
        let len = u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize;
        pos = pos
            .saturating_add(8)
            .saturating_add(len)
            .saturating_add(len & 1);
    }
    out
}
