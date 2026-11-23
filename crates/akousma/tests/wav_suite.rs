//! End-to-end decoding of the five WAVE containers through the default
//! probe and codecs, with tags around and inside the file.

use akousma::meta::StandardTag;
use akousma::{CodecId, ErrorKind, SeekMode, SeekTarget};

fn chunk(id: &[u8; 4], payload: &[u8], big_endian: bool) -> Vec<u8> {
    let mut out = id.to_vec();
    let size = payload.len() as u32;
    out.extend_from_slice(&if big_endian {
        size.to_be_bytes()
    } else {
        size.to_le_bytes()
    });
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// `fmt ` fields of 16-bit stereo PCM at 8 kHz.
fn fmt_fields(big_endian: bool) -> Vec<u8> {
    let fields: [(u32, usize); 6] = [(1, 2), (2, 2), (8000, 4), (32_000, 4), (4, 2), (16, 2)];
    let mut out = Vec::new();
    for (value, len) in fields {
        let bytes = if big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        if big_endian {
            out.extend_from_slice(&bytes[4 - len..]);
        } else {
            out.extend_from_slice(&bytes[..len]);
        }
    }
    out
}

const SAMPLES: [i16; 4] = [16_384, -16_384, i16::MIN, 8_192];
const EXPECTED: [f32; 4] = [0.5, -0.5, -1.0, 0.25];

fn data(big_endian: bool) -> Vec<u8> {
    SAMPLES
        .iter()
        .flat_map(|s| {
            if big_endian {
                s.to_be_bytes()
            } else {
                s.to_le_bytes()
            }
        })
        .collect()
}

fn riff_family(magic: &[u8; 4]) -> Vec<u8> {
    let be = magic == b"RIFX";
    let mut body = b"WAVE".to_vec();
    if magic == b"RF64" || magic == b"BW64" {
        let mut ds64 = 0u64.to_le_bytes().to_vec();
        ds64.extend_from_slice(&8u64.to_le_bytes());
        ds64.extend_from_slice(&[0; 12]);
        body.extend_from_slice(&chunk(b"ds64", &ds64, false));
    }
    body.extend_from_slice(&chunk(b"fmt ", &fmt_fields(be), be));
    body.extend_from_slice(&chunk(b"data", &data(be), be));
    let mut out = magic.to_vec();
    let size = body.len() as u32;
    out.extend_from_slice(&if be {
        size.to_be_bytes()
    } else {
        size.to_le_bytes()
    });
    out.extend_from_slice(&body);
    out
}

fn wave64() -> Vec<u8> {
    const SUFFIX: [u8; 12] = [
        0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
    ];
    let w64_chunk = |name: &[u8; 4], payload: &[u8]| {
        let mut out = name.to_vec();
        out.extend_from_slice(&SUFFIX);
        out.extend_from_slice(&(payload.len() as u64 + 24).to_le_bytes());
        out.extend_from_slice(payload);
        out.resize(out.len() + (payload.len().wrapping_neg() & 7), 0);
        out
    };
    let body = [
        w64_chunk(b"fmt ", &fmt_fields(false)),
        w64_chunk(b"data", &data(false)),
    ]
    .concat();
    let mut out = vec![
        0x72, 0x69, 0x66, 0x66, 0x2E, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00,
        0x00,
    ];
    out.extend_from_slice(&(body.len() as u64 + 40).to_le_bytes());
    out.extend_from_slice(b"wave");
    out.extend_from_slice(&SUFFIX);
    out.extend_from_slice(&body);
    out
}

#[test]
fn five_containers_end_to_end() {
    for file in [
        riff_family(b"RIFF"),
        riff_family(b"RIFX"),
        riff_family(b"RF64"),
        riff_family(b"BW64"),
        wave64(),
    ] {
        let decoded = akousma::decode_bytes(file).unwrap();
        assert_eq!(decoded.format, "wav");
        assert_eq!(
            (decoded.params.sample_rate, decoded.params.channels),
            (8000, 2)
        );
        assert_eq!(decoded.samples, EXPECTED);
    }
}

#[test]
fn tags_around_and_inside() {
    // ID3v2 in front, an `INFO` list inside, ID3v1 at the end.
    let mut id3 = b"ID3\x03\x00\x00\x00\x00\x00\x0D".to_vec();
    id3.extend_from_slice(b"TIT2\x00\x00\x00\x03\x00\x00\x00Hi");
    let mut wav = riff_family(b"RIFF");
    let info = chunk(b"LIST", b"INFOIART\x04\x00\x00\x00Band", false);
    wav.extend_from_slice(&info);
    let size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
    let mut id3v1 = b"TAG".to_vec();
    id3v1.extend_from_slice(b"Old title");
    id3v1.resize(128, 0);
    let file = [id3, wav, id3v1].concat();

    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(decoded.samples, EXPECTED);
    let standard: Vec<_> = decoded
        .metadata
        .iter()
        .flat_map(|r| &r.tags)
        .filter_map(|t| t.standard.clone())
        .collect();
    assert_eq!(standard[0], StandardTag::TrackTitle("Hi".into()));
    assert!(standard.contains(&StandardTag::TrackTitle("Old title".into())));
    assert!(standard.contains(&StandardTag::Artist("Band".into())));
}

#[test]
fn probe_demuxer_decoder_with_seek() {
    use std::io::Cursor;

    use akousma::{AudioBuf, AudioPlanes, ChannelMask, DecodeOptions, DemuxOptions, Hint, Packet};

    let mut probed = akousma::default_probe()
        .open(
            &Hint::new(),
            Cursor::new(riff_family(b"RIFF")),
            &DemuxOptions::new(),
        )
        .unwrap();
    let params = probed.demuxer.tracks()[0].params.clone();
    assert_eq!(params.codec, CodecId::PcmS16Le);
    let mut decoder = akousma::default_codecs()
        .make(&params, &DecodeOptions::new())
        .unwrap();
    let seeked = probed
        .demuxer
        .seek(
            SeekMode::Accurate,
            SeekTarget::Frame {
                track_id: 0,
                frame: 1,
            },
        )
        .unwrap();
    assert_eq!(seeked.actual_ts, 1);
    let mut packet = Packet::default();
    let mut buf = AudioBuf::new(
        AudioPlanes::I16(vec![Vec::new()]),
        1,
        ChannelMask::EMPTY,
        16,
    )
    .unwrap();
    assert!(probed.demuxer.next_packet(&mut packet).unwrap());
    decoder.decode(&packet, &mut buf).unwrap();
    assert_eq!(buf.plane::<i16>(0), Some(&[i16::MIN][..]));
    assert_eq!(buf.plane::<i16>(1), Some(&[8_192][..]));
    assert!(!probed.demuxer.next_packet(&mut packet).unwrap());
}

#[test]
fn not_audio() {
    assert_eq!(
        akousma::decode_bytes(Vec::new()).unwrap_err().kind,
        ErrorKind::UnsupportedFormat
    );
    let mut webp = riff_family(b"RIFF");
    webp[8..12].copy_from_slice(b"WEBP");
    assert_eq!(
        akousma::decode_bytes(webp).unwrap_err().kind,
        ErrorKind::UnsupportedFormat
    );
    let missing = akousma::decode_file("no/such/file.wav").unwrap_err();
    assert!(matches!(missing.kind, ErrorKind::Io(_)));
}

// Regressions of the probe path: container bounds, tags, revision order,
// empty audio and duplicate cue points.

/// Mono 16-bit PCM at 8 kHz.
fn fmt_mono() -> Vec<u8> {
    let mut fields = Vec::new();
    for (value, len) in [(1u32, 2), (1, 2), (8000, 4), (16_000, 4), (2, 2), (16, 2)] {
        fields.extend_from_slice(&value.to_le_bytes()[..len]);
    }
    chunk(b"fmt ", &fields, false)
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body = chunks.concat();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&body);
    out
}

fn info(title: &str) -> Vec<u8> {
    let mut value = title.as_bytes().to_vec();
    value.push(0);
    let mut list = b"INFO".to_vec();
    list.extend_from_slice(&chunk(b"INAM", &value, false));
    chunk(b"LIST", &list, false)
}

fn id3v1(title: &str) -> Vec<u8> {
    let mut tag = vec![0; 128];
    tag[..3].copy_from_slice(b"TAG");
    tag[3..3 + title.len()].copy_from_slice(title.as_bytes());
    tag[127] = 255;
    tag
}

/// An APEv2 tag with a footer only and one `Title` item.
fn ape(title: &str) -> Vec<u8> {
    let mut tag = (title.len() as u32).to_le_bytes().to_vec();
    tag.extend_from_slice(&0u32.to_le_bytes());
    tag.extend_from_slice(b"Title\0");
    tag.extend_from_slice(title.as_bytes());
    let size = tag.len() as u32 + 32;
    tag.extend_from_slice(b"APETAGEX");
    for field in [2000, size, 1, 0, 0, 0] {
        tag.extend_from_slice(&field.to_le_bytes());
    }
    tag
}

fn id3v2(title: &str) -> Vec<u8> {
    let mut frame = b"TIT2".to_vec();
    frame.extend_from_slice(&(title.len() as u32 + 1).to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(title.as_bytes());
    let mut tag = b"ID3\x03\x00\x00\x00\x00\x00".to_vec();
    tag.push(frame.len() as u8);
    tag.extend_from_slice(&frame);
    tag
}

/// The track titles of each revision, in the order of the log.
fn titles(metadata: &[akousma::meta::MetadataRevision]) -> Vec<String> {
    metadata
        .iter()
        .flat_map(|revision| &revision.tags)
        .filter_map(|tag| match &tag.standard {
            Some(StandardTag::TrackTitle(title)) => Some(title.clone()),
            _ => None,
        })
        .collect()
}

/// 200 bytes of audio (100 frames) that end with an APE tag lookalike of
/// 50 bytes and an ID3v1 tag lookalike.
fn lookalike_audio() -> Vec<u8> {
    let mut audio = vec![0x11; 22];
    audio.extend_from_slice(&ape("Fake"));
    audio.extend_from_slice(&id3v1("Fake"));
    assert_eq!(audio.len(), 200);
    audio
}

/// Decodes through the default probe with and without metadata.
fn decode_both(file: &[u8]) -> [(u64, Vec<String>); 2] {
    use std::io::Cursor;

    use akousma::{DemuxOptions, Hint, Packet};

    let decoded = akousma::decode_bytes(file.to_vec()).unwrap();
    let with = (decoded.frames() as u64, titles(&decoded.metadata));
    let options = DemuxOptions::new().with_read_metadata(false);
    let mut probed = akousma::default_probe()
        .open(&Hint::new(), Cursor::new(file.to_vec()), &options)
        .unwrap();
    let mut bytes = 0;
    let mut packet = Packet::default();
    while probed.demuxer.next_packet(&mut packet).unwrap() {
        bytes += packet.data.len() as u64;
    }
    let titles = titles(
        &probed
            .demuxer
            .metadata()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
    );
    [with, (bytes / 2, titles)]
}

#[test]
fn tag_lookalikes_inside_the_container_are_audio() {
    use akousma::Demuxer;

    let none: Vec<String> = Vec::new();
    // RIFF whose audio is nothing but an ID3v1 tag lookalike: the tag
    // would start exactly at the start of the audio.
    let file = riff(&[fmt_mono(), chunk(b"data", &id3v1_lookalike(), false)]);
    let direct = akousma::wav::WavDemuxer::open(
        std::io::Cursor::new(file.clone()),
        &akousma::DemuxOptions::new(),
    )
    .unwrap();
    assert_eq!(direct.tracks()[0].params.total_frames, Some(64));
    assert_eq!(decode_both(&file), [(64, none.clone()), (64, none.clone())]);

    let audio = lookalike_audio();
    // RIFF, directly and through the probe.
    let file = riff(&[fmt_mono(), chunk(b"data", &audio, false)]);
    let direct = akousma::wav::WavDemuxer::open(
        std::io::Cursor::new(file.clone()),
        &akousma::DemuxOptions::new(),
    )
    .unwrap();
    assert_eq!(direct.tracks()[0].params.total_frames, Some(100));
    assert_eq!(
        decode_both(&file),
        [(100, none.clone()), (100, none.clone())]
    );
    // With real tags after the container.
    let mut tagged = file.clone();
    tagged.extend_from_slice(&ape("Ape"));
    tagged.extend_from_slice(&id3v1("Last"));
    assert_eq!(
        decode_both(&tagged),
        [
            (100, vec!["Ape".into(), "Last".into()]),
            (100, none.clone())
        ]
    );

    // RF64 with the root size in `ds64`, read from the probe window.
    let mut ds64 = 0u64.to_le_bytes().to_vec();
    ds64.extend_from_slice(&200u64.to_le_bytes());
    ds64.extend_from_slice(&[0; 12]);
    let mut file = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    file.extend_from_slice(&chunk(b"ds64", &ds64, false));
    file.extend_from_slice(&fmt_mono());
    file.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
    file.extend_from_slice(&audio);
    let size = file.len() as u64 - 8;
    file[20..28].copy_from_slice(&size.to_le_bytes());
    file.extend_from_slice(&id3v1("Last"));
    assert_eq!(
        decode_both(&file),
        [(100, vec!["Last".into()]), (100, none.clone())]
    );

    // Wave64.
    const SUFFIX: [u8; 12] = [
        0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
    ];
    let w64_chunk = |name: &[u8; 4], payload: &[u8]| {
        let mut out = name.to_vec();
        out.extend_from_slice(&SUFFIX);
        out.extend_from_slice(&(payload.len() as u64 + 24).to_le_bytes());
        out.extend_from_slice(payload);
        out.resize(out.len() + (payload.len().wrapping_neg() & 7), 0);
        out
    };
    let mut fmt = fmt_mono()[8..].to_vec();
    fmt.extend_from_slice(&[0, 0]);
    let body = [w64_chunk(b"fmt ", &fmt), w64_chunk(b"data", &audio)].concat();
    let mut file = vec![
        0x72, 0x69, 0x66, 0x66, 0x2E, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00,
        0x00,
    ];
    file.extend_from_slice(&(body.len() as u64 + 40).to_le_bytes());
    file.extend_from_slice(b"wave");
    file.extend_from_slice(&SUFFIX);
    file.extend_from_slice(&body);
    file.extend_from_slice(&id3v1("Last"));
    assert_eq!(
        decode_both(&file),
        [(100, vec!["Last".into()]), (100, none)]
    );
}

#[test]
fn streamed_files_end_where_the_tags_start() {
    // Without a root size the container has no known end: tags at the end
    // of the file are found, and so are lookalikes at the end of the audio
    // (the documented limitation of such files).
    let streamed = |audio: &[u8], tail: &[u8]| {
        let mut file = b"RIFF\xFF\xFF\xFF\xFFWAVE".to_vec();
        file.extend_from_slice(&fmt_mono());
        file.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
        file.extend_from_slice(audio);
        file.extend_from_slice(tail);
        file
    };
    let none: Vec<String> = Vec::new();
    let file = streamed(&[0x11; 200], &id3v1("Last"));
    assert_eq!(
        decode_both(&file),
        [(100, vec!["Last".into()]), (100, none.clone())]
    );
    let file = streamed(&lookalike_audio(), &[]);
    assert_eq!(
        decode_both(&file),
        [(11, vec!["Fake".into(), "Fake".into()]), (11, none)]
    );
}

#[test]
fn revisions_follow_the_source_order() {
    // The same field with four values: all are kept, in the order of the
    // file, and none is merged into another.
    let wav = riff(&[fmt_mono(), info("Inside"), chunk(b"data", &[0, 0], false)]);
    let file = [id3v2("Lead"), wav.clone(), ape("Ape"), id3v1("Last")].concat();
    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(decoded.metadata.len(), 4);
    assert_eq!(titles(&decoded.metadata), ["Lead", "Inside", "Ape", "Last"]);
    assert_eq!(decoded.metadata[1].tags[0].raw.key, "INAM");

    // Without blocks in front, or after.
    let file = [wav.clone(), id3v1("Last")].concat();
    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(titles(&decoded.metadata), ["Inside", "Last"]);
    let file = [id3v2("Lead"), wav].concat();
    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(titles(&decoded.metadata), ["Lead", "Inside"]);

    // A list after the end of the root is not the container's.
    let mut file = riff(&[fmt_mono(), chunk(b"data", &[0, 0], false)]);
    file.extend_from_slice(&info("Outside"));
    let decoded = akousma::decode_bytes(file).unwrap();
    assert!(decoded.metadata.is_empty());
}

#[test]
fn hostile_sizes_through_the_facade() {
    // An empty `data` chunk before a list: no audio.
    let file = riff(&[fmt_mono(), chunk(b"data", &[], false), info("After")]);
    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(decoded.frames(), 0);
    assert_eq!(titles(&decoded.metadata), ["After"]);

    // `data` crossing the end of the root, but fully present: decoded.
    let mut file = riff(&[fmt_mono(), chunk(b"data", &[0, 0, 1, 0], false)]);
    let size = file.len() as u32 - 8 - 2;
    file[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(akousma::decode_bytes(file).unwrap().frames(), 2);

    // `data` declaring more than the source holds: `InvalidHeader`.
    let mut file = riff(&[fmt_mono(), chunk(b"data", &[0, 0, 1, 0], false)]);
    let at = 12 + fmt_mono().len() + 4;
    file[at..at + 4].copy_from_slice(&0x0000_1000u32.to_le_bytes());
    assert_eq!(
        akousma::decode_bytes(file).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );

    // 128 cue points with one identifier and 128 titles for it.
    let mut cue = 128u32.to_le_bytes().to_vec();
    for _ in 0..128 {
        cue.extend_from_slice(&1u32.to_le_bytes());
        cue.extend_from_slice(&0u32.to_le_bytes());
        cue.extend_from_slice(b"data");
        cue.extend_from_slice(&[0; 12]);
    }
    let mut adtl = b"adtl".to_vec();
    for _ in 0..128 {
        adtl.extend_from_slice(&chunk(b"labl", b"\x01\x00\x00\x00long title\0", false));
    }
    let file = riff(&[
        fmt_mono(),
        chunk(b"data", &[0, 0], false),
        chunk(b"cue ", &cue, false),
        chunk(b"LIST", &adtl, false),
    ]);
    let decoded = akousma::decode_bytes(file).unwrap();
    assert_eq!(decoded.chapters.len(), 1);
    assert_eq!(decoded.chapters[0].tags.len(), 1);
}

// The probe path: the size of RF64 past the probe window, one metadata
// budget for every block, the order of the chunks and the probe limit.

/// Opens `file` through the default probe and reads all of its packets:
/// the frames of mono 16-bit audio and the track titles.
fn probe_file(
    file: Vec<u8>,
    options: &akousma::DemuxOptions,
) -> Result<(u64, Vec<String>), akousma::Error> {
    use std::io::Cursor;

    use akousma::{Hint, Packet};

    let mut probed = akousma::default_probe().open(&Hint::new(), Cursor::new(file), options)?;
    let mut bytes = 0;
    let mut packet = Packet::default();
    while probed.demuxer.next_packet(&mut packet)? {
        bytes += packet.data.len() as u64;
    }
    let metadata: Vec<_> = probed.demuxer.metadata().iter().cloned().collect();
    Ok((bytes / 2, titles(&metadata)))
}

/// The kind and offset of the error of `result`.
fn failure<T: std::fmt::Debug>(result: Result<T, akousma::Error>) -> (ErrorKind, Option<u64>) {
    let err = result.unwrap_err();
    (err.kind, err.byte_offset)
}

/// An RF64 file of mono 16-bit audio with `before` (whole chunks) between
/// the stream header and `ds64`, whose RIFF size is that of the file or
/// `riff_size`.
fn rf64(before: &[Vec<u8>], audio: &[u8], riff_size: Option<u64>) -> Vec<u8> {
    let mut file = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    file.extend_from_slice(&before.concat());
    let ds64_at = file.len() + 8;
    let mut ds64 = vec![0; 28];
    ds64[8..16].copy_from_slice(&(audio.len() as u64).to_le_bytes());
    file.extend_from_slice(&chunk(b"ds64", &ds64, false));
    file.extend_from_slice(&fmt_mono());
    file.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
    file.extend_from_slice(audio);
    let size = riff_size.unwrap_or(file.len() as u64 - 8);
    file[ds64_at..ds64_at + 8].copy_from_slice(&size.to_le_bytes());
    file
}

/// 128 bytes of audio (64 frames) that look like an ID3v1 tag.
fn id3v1_lookalike() -> Vec<u8> {
    id3v1("Fake")
}

#[test]
fn rf64_size_beyond_the_probe_window() {
    use akousma::{DemuxOptions, Demuxer, FormatLimits};

    let audio = id3v1_lookalike();
    let none: Vec<String> = Vec::new();
    let junk = chunk(b"JUNK", &[0; 17_000], false);
    // `ds64` after 17 000 bytes of `JUNK`, past the 16 KiB window: still
    // found, so the lookalike stays audio, with and without metadata.
    let file = rf64(std::slice::from_ref(&junk), &audio, None);
    let direct =
        akousma::wav::WavDemuxer::open(std::io::Cursor::new(file.clone()), &DemuxOptions::new())
            .unwrap();
    assert_eq!(direct.tracks()[0].params.total_frames, Some(64));
    assert_eq!(decode_both(&file), [(64, none.clone()), (64, none.clone())]);
    // Behind several chunks of padding and an unknown one.
    let padding = [
        junk.clone(),
        chunk(b"PAD ", &[0; 3], false),
        chunk(b"FLLR", &[0; 20_000], false),
        chunk(b"abcd", &[1; 9], false),
    ];
    let file = rf64(&padding, &audio, None);
    assert_eq!(decode_both(&file), [(64, none.clone()), (64, none.clone())]);
    // With real tags after it.
    let mut tagged = rf64(std::slice::from_ref(&junk), &audio, None);
    tagged.extend_from_slice(&ape("Ape"));
    tagged.extend_from_slice(&id3v1("Last"));
    assert_eq!(
        decode_both(&tagged),
        [(64, vec!["Ape".into(), "Last".into()]), (64, none.clone())]
    );
    // A real 32-bit root size with `ds64`: the root size bounds the file.
    let mut file = rf64(std::slice::from_ref(&junk), &audio, None);
    let size = file.len() as u32 - 8;
    file[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(decode_both(&file), [(64, none.clone()), (64, none.clone())]);

    // A `ds64` RIFF size of 0 declares no bound: like a streamed RIFF file,
    // the lookalike is taken for a tag.
    let file = rf64(std::slice::from_ref(&junk), &audio, Some(0));
    assert_eq!(decode_both(&file), [(0, vec!["Fake".into()]), (0, none)]);

    // Broken sizes fail through the probe as they do directly.
    let options = DemuxOptions::new();
    let both = |file: Vec<u8>| {
        let direct = failure(akousma::wav::WavDemuxer::open(
            std::io::Cursor::new(file.clone()),
            &options,
        ));
        let probed = failure(probe_file(file, &options));
        (direct.0, probed.0)
    };
    let invalid = (ErrorKind::InvalidHeader, ErrorKind::InvalidHeader);
    // No `ds64` before `fmt `.
    let mut no_ds64 = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    no_ds64.extend_from_slice(&junk);
    no_ds64.extend_from_slice(&fmt_mono());
    no_ds64.extend_from_slice(&chunk(b"data", &audio, false));
    assert_eq!(both(no_ds64), invalid);
    // A `ds64` shorter than its fixed part.
    let mut short = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    short.extend_from_slice(&junk);
    short.extend_from_slice(&chunk(b"ds64", &[0; 20], false));
    short.extend_from_slice(&fmt_mono());
    short.extend_from_slice(&chunk(b"data", &audio, false));
    assert_eq!(both(short), invalid);
    // A RIFF size that overflows.
    assert_eq!(
        both(rf64(std::slice::from_ref(&junk), &audio, Some(u64::MAX))),
        invalid
    );
    // A `JUNK` chunk that runs past the end of the file.
    let mut endless = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    endless.extend_from_slice(&chunk(b"JUNK", &[0; 100], false));
    endless[16..20].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert_eq!(both(endless), invalid);
    // A `ds64` cut by the end of the file.
    let mut cut = b"RF64\xFF\xFF\xFF\xFFWAVE".to_vec();
    cut.extend_from_slice(&junk);
    cut.extend_from_slice(b"ds64\x1C\x00\x00\x00\x00\x00");
    assert_eq!(
        both(cut),
        (ErrorKind::UnexpectedEof, ErrorKind::UnexpectedEof)
    );

    // The lookup reads 8 bytes per chunk header past the window, within
    // the probe limit: the window (16 384 bytes), then the `ds64` header and
    // its RIFF size.
    let file = rf64(std::slice::from_ref(&junk), &audio, None);
    let probe_limit =
        |bytes| DemuxOptions::new().with_limits(FormatLimits::new().with_max_probe_bytes(bytes));
    assert_eq!(
        probe_file(file.clone(), &probe_limit(16_400)).unwrap(),
        (64, Vec::new())
    );
    // One byte less: the header fits, its RIFF size does not; not knowing
    // the size is an error, not "no size".
    assert_eq!(
        failure(probe_file(file.clone(), &probe_limit(16_399))),
        (ErrorKind::LimitExceeded, Some(17_028))
    );
    // Seeking over the chunks costs nothing: 20 000 bytes more of padding
    // need one header more.
    let file = rf64(&padding, &audio, None);
    let headers = 16_384 + 8 * 4 + 8;
    assert_eq!(
        probe_file(file.clone(), &probe_limit(headers)).unwrap(),
        (64, Vec::new())
    );
    assert_eq!(
        failure(probe_file(file, &probe_limit(headers - 1))).0,
        ErrorKind::LimitExceeded
    );
}

/// A source that fails to read from byte `from` until byte `until`, or that
/// reports the end of its data there if `ends` is set, while its length is
/// still that of the whole file. Reads that start before `from` stop there.
struct FailsFrom {
    inner: std::io::Cursor<Vec<u8>>,
    from: u64,
    until: u64,
    ends: bool,
}

impl std::io::Read for FailsFrom {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let pos = self.inner.position();
        if pos >= self.until {
            return self.inner.read(buf);
        }
        if pos >= self.from {
            if self.ends {
                return Ok(0);
            }
            return Err(std::io::Error::other("bad sector"));
        }
        let room = usize::try_from(self.from - self.inner.position()).unwrap_or(usize::MAX);
        let len = buf.len().min(room);
        self.inner.read(&mut buf[..len])
    }
}

impl std::io::Seek for FailsFrom {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn source_errors_while_probing() {
    use akousma::{DemuxOptions, Hint};

    let junk = chunk(b"JUNK", &[0; 17_000], false);
    let file = rf64(std::slice::from_ref(&junk), &id3v1_lookalike(), None);
    let open = |from| {
        let source = FailsFrom {
            inner: std::io::Cursor::new(file.clone()),
            from,
            until: u64::MAX,
            ends: false,
        };
        akousma::default_probe()
            .open(&Hint::new(), source, &DemuxOptions::new())
            .map(|probed| probed.format)
    };
    // In the window, at the `ds64` header and at its RIFF size: an error,
    // not a container without a size.
    for from in [100, 17_020, 17_030] {
        let (kind, offset) = failure(open(from));
        assert!(matches!(kind, ErrorKind::Io(_)), "{from}: {kind:?}");
        assert!(
            offset.is_some_and(|offset| offset <= from),
            "{from}: {offset:?}"
        );
    }
    assert_eq!(open(file.len() as u64).unwrap(), "wav");

    // A source that ends or fails inside the APEv2 tag at the end of a
    // streamed file: an error with and without metadata, not a damaged tag
    // left as audio.
    let mut file = b"RIFF\xFF\xFF\xFF\xFFWAVE".to_vec();
    file.extend_from_slice(&fmt_mono());
    file.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
    // Audio longer than the probe window, which therefore ends before the
    // tag.
    file.extend_from_slice(&[0x11; 20_000]);
    let items = file.len() as u64;
    let tag = ape("Ape");
    file.extend_from_slice(&tag);
    for read_metadata in [true, false] {
        let options = DemuxOptions::new().with_read_metadata(read_metadata);
        let titles = if read_metadata {
            vec!["Ape".into()]
        } else {
            Vec::new()
        };
        assert_eq!(
            probe_file(file.clone(), &options).unwrap(),
            (10_000, titles)
        );
        let open = |ends| {
            // Only the items: the footer after them can be read.
            let source = FailsFrom {
                inner: std::io::Cursor::new(file.clone()),
                from: items,
                until: items + 1,
                ends,
            };
            akousma::default_probe()
                .open(&Hint::new(), source, &options)
                .map(|probed| probed.format)
        };
        assert_eq!(failure(open(true)), (ErrorKind::UnexpectedEof, Some(items)));
        assert!(matches!(failure(open(false)).0, ErrorKind::Io(_)));
    }
    // The same tag with an item that runs 1 byte past its end: all bytes
    // are there, so it is only malformed, and its 49 bytes stay audio.
    let at = items as usize;
    file[at..at + 4].copy_from_slice(&4u32.to_le_bytes());
    assert_eq!(tag.len(), 49);
    for read_metadata in [true, false] {
        let options = DemuxOptions::new().with_read_metadata(read_metadata);
        assert_eq!(
            probe_file(file.clone(), &options).unwrap(),
            (10_024, Vec::new())
        );
    }
}

#[test]
fn one_metadata_budget_on_the_probe_path() {
    use akousma::{DemuxOptions, FormatLimits};

    // Two ID3v2 tags in front, a list inside, APEv2 and ID3v1 after.
    let wav = riff(&[fmt_mono(), info("Inside"), chunk(b"data", &[0, 0], false)]);
    let list = info("Inside").len() - 8;
    let (lead, second, tail) = (id3v2("Lead"), id3v2("Two"), ape("Ape"));
    let file = [
        lead.clone(),
        second.clone(),
        wav.clone(),
        tail.clone(),
        id3v1("Last"),
    ]
    .concat();
    let all = lead.len() + second.len() + list + tail.len() + 128;
    let with = |bytes: usize, read: bool| {
        let limits = FormatLimits::new().with_max_metadata_bytes(bytes);
        let options = DemuxOptions::new()
            .with_limits(limits)
            .with_read_metadata(read);
        probe_file(file.clone(), &options)
    };
    let titles = ["Lead", "Two", "Inside", "Ape", "Last"]
        .map(String::from)
        .to_vec();
    assert_eq!(with(all, true).unwrap(), (1, titles));
    // One byte less, whatever the block that no longer fits.
    assert_eq!(failure(with(all - 1, true)).0, ErrorKind::LimitExceeded);
    for bytes in [0, lead.len() - 1, lead.len() + second.len() + list - 1] {
        assert_eq!(
            failure(with(bytes, true)).0,
            ErrorKind::LimitExceeded,
            "{bytes}"
        );
    }
    // Without metadata the byte limit is not used: only the items of the
    // APEv2 tag are read, to check them, within the scanned bytes.
    assert_eq!(with(0, false).unwrap(), (1, Vec::new()));
    let items = tail.len() - 32;
    let scan = |bytes: usize| {
        let limits = FormatLimits::new()
            .with_max_metadata_bytes(0)
            .with_max_tag_scan_bytes(bytes);
        let options = DemuxOptions::new()
            .with_limits(limits)
            .with_read_metadata(false);
        probe_file(file.clone(), &options)
    };
    assert_eq!(scan(items).unwrap(), (1, Vec::new()));
    let tail_at = (lead.len() + second.len() + wav.len()) as u64;
    assert_eq!(
        failure(scan(items - 1)),
        (ErrorKind::LimitExceeded, Some(tail_at))
    );

    // A tag and a list of 22 bytes each with a limit of 30: one budget,
    // not one per block. Offsets of the demuxer are relative to the audio
    // range.
    let small = riff(&[
        fmt_mono(),
        info("container"),
        chunk(b"data", &[0, 0], false),
    ]);
    let file = [id3v2("L"), small.clone()].concat();
    assert_eq!(id3v2("L").len(), 22);
    assert_eq!(info("container").len() - 8, 22);
    let limit =
        |bytes| DemuxOptions::new().with_limits(FormatLimits::new().with_max_metadata_bytes(bytes));
    assert_eq!(
        failure(probe_file(file.clone(), &limit(30))),
        (ErrorKind::LimitExceeded, Some(44))
    );
    assert_eq!(
        failure(probe_file(file.clone(), &limit(43))),
        (ErrorKind::LimitExceeded, Some(44))
    );
    assert_eq!(
        probe_file(file.clone(), &limit(44)).unwrap(),
        (1, vec!["L".into(), "container".into()])
    );
    // Directly, the leading tag is read by the demuxer, from the same budget.
    let direct = |bytes| {
        akousma::wav::WavDemuxer::open(std::io::Cursor::new(file.clone()), &limit(bytes))
            .map(|_| ())
    };
    assert_eq!(failure(direct(43)), (ErrorKind::LimitExceeded, Some(66)));
    assert!(direct(44).is_ok());
    assert_eq!(failure(direct(21)), (ErrorKind::LimitExceeded, Some(0)));
    // A list over the limit is an error, not skipped.
    assert_eq!(
        failure(probe_file(small.clone(), &limit(2))),
        (ErrorKind::LimitExceeded, Some(44))
    );
    assert_eq!(
        probe_file(small, &limit(22)).unwrap(),
        (1, vec!["container".into()])
    );
}

#[test]
fn same_audio_without_metadata_within_the_same_limits() {
    use akousma::{DemuxOptions, FormatLimits};

    // A streamed file, whose audio ends where the tags start: an ID3v2 tag
    // in front, then each of a valid and a rejected APEv2 tag, a Lyrics3
    // tag and an ID3v1 tag after the audio.
    let streamed = |tail: &[u8]| {
        let mut file = id3v2("Lead");
        file.extend_from_slice(b"RIFF\xFF\xFF\xFF\xFFWAVE");
        file.extend_from_slice(&fmt_mono());
        file.extend_from_slice(b"data\xFF\xFF\xFF\xFF");
        file.extend_from_slice(&[0x11; 200]);
        file.extend_from_slice(tail);
        file
    };
    let mut rejected = ape("Ape");
    rejected[8] = b'\x7F'; // a control character in the key
    let mut lyrics = b"LYRICSBEGINLYR00005Hello".to_vec();
    lyrics.extend_from_slice(b"000024LYRICS200");
    let cases = [
        (
            "valid",
            [ape("Ape"), lyrics.clone(), id3v1("Last")].concat(),
            100,
        ),
        (
            "rejected",
            [rejected.clone(), id3v1("Last")].concat(),
            100 + rejected.len() as u64 / 2,
        ),
    ];
    for (label, tail, frames) in cases {
        let file = streamed(&tail);
        let mut opened = 0;
        for limit in 0..=file.len() {
            let options = |read: bool| {
                let limits = FormatLimits::new()
                    .with_max_metadata_bytes(limit)
                    .with_max_tag_scan_bytes(limit);
                DemuxOptions::new()
                    .with_limits(limits)
                    .with_read_metadata(read)
            };
            let Ok((with, _)) = probe_file(file.clone(), &options(true)) else {
                continue;
            };
            opened += 1;
            assert_eq!(with, frames, "{label}: {limit}");
            assert_eq!(
                probe_file(file.clone(), &options(false)).unwrap(),
                (frames, Vec::new()),
                "{label}: {limit}"
            );
        }
        assert!(opened > 0, "{label}");
    }
}

#[test]
fn chunk_order_through_the_probe() {
    use akousma::DemuxOptions;

    let id3_chunk = |title: &str| chunk(b"id3 ", &id3v2(title), false);
    // `fmt `, `id3 `, `LIST`, `data`: the order of the chunks.
    let file = riff(&[
        fmt_mono(),
        id3_chunk("Tag"),
        info("List"),
        chunk(b"data", &[0, 0], false),
    ]);
    let decoded = akousma::decode_bytes(file).unwrap();
    let keys: Vec<_> = decoded
        .metadata
        .iter()
        .flat_map(|r| &r.tags)
        .map(|t| t.raw.key.as_str())
        .collect();
    assert_eq!(keys, ["TIT2", "INAM"]);
    assert_eq!(titles(&decoded.metadata), ["Tag", "List"]);

    // INFO, ID3, INFO, ID3 with conflicting titles, between external tags.
    let wav = riff(&[
        fmt_mono(),
        info("a"),
        id3_chunk("b"),
        chunk(b"data", &[0, 0], false),
        info("c"),
        id3_chunk("d"),
    ]);
    let file = [id3v2("Lead"), wav, ape("Ape"), id3v1("Last")].concat();
    let decoded = akousma::decode_bytes(file.clone()).unwrap();
    assert_eq!(decoded.metadata.len(), 7);
    assert_eq!(
        titles(&decoded.metadata),
        ["Lead", "a", "b", "c", "d", "Ape", "Last"]
    );
    let quiet = DemuxOptions::new().with_read_metadata(false);
    assert_eq!(probe_file(file, &quiet).unwrap(), (1, Vec::new()));
}

#[test]
fn probe_limit_bounds_the_signature_read() {
    use akousma::{DemuxOptions, FormatLimits};

    let limit =
        |bytes| DemuxOptions::new().with_limits(FormatLimits::new().with_max_probe_bytes(bytes));
    let file = riff(&[fmt_mono(), chunk(b"data", &[0, 0], false)]);
    // The signature of RIFF needs its first 12 bytes.
    assert_eq!(
        probe_file(file.clone(), &limit(12)).unwrap(),
        (1, Vec::new())
    );
    for bytes in [0, 1, 11] {
        assert_eq!(
            failure(probe_file(file.clone(), &limit(bytes))).0,
            ErrorKind::LimitExceeded,
            "{bytes}"
        );
    }
    assert_eq!(
        probe_file(file.clone(), &limit(usize::MAX)).unwrap(),
        (1, Vec::new())
    );
    // Wave64 needs 40.
    let w64 = wave64();
    assert!(probe_file(w64.clone(), &limit(40)).is_ok());
    assert_eq!(
        failure(probe_file(w64, &limit(39))).0,
        ErrorKind::LimitExceeded
    );
    // A leading tag is not part of the probe limit, only of the metadata
    // budget.
    let tagged = [id3v2("Lead"), file].concat();
    assert_eq!(
        probe_file(tagged, &limit(12)).unwrap(),
        (1, vec!["Lead".into()])
    );
    // Not audio: the whole window was read, so the format is unknown.
    assert_eq!(
        failure(probe_file(vec![0; 100], &limit(100))).0,
        ErrorKind::UnsupportedFormat
    );
    assert_eq!(
        failure(probe_file(vec![0; 100], &limit(99))).0,
        ErrorKind::LimitExceeded
    );
}
