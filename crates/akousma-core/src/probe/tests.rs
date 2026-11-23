use super::*;
use crate::FormatLimits;
use crate::meta::{RawTag, RawValue, Tag};
use std::io::{Cursor, Read};

/// Demuxer that returns its whole source as one packet.
struct WholeDemuxer {
    source: ScopedSource,
    metadata: MetadataLog,
    chapters: Vec<Chapter>,
    done: bool,
}

impl Demuxer for WholeDemuxer {
    fn tracks(&self) -> &[Track] {
        &[]
    }

    fn metadata(&mut self) -> &mut MetadataLog {
        &mut self.metadata
    }

    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    fn chapters_mut(&mut self) -> &mut Vec<Chapter> {
        &mut self.chapters
    }

    fn next_packet(&mut self, packet: &mut Packet) -> Result<bool, Error> {
        if self.done {
            return Ok(false);
        }
        self.done = true;
        packet.data.clear();
        self.source
            .read_to_end(&mut packet.data)
            .map_err(|err| Error::from(ErrorKind::from(err.kind())))?;
        Ok(true)
    }

    fn seek(&mut self, _: SeekMode, _: SeekTarget) -> Result<SeekedTo, Error> {
        Err(ErrorKind::UnsupportedFeature.into())
    }
}

/// Opens a [`WholeDemuxer`] with one revision, whose second tag records the
/// metadata bytes it was left, and one chapter.
fn open_whole(
    source: ScopedSource,
    _: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<Box<dyn Demuxer>, Error> {
    let mut metadata = MetadataLog::new();
    let mut container = revision("container");
    container.tags.push(Tag {
        raw: RawTag {
            key: "BUDGET".into(),
            value: RawValue::Int(i64::try_from(budget.remaining_bytes()).unwrap()),
        },
        standard: None,
    });
    metadata.push(container);
    Ok(Box::new(WholeDemuxer {
        source,
        metadata,
        chapters: vec![chapter("container")],
        done: false,
    }))
}

/// Opens a [`WholeDemuxer`] without metadata or chapters of its own.
fn open_bare(
    source: ScopedSource,
    _: &DemuxOptions,
    _: &mut MetadataBudget,
) -> Result<Box<dyn Demuxer>, Error> {
    Ok(Box::new(WholeDemuxer {
        source,
        metadata: MetadataLog::new(),
        chapters: Vec::new(),
        done: false,
    }))
}

/// The metadata bytes the demuxer opened by [`open_whole`] was left.
fn budget_seen(demuxer: &mut dyn Demuxer) -> i64 {
    let container = demuxer
        .metadata()
        .iter()
        .find(|r| r.tags.len() == 2)
        .cloned()
        .unwrap();
    match container.tags[1].raw.value {
        RawValue::Int(bytes) => bytes,
        _ => unreachable!(),
    }
}

fn chapter(id: &str) -> Chapter {
    Chapter {
        id: id.into(),
        start: core::time::Duration::ZERO,
        end: None,
        byte_offset_start: None,
        byte_offset_end: None,
        tags: Vec::new(),
        visuals: Vec::new(),
    }
}

fn revision(title: &str) -> MetadataRevision {
    MetadataRevision {
        tags: vec![Tag {
            raw: RawTag {
                key: "TITLE".into(),
                value: RawValue::Text(title.into()),
            },
            standard: None,
        }],
        ..MetadataRevision::default()
    }
}

fn format(name: &'static str, tier: Tier, score: fn(&[u8]) -> Score) -> FormatDescriptor {
    FormatDescriptor {
        name,
        extensions: &[],
        mime_types: &[],
        tier,
        score,
        open: open_whole,
        extent: None,
    }
}

fn riff(window: &[u8]) -> Score {
    if window.starts_with(b"RIFF") { 255 } else { 0 }
}

/// Scores 40 per `0xFF` byte at the start, like a chain of frames.
fn frames(window: &[u8]) -> Score {
    let run = window.iter().take_while(|&&b| b == 0xFF).count();
    (run * 40).min(255) as Score
}

fn any(_: &[u8]) -> Score {
    1
}

#[test]
fn detect_tiers_scores_and_hints() {
    let mut probe = Probe::new();
    probe.register(format("frames", Tier::Fallback, frames));
    probe.register(format("riff", Tier::Standard, riff));
    let none = Hint::new();
    // A standard match beats any fallback score.
    let mut window = b"RIFF".to_vec();
    window.extend_from_slice(&[0xFF; 8]);
    assert_eq!(probe.detect(&none, &window).map(|f| f.name), Some("riff"));
    // Fallback only from the threshold: 2 frames (80) are not enough.
    assert_eq!(probe.detect(&none, &[0xFF, 0xFF, 0]).map(|f| f.name), None);
    assert_eq!(
        probe.detect(&none, &[0xFF, 0xFF, 0xFF, 0]).map(|f| f.name),
        Some("frames")
    );

    // Equal scores: the hint decides, otherwise the first registered.
    let mut probe = Probe::new();
    probe.register(format("first", Tier::Standard, any));
    let mut second = format("second", Tier::Standard, any);
    second.extensions = &["snd"];
    second.mime_types = &["audio/x-second"];
    probe.register(second);
    assert_eq!(probe.detect(&none, b"").map(|f| f.name), Some("first"));
    let hint = Hint::new().with_extension("SND");
    assert_eq!(probe.detect(&hint, b"").map(|f| f.name), Some("second"));
    let hint = Hint::new().with_mime_type("audio/x-second");
    assert_eq!(probe.detect(&hint, b"").map(|f| f.name), Some("second"));
    // A higher score beats the hint.
    probe.register(format("riff", Tier::Standard, riff));
    let hint = Hint::new().with_extension("snd");
    assert_eq!(probe.detect(&hint, b"RIFF").map(|f| f.name), Some("riff"));
    assert_eq!(probe.formats().len(), 3);
}

/// A trailing block: start, end and contents.
type Trailing = (u64, u64, MetadataRevision);

/// Scanner that reports fixed leading anchors and takes the trailing ones
/// that start at or after the floor, recording the floor it was given. It
/// takes the bytes in front of the audio data and those of every trailing
/// tag from the metadata budget.
struct FixedAnchors {
    anchors: Anchors,
    /// Trailing tags: start, end and revision.
    trailing: Vec<Trailing>,
    floor: std::sync::Mutex<Option<u64>>,
}

impl FixedAnchors {
    fn new(anchors: Anchors, trailing: Vec<Trailing>) -> Self {
        Self {
            anchors,
            trailing,
            floor: std::sync::Mutex::new(None),
        }
    }
}

impl AnchorScanner for FixedAnchors {
    fn scan_leading(
        &self,
        _: &mut dyn MediaSource,
        _: u64,
        _: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<Anchors, Error> {
        budget.take_bytes(self.anchors.audio_start)?;
        Ok(self.anchors.clone())
    }

    fn scan_trailing(
        &self,
        _: &mut dyn MediaSource,
        anchors: &mut Anchors,
        floor: u64,
        _: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<(), Error> {
        *self.floor.lock().unwrap() = Some(floor);
        // From the outside in, as long as each tag ends where the audio
        // range ends and starts at or after the floor.
        let mut found = Vec::new();
        for (start, end, revision) in self.trailing.iter().rev() {
            if *end != anchors.audio_end || *start < floor {
                break;
            }
            budget.take_bytes(end - start)?;
            anchors.audio_end = *start;
            found.push(revision.clone());
        }
        found.reverse();
        anchors.trailing = found;
        Ok(())
    }
}

fn titles(demuxer: &mut dyn Demuxer) -> Vec<String> {
    demuxer
        .metadata()
        .iter()
        .map(|r| match &r.tags[0].raw.value {
            RawValue::Text(text) => text.clone(),
            _ => String::new(),
        })
        .collect()
}

#[test]
fn open_scopes_the_source_and_orders_revisions() {
    let mut probe = Probe::new();
    probe.register(format("riff", Tier::Standard, riff));
    let file = b"HEADRIFFdataTAILtail".to_vec();

    // Without a scanner the window starts at byte 0, where `RIFF` is not.
    let err = probe
        .open(
            &Hint::new(),
            Cursor::new(file.clone()),
            &DemuxOptions::new(),
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::UnsupportedFormat);

    probe.set_anchor_scanner(Box::new(FixedAnchors::new(
        Anchors {
            audio_start: 4,
            audio_end: 20,
            leading: vec![revision("lead 1"), revision("lead 2")],
            trailing: Vec::new(),
            chapters: vec![chapter("tag 1"), chapter("tag 2")],
        },
        vec![(12, 16, revision("tail 1")), (16, 20, revision("tail 2"))],
    )));
    let mut probed = probe
        .open(
            &Hint::new(),
            Cursor::new(file.clone()),
            &DemuxOptions::new(),
        )
        .unwrap();
    assert_eq!(probed.format, "riff");
    assert!(format!("{probed:?}").contains("riff"));
    let mut packet = Packet::default();
    assert!(probed.demuxer.next_packet(&mut packet).unwrap());
    assert_eq!(packet.data, b"RIFFdata");
    // Source order: tags in front, the container, tags after.
    assert_eq!(
        titles(&mut *probed.demuxer),
        ["lead 1", "lead 2", "container", "tail 1", "tail 2"]
    );
    assert!(probed.demuxer.default_track().is_none());
    let ids: Vec<_> = probed
        .demuxer
        .chapters()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, ["tag 1", "tag 2", "container"]);
    // The scanner took the 4 leading and 8 trailing bytes.
    let limit = FormatLimits::DEFAULT_MAX_METADATA_BYTES as i64;
    assert_eq!(budget_seen(&mut *probed.demuxer), limit - 12);

    // Anchors outside the source are rejected.
    probe.set_anchor_scanner(Box::new(FixedAnchors::new(
        Anchors {
            audio_start: 4,
            audio_end: 21,
            ..Anchors::default()
        },
        Vec::new(),
    )));
    let err = probe
        .open(&Hint::new(), Cursor::new(file), &DemuxOptions::new())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidData);
}

#[test]
fn revision_order_without_some_blocks() {
    let file = b"RIFFdataTAIL".to_vec();
    let cases: [(Vec<MetadataRevision>, Vec<Trailing>, &[&str]); 4] = [
        (Vec::new(), Vec::new(), &["container"]),
        (vec![revision("lead")], Vec::new(), &["lead", "container"]),
        (
            Vec::new(),
            vec![(8, 12, revision("tail"))],
            &["container", "tail"],
        ),
        (
            vec![revision("lead")],
            vec![(8, 12, revision("tail"))],
            &["lead", "container", "tail"],
        ),
    ];
    for (leading, trailing, expected) in cases {
        let mut probe = Probe::new();
        probe.register(format("riff", Tier::Standard, riff));
        probe.set_anchor_scanner(Box::new(FixedAnchors::new(
            Anchors {
                audio_start: 0,
                audio_end: 12,
                leading,
                ..Anchors::default()
            },
            trailing,
        )));
        let mut probed = probe
            .open(
                &Hint::new(),
                Cursor::new(file.clone()),
                &DemuxOptions::new(),
            )
            .unwrap();
        assert_eq!(titles(&mut *probed.demuxer), expected);
    }
}

/// Declares a container of 8 bytes.
fn eight_bytes(_: &mut ProbeReader<'_>) -> Result<Extent, Error> {
    Ok(Extent::Bounded(8))
}

/// Declares no length.
fn unbounded(_: &mut ProbeReader<'_>) -> Result<Extent, Error> {
    Ok(Extent::Unbounded)
}

#[test]
fn trailing_tags_only_after_the_declared_container() {
    let file = b"RIFFTAIL".to_vec();
    for (extent, floor, end, tails) in [
        // The container declares all 8 bytes: `TAIL` is inside it.
        (Some(eight_bytes as ExtentFn), 8, 8, 0),
        // Without a declared length the whole range is searched.
        (None, 0, 4, 1),
        (Some(unbounded as ExtentFn), 0, 4, 1),
    ] {
        let mut probe = Probe::new();
        let mut riff_format = format("riff", Tier::Standard, riff);
        riff_format.extent = extent;
        probe.register(riff_format);
        let scanner = std::sync::Arc::new(FixedAnchors::new(
            Anchors {
                audio_start: 0,
                audio_end: 8,
                ..Anchors::default()
            },
            vec![(4, 8, revision("tail"))],
        ));
        probe.set_anchor_scanner(Box::new(SharedScanner(scanner.clone())));
        let mut probed = probe
            .open(
                &Hint::new(),
                Cursor::new(file.clone()),
                &DemuxOptions::new(),
            )
            .unwrap();
        assert_eq!(*scanner.floor.lock().unwrap(), Some(floor));
        let mut packet = Packet::default();
        assert!(probed.demuxer.next_packet(&mut packet).unwrap());
        assert_eq!(packet.data.len(), end);
        assert_eq!(probed.demuxer.metadata().len(), 1 + tails);
    }

    // A container longer than the source (a truncated file): no tag can
    // follow it.
    fn huge(_: &mut ProbeReader<'_>) -> Result<Extent, Error> {
        Ok(Extent::Bounded(u64::MAX))
    }
    let mut probe = Probe::new();
    let mut riff_format = format("riff", Tier::Standard, riff);
    riff_format.extent = Some(huge);
    probe.register(riff_format);
    let scanner = std::sync::Arc::new(FixedAnchors::new(
        Anchors {
            audio_start: 0,
            audio_end: 8,
            ..Anchors::default()
        },
        vec![(4, 8, revision("tail"))],
    ));
    probe.set_anchor_scanner(Box::new(SharedScanner(scanner.clone())));
    probe
        .open(&Hint::new(), Cursor::new(file), &DemuxOptions::new())
        .unwrap();
    assert_eq!(*scanner.floor.lock().unwrap(), Some(8));
}

/// A scanner shared with the test, which reads the floor afterwards.
struct SharedScanner(std::sync::Arc<FixedAnchors>);

impl AnchorScanner for SharedScanner {
    fn scan_leading(
        &self,
        source: &mut dyn MediaSource,
        len: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<Anchors, Error> {
        self.0.scan_leading(source, len, options, budget)
    }

    fn scan_trailing(
        &self,
        source: &mut dyn MediaSource,
        anchors: &mut Anchors,
        floor: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<(), Error> {
        self.0
            .scan_trailing(source, anchors, floor, options, budget)
    }
}

/// Probes `file` for a `RIFF` format with `extent`, a scanner without tags
/// and a probe limit of `limit` bytes.
fn open_limited(
    file: impl MediaSource + 'static,
    len: u64,
    extent: Option<ExtentFn>,
    limit: usize,
) -> Result<Probed, Error> {
    let mut probe = Probe::new();
    let mut riff_format = format("riff", Tier::Standard, riff);
    riff_format.extent = extent;
    probe.register(riff_format);
    probe.set_anchor_scanner(Box::new(FixedAnchors::new(
        Anchors {
            audio_end: len,
            ..Anchors::default()
        },
        Vec::new(),
    )));
    let limits = FormatLimits::new().with_max_probe_bytes(limit);
    probe.open(&Hint::new(), file, &DemuxOptions::new().with_limits(limits))
}

/// Offset of the length read by [`stored_length`]: past the standard window.
const LENGTH_AT: u64 = 20_000;

/// Reads the 8-byte length stored at [`LENGTH_AT`].
fn stored_length(reader: &mut ProbeReader<'_>) -> Result<Extent, Error> {
    let mut field = [0u8; 8];
    reader.read_exact_at(LENGTH_AT, &mut field)?;
    Ok(Extent::Bounded(u64::from_le_bytes(field)))
}

/// `RIFF`, zeros, and the length of the file stored at `at`.
fn riff_with_length(at: u64) -> Vec<u8> {
    let mut file = b"RIFF".to_vec();
    file.resize(usize::try_from(at).unwrap(), 0);
    file.extend_from_slice(&(at + 8).to_le_bytes());
    file
}

#[test]
fn probe_limit_counts_the_bytes_read() {
    let file = riff_with_length(LENGTH_AT);
    let len = file.len() as u64;
    let kind = |file: &[u8], extent, limit| {
        let len = file.len() as u64;
        open_limited(Cursor::new(file.to_vec()), len, extent, limit)
            .map(|probed| probed.format)
            .map_err(|err| (err.kind, err.byte_offset))
    };
    // The signature needs 4 bytes: fewer is a limit error, `0` included.
    for limit in [0, 3] {
        let result = kind(&file, None, limit);
        assert_eq!(result, Err((ErrorKind::LimitExceeded, None)));
    }
    assert_eq!(kind(&file, None, 4), Ok("riff"));
    // The window takes up to 16 KiB of the limit; the length lies past it
    // and needs 8 more bytes. Seeking over the 3600 bytes in between is
    // free.
    let extent = Some(stored_length as ExtentFn);
    let window = PROBE_WINDOW;
    for limit in [4, 12, window, window + 7] {
        let result = kind(&file, extent, limit);
        let expected = Err((ErrorKind::LimitExceeded, Some(LENGTH_AT)));
        assert_eq!(result, expected, "{limit}");
    }
    assert_eq!(kind(&file, extent, window + 8), Ok("riff"));
    assert_eq!(kind(&file, extent, usize::MAX), Ok("riff"));
    // A field inside the window costs nothing more; one that only starts
    // inside it is read from the source, in full.
    fn length_at_100(reader: &mut ProbeReader<'_>) -> Result<Extent, Error> {
        let mut field = [0u8; 8];
        reader.read_exact_at(100, &mut field)?;
        Ok(Extent::Bounded(u64::from_le_bytes(field)))
    }
    let small = riff_with_length(100);
    assert_eq!(kind(&small, Some(length_at_100), 108), Ok("riff"));
    let result = kind(&small, Some(length_at_100), 107);
    assert_eq!(result, Err((ErrorKind::LimitExceeded, Some(100))));
    // Past the end of the range: the header is cut.
    let result = kind(&file[..20_004], extent, usize::MAX);
    assert_eq!(result, Err((ErrorKind::UnexpectedEof, Some(LENGTH_AT))));
    assert_eq!(len, LENGTH_AT + 8);

    // A whole window that matches nothing is not a limit error, and neither
    // is an empty source with a limit of 0.
    let result = kind(b"OggS and more", None, usize::MAX);
    assert_eq!(result, Err((ErrorKind::UnsupportedFormat, None)));
    assert_eq!(
        kind(b"", None, 0),
        Err((ErrorKind::UnsupportedFormat, None))
    );
    // A limit cutting a window that holds no format is a limit error: the
    // format might start beyond it.
    assert_eq!(
        kind(b"OggS", None, 2),
        Err((ErrorKind::LimitExceeded, None))
    );

    // Without an anchor scanner the length is not needed and not read.
    let mut probe = Probe::new();
    let mut riff_format = format("riff", Tier::Standard, riff);
    riff_format.extent = extent;
    probe.register(riff_format);
    let limits = FormatLimits::new().with_max_probe_bytes(4);
    let options = DemuxOptions::new().with_limits(limits);
    assert!(
        probe
            .open(&Hint::new(), Cursor::new(file), &options)
            .is_ok()
    );
}

/// A source whose reads fail from byte `fail_at` on.
struct Failing {
    inner: Cursor<Vec<u8>>,
    fail_at: u64,
}

impl std::io::Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.inner.position() >= self.fail_at {
            return Err(std::io::Error::other("disk error"));
        }
        let room = usize::try_from(self.fail_at - self.inner.position()).unwrap();
        let n = buf.len().min(room);
        self.inner.read(&mut buf[..n])
    }
}

impl Seek for Failing {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn probe_reports_source_and_extent_errors() {
    let file = riff_with_length(LENGTH_AT);
    let len = file.len() as u64;
    let failing = |fail_at| Failing {
        inner: Cursor::new(file.clone()),
        fail_at,
    };
    let extent = Some(stored_length as ExtentFn);
    // The window read fails.
    let err = open_limited(failing(2), len, None, usize::MAX).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::Io(_)), "{err:?}");
    // The read of the length, past the window, fails.
    let err = open_limited(failing(17_000), len, extent, usize::MAX).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::Io(_)), "{err:?}");
    assert_eq!(err.byte_offset, Some(LENGTH_AT));

    // An error of the extent function is the error of `open`.
    fn broken(_: &mut ProbeReader<'_>) -> Result<Extent, Error> {
        Err(Error::at(ErrorKind::InvalidHeader, 12))
    }
    let err = open_limited(Cursor::new(file), len, Some(broken), usize::MAX).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::InvalidHeader, Some(12))
    );
}

#[test]
fn metadata_budget_is_shared_with_the_demuxer() {
    let file = b"HEADRIFFdataTAILtail".to_vec();
    let open = |limit: usize| {
        let mut probe = Probe::new();
        probe.register(format("riff", Tier::Standard, riff));
        probe.set_anchor_scanner(Box::new(FixedAnchors::new(
            Anchors {
                audio_start: 4,
                audio_end: 20,
                ..Anchors::default()
            },
            vec![(12, 16, revision("tail 1")), (16, 20, revision("tail 2"))],
        )));
        let limits = FormatLimits::new().with_max_metadata_bytes(limit);
        let options = DemuxOptions::new().with_limits(limits);
        probe.open(&Hint::new(), Cursor::new(file.clone()), &options)
    };
    // 4 leading and 8 trailing bytes: the demuxer gets the rest.
    for (limit, rest) in [(12, 0), (13, 1), (100, 88)] {
        let mut probed = open(limit).unwrap();
        assert_eq!(budget_seen(&mut *probed.demuxer), rest);
    }
    for limit in [0, 3, 11] {
        let err = open(limit).unwrap_err();
        assert_eq!(err.kind, ErrorKind::LimitExceeded, "{limit}");
    }
}

#[test]
fn leading_chapters_reach_a_demuxer_without_chapters() {
    let mut probe = Probe::new();
    let mut bare = format("bare", Tier::Standard, riff);
    bare.open = open_bare;
    probe.register(bare);
    probe.set_anchor_scanner(Box::new(FixedAnchors::new(
        Anchors {
            audio_start: 4,
            audio_end: 12,
            chapters: vec![chapter("tag 1"), chapter("tag 2")],
            ..Anchors::default()
        },
        Vec::new(),
    )));
    let probed = probe
        .open(
            &Hint::new(),
            Cursor::new(b"HEADRIFFdata".to_vec()),
            &DemuxOptions::new(),
        )
        .unwrap();
    let ids: Vec<_> = probed
        .demuxer
        .chapters()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, ["tag 1", "tag 2"]);
}
