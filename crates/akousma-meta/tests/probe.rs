//! Format detection through `Probe` with the tag scanner of this crate.

use std::io::{Cursor, Read};

use akousma_core::codec::{Packet, Track};
use akousma_core::io::MediaSource;
use akousma_core::meta::{Chapter, MetadataBudget, MetadataLog, RawValue};
use akousma_core::probe::{
    Demuxer, Extent, FormatDescriptor, Hint, Probe, ProbeReader, ScopedSource, Score, Tier,
};
use akousma_core::units::{SeekMode, SeekTarget, SeekedTo};
use akousma_core::{DemuxOptions, Error, ErrorKind};
use akousma_meta::anchors::TagScanner;

/// Demuxer that returns everything it can read as one packet.
struct Raw {
    source: ScopedSource,
    metadata: MetadataLog,
    chapters: Vec<Chapter>,
}

impl Demuxer for Raw {
    fn tracks(&self) -> &[Track] {
        &[]
    }

    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    fn chapters_mut(&mut self) -> &mut Vec<Chapter> {
        &mut self.chapters
    }

    fn metadata(&mut self) -> &mut MetadataLog {
        &mut self.metadata
    }

    fn next_packet(&mut self, packet: &mut Packet) -> Result<bool, Error> {
        packet.data.clear();
        self.source
            .read_to_end(&mut packet.data)
            .map_err(|err| Error::from(ErrorKind::from(err.kind())))?;
        Ok(!packet.data.is_empty())
    }

    fn seek(&mut self, _: SeekMode, _: SeekTarget) -> Result<SeekedTo, Error> {
        Err(ErrorKind::UnsupportedFeature.into())
    }
}

fn open_raw(
    source: ScopedSource,
    _: &DemuxOptions,
    _: &mut MetadataBudget,
) -> Result<Box<dyn Demuxer>, Error> {
    Ok(Box::new(Raw {
        source,
        metadata: MetadataLog::new(),
        chapters: Vec::new(),
    }))
}

/// Container with a signature.
fn container(window: &[u8]) -> Score {
    if window.starts_with(b"BOX!") { 255 } else { 0 }
}

/// Raw stream: 50 points per `0xFFF` sync word at the start of a 4-byte
/// frame, as a stand-in for MPEG audio.
fn frames(window: &[u8]) -> Score {
    let run = window
        .as_chunks::<4>()
        .0
        .iter()
        .take_while(|frame| frame[0] == 0xFF && frame[1] & 0xF0 == 0xF0)
        .count();
    u8::try_from((run * 50).min(255)).unwrap_or(u8::MAX)
}

fn probe() -> Probe {
    let mut probe = Probe::new();
    probe.register(FormatDescriptor {
        name: "frames",
        extensions: &["frm"],
        mime_types: &[],
        tier: Tier::Fallback,
        score: frames,
        open: open_raw,
        extent: None,
    });
    probe.register(FormatDescriptor {
        name: "box",
        extensions: &["box"],
        mime_types: &[],
        tier: Tier::Standard,
        score: container,
        open: open_raw,
        extent: None,
    });
    probe.register(FormatDescriptor {
        name: "sized",
        extensions: &[],
        mime_types: &[],
        tier: Tier::Standard,
        score: sized,
        open: open_raw,
        extent: Some(sized_extent),
    });
    probe.set_anchor_scanner(Box::new(TagScanner));
    probe
}

/// Container with a signature and its total length (`u32` BE, 0 when
/// unknown) in its header.
fn sized(window: &[u8]) -> Score {
    if window.starts_with(b"LEN!") { 255 } else { 0 }
}

fn sized_extent(reader: &mut ProbeReader<'_>) -> Result<Extent, Error> {
    let mut len = [0u8; 4];
    reader.read_exact_at(4, &mut len)?;
    Ok(match u32::from_be_bytes(len) {
        0 => Extent::Unbounded,
        len => Extent::Bounded(u64::from(len)),
    })
}

/// A `LEN!` container whose body is `body`, with its length declared or 0.
fn sized_container(body: &[u8], declared: bool) -> Vec<u8> {
    let len = u32::try_from(body.len() + 8).unwrap();
    let mut out = b"LEN!".to_vec();
    out.extend_from_slice(&(if declared { len } else { 0 }).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn id3v2_title(title: &str) -> Vec<u8> {
    let mut frame = b"TIT2".to_vec();
    frame.extend_from_slice(&u32::try_from(title.len() + 1).unwrap().to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(title.as_bytes());
    let mut tag = vec![b'I', b'D', b'3', 3, 0, 0, 0, 0, 0];
    tag.push(u8::try_from(frame.len()).unwrap());
    tag.extend(frame);
    tag
}

/// An APEv2 tag with a footer only and one `Title` item.
fn ape_title(title: &str) -> Vec<u8> {
    let mut tag = u32::try_from(title.len()).unwrap().to_le_bytes().to_vec();
    tag.extend_from_slice(&0u32.to_le_bytes());
    tag.extend_from_slice(b"Title\0");
    tag.extend_from_slice(title.as_bytes());
    let size = u32::try_from(tag.len() + 32).unwrap();
    tag.extend_from_slice(b"APETAGEX");
    for field in [2000, size, 1, 0, 0, 0] {
        tag.extend_from_slice(&field.to_le_bytes());
    }
    tag
}

fn id3v1_title(title: &str) -> Vec<u8> {
    let mut tag = vec![0u8; 128];
    tag[..3].copy_from_slice(b"TAG");
    tag[3..3 + title.len()].copy_from_slice(title.as_bytes());
    tag[127] = 255;
    tag
}

fn titles(log: &MetadataLog) -> Vec<String> {
    log.iter()
        .map(|revision| match &revision.tags[0].raw.value {
            RawValue::Text(text) => text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn open(file: Vec<u8>, hint: &Hint) -> Result<(akousma_core::probe::Probed, Vec<u8>), Error> {
    let source: Box<dyn MediaSource> = Box::new(Cursor::new(file));
    let mut probed = probe().open(hint, source, &DemuxOptions::new())?;
    let mut packet = Packet::default();
    probed.demuxer.next_packet(&mut packet)?;
    Ok((probed, packet.data))
}

#[test]
fn probe_tiers_and_stacked_tags() {
    // Two ID3v2 tags in front of a container whose body happens to start
    // with sync words, and APEv2 and ID3v1 tags after it.
    let audio = b"BOX!\xFF\xF0\x00\x00\xFF\xF0\x00\x00\xFF\xF0\x00\x00".to_vec();
    let mut file = id3v2_title("first");
    file.extend(id3v2_title("second"));
    file.extend_from_slice(&audio);
    file.extend(ape_title("ape"));
    file.extend(id3v1_title("last"));
    let (mut probed, data) = open(file, &Hint::new().with_extension("frm")).unwrap();
    // The standard tier wins over the hint and the fallback score.
    assert_eq!(probed.format, "box");
    assert_eq!(data, audio);
    assert_eq!(
        titles(probed.demuxer.metadata()),
        ["first", "second", "ape", "last"]
    );

    // A raw stream: accepted from three frames (150 points) on.
    let stream = b"\xFF\xF1\x00\x00\xFF\xF1\x00\x00\xFF\xF1\x00\x00".to_vec();
    let mut file = id3v2_title("lead");
    file.extend_from_slice(&stream);
    let (probed, data) = open(file, &Hint::new()).unwrap();
    assert_eq!(probed.format, "frames");
    assert_eq!(data, stream);

    // Two frames (100 points) reach the threshold; one does not.
    let (probed, _) = open(stream[..8].to_vec(), &Hint::new()).unwrap();
    assert_eq!(probed.format, "frames");
    let err = open(stream[..4].to_vec(), &Hint::new()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::UnsupportedFormat);

    // A false "ID3" is audio: here it hides the container signature.
    let mut file = b"ID3\xBF\x00\x00\x00\x00\x00\x00".to_vec();
    file.extend_from_slice(&audio);
    let err = open(file, &Hint::new()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::UnsupportedFormat);
}

#[test]
fn tag_lookalikes_inside_a_declared_container() {
    // The body ends with bytes that look like an APE tag and then like an
    // ID3v1 tag.
    let mut body = vec![0x11; 16];
    body.extend(ape_title("inside"));
    body.extend(id3v1_title("inside"));

    // Declared length: both stay audio; the real tag after it is found.
    let mut file = id3v2_title("lead");
    let container = sized_container(&body, true);
    file.extend_from_slice(&container);
    file.extend(id3v1_title("outside"));
    let (mut probed, data) = open(file.clone(), &Hint::new()).unwrap();
    assert_eq!(probed.format, "sized");
    assert_eq!(data, container);
    assert_eq!(titles(probed.demuxer.metadata()), ["lead", "outside"]);
    // Without metadata the audio range is the same.
    let source: Box<dyn MediaSource> = Box::new(Cursor::new(file));
    let options = DemuxOptions::new().with_read_metadata(false);
    let mut probed = probe().open(&Hint::new(), source, &options).unwrap();
    let mut packet = Packet::default();
    probed.demuxer.next_packet(&mut packet).unwrap();
    assert_eq!(packet.data, container);
    assert!(probed.demuxer.metadata().is_empty());

    // Unknown length (a streamed container): the whole rest of the source is
    // searched, and the lookalikes are taken for tags.
    let container = sized_container(&body, false);
    let (mut probed, data) = open(container.clone(), &Hint::new()).unwrap();
    assert_eq!(data, container[..24]);
    assert_eq!(titles(probed.demuxer.metadata()), ["inside", "inside"]);
}
