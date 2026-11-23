use super::*;
use akousma_core::meta::{StandardTag, Tag, VisualKind};
use core::time::Duration;

/// A frame of an ID3v2.3 (plain size) or ID3v2.4 (synchsafe size) tag.
fn frame(major: u8, id: &[u8; 4], flags: u8, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).unwrap();
    let size = if major == 4 {
        to_synchsafe(len).unwrap()
    } else {
        len.to_be_bytes()
    };
    let mut bytes = id.to_vec();
    bytes.extend_from_slice(&size);
    bytes.extend_from_slice(&[0, flags]);
    bytes.extend_from_slice(payload);
    bytes
}

/// A tag with the given header flags and body.
fn tag(major: u8, flags: u8, body: &[u8]) -> Vec<u8> {
    let size = to_synchsafe(u32::try_from(body.len()).unwrap()).unwrap();
    let mut bytes = vec![b'I', b'D', b'3', major, 0, flags];
    bytes.extend_from_slice(&size);
    bytes.extend_from_slice(body);
    bytes
}

/// A Latin-1 text frame payload.
fn text(value: &str) -> Vec<u8> {
    let mut payload = vec![0];
    payload.extend_from_slice(value.as_bytes());
    payload
}

fn standard(tags: &[Tag]) -> Vec<StandardTag> {
    tags.iter().filter_map(|tag| tag.standard.clone()).collect()
}

fn keys(tags: &[Tag]) -> Vec<&str> {
    tags.iter().map(|tag| tag.raw.key.as_str()).collect()
}

#[test]
fn synchsafe_encode_decode_and_itunes_fallback() {
    for value in [0, 1, 0x7F, 0x80, 0x3FFF, 0x4000, 0x0FFF_FFFF] {
        let bytes = to_synchsafe(value).unwrap();
        assert!(bytes.iter().all(|&b| b < 0x80));
        assert_eq!(synchsafe(bytes), Some(value));
    }
    assert_eq!(to_synchsafe(0x0FFF_FFFF), Some([0x7F; 4]));
    assert_eq!(to_synchsafe(0x1000_0000), None);
    assert_eq!(synchsafe([0, 0, 0x80, 0]), None);
    assert_eq!(synchsafe([0, 0, 1, 0]), Some(128));

    // An ID3v2.4 frame of 200 bytes written with a plain size (00 00 00 C8):
    // the byte 0xC8 cannot be synchsafe.
    let cover = vec![0xAB; 200];
    let mut plain = b"PRIV\x00\x00\x00\xC8\x00\x00".to_vec();
    plain.extend_from_slice(&cover);
    plain.extend(frame(4, b"TIT2", 0, &text("After")));
    let parsed = parse(&tag(4, 0, &plain)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["PRIV", "TIT2"]);
    assert_eq!(parsed.revision.tags[0].raw.value, RawValue::Binary(cover));

    // 0x0100 bytes stored plain (00 00 01 00): synchsafe reads 128 bytes,
    // which ends inside the frame; the plain reading ends at the next frame.
    let mut plain = b"PRIV\x00\x00\x01\x00\x00\x00".to_vec();
    plain.extend_from_slice(&[0x41; 256]);
    plain.extend(frame(4, b"TIT2", 0, &text("After")));
    let parsed = parse(&tag(4, 0, &plain)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["PRIV", "TIT2"]);

    // Both readings plausible: synchsafe wins.
    let mut ambiguous = frame(4, b"PRIV", 0, &[0x30; 128]);
    ambiguous.extend(frame(4, b"TIT2", 0, &text("Next")));
    ambiguous.resize(10 + 256, 0);
    let parsed = parse(&tag(4, 0, &ambiguous)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["PRIV", "TIT2"]);

    // Neither reading ends at a boundary: the frame list ends.
    let mut neither = b"PRIV\x00\x00\x01\x00\x00\x00".to_vec();
    neither.extend_from_slice(&[0x41; 300]);
    let parsed = parse(&tag(4, 0, &neither)).unwrap();
    assert!(parsed.revision.tags.is_empty());
}

#[test]
fn id3v23_vs_v24_extended_header_and_frame_flags() {
    let title = frame(3, b"TIT2", 0, &text("T"));
    // ID3v2.3 extended header: plain size 6 or 10 that excludes itself.
    for (ext_size, crc) in [(6u32, &[][..]), (10, &[1, 2, 3, 4][..])] {
        let mut body = ext_size.to_be_bytes().to_vec();
        body.extend_from_slice(&[if crc.is_empty() { 0 } else { 0x80 }, 0, 0, 0, 0, 0]);
        body.extend_from_slice(crc);
        body.extend_from_slice(&title);
        let parsed = parse(&tag(3, FLAG_EXTENDED, &body)).unwrap();
        assert_eq!(keys(&parsed.revision.tags), ["TIT2"], "ext_size {ext_size}");
    }
    // ID3v2.3 with unsynchronisation: the extended header is unsynchronised
    // too (its CRC FF 00 00 00 is stored as FF 00 00 00 00).
    let mut body = vec![0, 0, 0, 10, 0x80, 0, 0, 0, 0, 0, 0xFF, 0x00, 0x00, 0, 0];
    body.extend_from_slice(&title);
    let parsed = parse(&tag(3, FLAG_EXTENDED | FLAG_UNSYNC, &body)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["TIT2"]);

    // ID3v2.4 extended header: synchsafe size that includes itself.
    let title4 = frame(4, b"TIT2", 0, &text("T"));
    for ext in [
        vec![0, 0, 0, 6, 1, 0],
        vec![0, 0, 0, 15, 1, 0x20, 1, 2, 3, 4, 5, 0, 0, 0, 0],
    ] {
        let mut body = ext.clone();
        body.extend_from_slice(&title4);
        let parsed = parse(&tag(4, FLAG_EXTENDED, &body)).unwrap();
        assert_eq!(keys(&parsed.revision.tags), ["TIT2"], "ext {ext:?}");
    }
    // Extended header larger than the tag, or below 6 in ID3v2.4.
    let err = parse(&tag(3, FLAG_EXTENDED, &[0x7F, 0xFF, 0xFF, 0xFF, 0, 0])).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidData);
    let err = parse(&tag(4, FLAG_EXTENDED, &[0, 0, 0, 5, 1, 0])).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidData);

    // ID3v2.3 frame flags: decompressed size (4), encryption (1), group (1).
    let grouped = frame(3, b"TIT2", 0x20, &[7, 0, b'G']);
    let parsed = parse(&tag(3, 0, &grouped)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [StandardTag::TrackTitle("G".into())]
    );
    let compressed = frame(3, b"TIT2", 0x80 | 0x40 | 0x20, &[0, 0, 0, 9, 1, 7, 0xAA]);
    let parsed = parse(&tag(3, 0, &compressed)).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Binary(vec![0xAA])
    );

    // ID3v2.4 frame flags: group (1), encryption (1), data length (4).
    let both = frame(4, b"TIT2", 0x40 | 0x01, &[7, 0, 0, 0, 2, 0, b'D']);
    let parsed = parse(&tag(4, 0, &both)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [StandardTag::TrackTitle("D".into())]
    );
    // Data length indicator in a frame of fewer than 4 bytes: skipped.
    let mut short = frame(4, b"TIT2", 0x01, &[0, 0, 1]);
    short.extend(frame(4, b"TALB", 0, &text("A")));
    let parsed = parse(&tag(4, 0, &short)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["TALB"]);

    // Footer bit: counted in ID3v2.4 only.
    let v3 = Id3v2Header::parse(b"ID3\x03\x00\x10\x00\x00\x00\x05").unwrap();
    assert_eq!((v3.has_footer(), v3.total_len()), (false, 15));
    let v4 = Id3v2Header::parse(b"ID3\x04\x00\x10\x00\x00\x00\x05").unwrap();
    assert_eq!((v4.has_footer(), v4.total_len()), (true, 25));
    for bad in [
        b"ID3\x05\x00\x00\x00\x00\x00\x00",
        b"ID3\x03\xFF\x00\x00\x00\x00\x00",
    ] {
        assert_eq!(
            Id3v2Header::parse(bad).unwrap_err().kind,
            ErrorKind::InvalidHeader
        );
    }
    // A body shorter than the header says.
    let mut cut = tag(3, 0, &title);
    cut.pop();
    assert_eq!(parse(&cut).unwrap_err().kind, ErrorKind::UnexpectedEof);
}

#[test]
fn unsync_swar_word_boundary() {
    let cases: [(&[u8], &[u8]); 7] = [
        (&[], &[]),
        (&[0xFF, 0x00], &[0xFF]),
        (&[0xFF; 16], &[0xFF; 16]),
        // FF at the end of the first word, 00 at the start of the second.
        (
            &[
                1, 2, 3, 4, 5, 6, 7, 0xFF, 0x00, 9, 10, 11, 12, 13, 14, 15, 16,
            ],
            &[1, 2, 3, 4, 5, 6, 7, 0xFF, 9, 10, 11, 12, 13, 14, 15, 16],
        ),
        // FF 00 00: only the first zero goes.
        (
            &[0xFF, 0x00, 0x00, 1, 2, 3, 4, 5, 6, 7],
            &[0xFF, 0x00, 1, 2, 3, 4, 5, 6, 7],
        ),
        // FF 00 FF 00 across words.
        (
            &[0, 0, 0, 0, 0, 0, 0xFF, 0, 0xFF, 0, 1, 1, 1, 1, 1, 1, 1, 1],
            &[0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 1, 1, 1, 1, 1, 1, 1, 1],
        ),
        (&[0xFE, 0x00, 0x7F, 0x00], &[0xFE, 0x00, 0x7F, 0x00]),
    ];
    for (input, expected) in cases {
        let mut bytes = input.to_vec();
        remove_unsync(&mut bytes);
        assert_eq!(bytes, expected, "{input:?}");
    }
    // Every start offset of a long buffer agrees with a byte-wise reference.
    let input: Vec<u8> = (0..300u32)
        .map(|i| match i % 7 {
            0 | 3 => 0xFF,
            1 => 0,
            _ => u8::try_from(i % 251).unwrap(),
        })
        .collect();
    for start in 0..16 {
        let mut bytes = input[start..].to_vec();
        remove_unsync(&mut bytes);
        assert_eq!(bytes, unsync_reference(&input[start..]), "start {start}");
    }

    // Unsynchronisation is removed once: ID3v2.4 tag flag and frame flag
    // together (FF 00 00 must become FF 00).
    let payload = [0, b'A', 0xFF, 0x00, 0x00];
    let parsed = parse(&tag(4, FLAG_UNSYNC, &frame(4, b"PRIV", 0x02, &payload))).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Binary(vec![0, b'A', 0xFF, 0x00])
    );
    // ID3v2.3: the whole body, frame headers included.
    let body = b"TIT2\x00\x00\x00\x03\x00\x00\x00\xFF\x00\xFF".to_vec();
    let parsed = parse(&tag(3, FLAG_UNSYNC, &body)).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Text("\u{FF}\u{FF}".into())
    );
}

/// Byte-by-byte removal of unsynchronisation.
fn unsync_reference(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut prev_ff = false;
    for &byte in input {
        if !(prev_ff && byte == 0) {
            out.push(byte);
        }
        prev_ff = byte == 0xFF && !(prev_ff && byte == 0);
    }
    out
}

#[test]
fn utf16_even_alignment_and_single_bom() {
    // 'A' U+0100 in UTF-16LE contains 00 00 at an odd offset.
    let mut payload = vec![1, 0xFF, 0xFE, 0x41, 0x00, 0x00, 0x01, 0x00, 0x00];
    let parsed = parse(&tag(3, 0, &frame(3, b"TIT2", 0, &payload))).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Text("A\u{100}".into())
    );
    // Several values, each with its own mark (ID3v2.4).
    payload.extend_from_slice(&[0xFE, 0xFF, 0x00, 0x42]);
    let parsed = parse(&tag(4, 0, &frame(4, b"TPE1", 0, &payload))).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::MultiText(vec!["A\u{100}".into(), "B".into()])
    );
    // An empty UTF-16 frame without a mark, and a missing mark.
    let mut body = frame(3, b"TIT2", 0, &[1]);
    body.extend(frame(3, b"TALB", 0, &[1, 0x43, 0x00, 0x44, 0x00]));
    let parsed = parse(&tag(3, 0, &body)).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Text(String::new())
    );
    assert_eq!(parsed.revision.tags[0].standard, None);
    assert_eq!(
        parsed.revision.tags[1].raw.value,
        RawValue::Text("CD".into())
    );

    // COMM and USLT with one mark before the description only.
    for (id, expected) in [
        (b"COMM", StandardTag::Comment("Hi".into())),
        (b"USLT", StandardTag::Lyrics("Hi".into())),
    ] {
        let payload = [
            1, b'e', b'n', b'g', 0xFE, 0xFF, 0x00, 0x00, 0x00, b'H', 0x00, b'i',
        ];
        let parsed = parse(&tag(3, 0, &frame(3, id, 0, &payload))).unwrap();
        let tag = &parsed.revision.tags[0];
        assert_eq!(tag.raw.value, RawValue::Text("Hi".into()));
        assert_eq!(tag.standard, Some(expected));
    }
}

/// A `CHAP` payload with a `TIT2` sub-frame.
fn chap(major: u8, id: &str, start_ms: u32, title: &str, extra: &[u8]) -> Vec<u8> {
    let mut payload = id.as_bytes().to_vec();
    payload.push(0);
    for field in [start_ms, start_ms + 1000, u32::MAX, 77] {
        payload.extend_from_slice(&field.to_be_bytes());
    }
    payload.extend(frame(major, b"TIT2", 0, &text(title)));
    payload.extend_from_slice(extra);
    payload
}

#[test]
fn id3v2_chap_ctoc_and_depth_limit() {
    let mut body = frame(4, b"CHAP", 0, &chap(4, "c1", 0, "One", &[]));
    body.extend(frame(4, b"CHAP", 0, &chap(4, "c2", 5000, "Two", &[])));
    // Top-level ordered table: c2 before c1.
    body.extend(frame(4, b"CTOC", 0, b"toc\x00\x03\x02c2\x00c1\x00"));
    let parsed = parse(&tag(4, 0, &body)).unwrap();
    let ids: Vec<_> = parsed.chapters.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["c2", "c1"]);
    let c1 = &parsed.chapters[1];
    assert_eq!(c1.start, Duration::ZERO);
    assert_eq!(c1.end, Some(Duration::from_secs(1)));
    assert_eq!((c1.byte_offset_start, c1.byte_offset_end), (None, Some(77)));
    assert_eq!(standard(&c1.tags), [StandardTag::TrackTitle("One".into())]);
    assert!(parsed.revision.tags.is_empty());

    // An unordered table keeps the frame order.
    let mut body = frame(3, b"CHAP", 0, &chap(3, "c1", 0, "One", &[]));
    body.extend(frame(3, b"CHAP", 0, &chap(3, "c2", 5000, "Two", &[])));
    body.extend(frame(3, b"CTOC", 0, b"toc\x00\x02\x02c2\x00c1\x00"));
    let parsed = parse(&tag(3, 0, &body)).unwrap();
    let ids: Vec<_> = parsed.chapters.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2"]);

    // CHAP inside CHAP inside CHAP, 1000 levels deep: only the outer
    // chapter is read, nested ones are dropped without recursion.
    let mut nested = chap(4, "deep", 0, "Deep", &[]);
    for level in 0..1000 {
        let inner = frame(4, b"CHAP", 0, &nested);
        nested = chap(4, &format!("l{level}"), 0, "Level", &inner);
    }
    let parsed = parse(&tag(4, 0, &frame(4, b"CHAP", 0, &nested))).unwrap();
    assert_eq!(parsed.chapters.len(), 1);
    assert_eq!(parsed.chapters[0].id, "l999");
    assert_eq!(
        standard(&parsed.chapters[0].tags),
        [StandardTag::TrackTitle("Level".into())]
    );
}

#[test]
fn replaygain_rva2_itunsmpb_and_riff_info() {
    let txxx = |description: &str, value: &str| {
        let mut payload = text(description);
        payload.push(0);
        payload.extend_from_slice(value.as_bytes());
        frame(3, b"TXXX", 0, &payload)
    };
    let mut body = txxx("replaygain_track_gain", "-6,42 dB");
    body.extend(txxx("REPLAYGAIN_ALBUM_GAIN", "+3.21dB"));
    body.extend(txxx("REPLAYGAIN_TRACK_PEAK", "0.988"));
    body.extend(txxx("REPLAYGAIN_ALBUM_PEAK", "NaN"));
    body.extend(txxx(
        "iTunSMPB",
        " 00000000 00000840 000001C0 0000000000046E00",
    ));
    // RVA2: a front-left channel, then the master volume channel with a
    // 16-bit peak; -1024 / 512 = -2 dB.
    let mut rva2 = b"album\x00".to_vec();
    rva2.extend_from_slice(&[2, 0x01, 0x00, 0]);
    rva2.extend_from_slice(&[1, 0xFC, 0x00, 16, 0x7F, 0xFF]);
    body.extend(frame(3, b"RVA2", 0, &rva2));
    let mut comm = b"\x00eng".to_vec();
    comm.extend_from_slice(b"iTunSMPB\x00 00000000 00000210 00000A00 00000000001CBC00");
    body.extend(frame(3, b"COMM", 0, &comm));
    let parsed = parse(&tag(3, 0, &body)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [
            StandardTag::ReplayGainTrackGainDb(-6.42),
            StandardTag::ReplayGainAlbumGainDb(3.21),
            StandardTag::ReplayGainTrackPeak(0.988),
            StandardTag::EncoderDelay(0x840),
            StandardTag::EncoderPadding(0x1C0),
            StandardTag::ReplayGainAlbumGainDb(-2.0),
            StandardTag::EncoderDelay(0x210),
            StandardTag::EncoderPadding(0xA00),
        ]
    );
    let tags = &parsed.revision.tags;
    assert_eq!(tags[0].raw.key, "TXXX:replaygain_track_gain");
    assert_eq!(tags[3].standard, None);
    assert_eq!(tags[4].raw, tags[5].raw);
    assert_eq!(tags[6].raw.key, "RVA2:album");

    // A truncated RVA2 or a malformed iTunSMPB keeps only the raw value.
    let mut body = frame(3, b"RVA2", 0, b"track\x00\x01\x00");
    body.extend(txxx("iTunSMPB", "garbage"));
    let parsed = parse(&tag(3, 0, &body)).unwrap();
    assert_eq!(
        parsed.revision.tags[0].raw.value,
        RawValue::Binary(b"track\x00\x01\x00".to_vec())
    );
    assert_eq!(standard(&parsed.revision.tags), []);
}

#[test]
fn text_frames_genres_and_id3v22() {
    let mut body = Vec::new();
    for (id, value) in [
        (b"TRCK", "3/12"),
        (b"TPOS", "1"),
        (b"TBPM", "128"),
        (b"TDRC", "2003-05-31"),
        (b"TDRL", "May 2003"),
        (b"TSSE", "LAME 3.100"),
        (b"TCON", "(17)"),
        (b"TCON", "(17)Hard Rock"),
        (b"TCON", "((Escaped)"),
        (b"TCON", "RX"),
        (b"TCON", "255"),
        (b"TXXX", "\u{0}"),
    ] {
        body.extend(frame(4, id, 0, &text(value)));
    }
    body.extend(frame(4, b"WOAR", 0, b"https://example.com\x00"));
    body.extend(frame(4, b"WXXX", 0, b"\x00site\x00https://example.org"));
    body.extend(frame(4, b"T+T2", 0, &text("bad id")));
    body.extend(frame(4, b"TALB", 0, &[]));
    body.extend(frame(4, b"TIT2", 0, &[9, b'x']));
    body.resize(body.len() + 20, 0);
    let parsed = parse(&tag(4, 0, &body)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [
            StandardTag::TrackNumber(3),
            StandardTag::DiscNumber(1),
            StandardTag::Bpm(128.0),
            StandardTag::RecordingDate("2003-05-31".into()),
            StandardTag::EncoderSettings("LAME 3.100".into()),
            StandardTag::Genre("Rock".into()),
            StandardTag::Genre("Hard Rock".into()),
            StandardTag::Genre("(Escaped)".into()),
            StandardTag::Genre("Remix".into()),
        ]
    );
    let tags = &parsed.revision.tags;
    assert_eq!(tags[0].raw.value, RawValue::Text("3/12".into()));
    assert_eq!(tags[12].raw.key, "WOAR");
    assert_eq!(tags[13].raw.key, "WXXX:site");
    assert_eq!(
        tags[13].raw.value,
        RawValue::Text("https://example.org".into())
    );
    // The frame with an unknown encoding byte is kept as binary.
    assert_eq!(tags[14].raw.key, "TIT2");
    assert_eq!(tags[14].raw.value, RawValue::Binary(vec![9, b'x']));
    assert_eq!(tags.len(), 15);

    // ID3v2.2: 6-byte frame headers, PIC with a 3-character format.
    let mut body = b"TT2\x00\x00\x06\x00Title".to_vec();
    let mut pic = b"\x00PNG\x03desc\x00".to_vec();
    pic.extend_from_slice(b"\x89PNG");
    body.extend_from_slice(&[b'P', b'I', b'C', 0, 0, u8::try_from(pic.len()).unwrap()]);
    body.extend_from_slice(&pic);
    body.extend_from_slice(b"XYZ\x00\x00\x01\x05");
    let parsed = parse(&tag(2, 0, &body)).unwrap();
    assert_eq!(keys(&parsed.revision.tags), ["TIT2", "XYZ"]);
    let visual = &parsed.revision.visuals[0];
    assert_eq!(visual.kind, VisualKind::FrontCover);
    assert_eq!(
        (visual.media_type.as_str(), visual.description.as_str()),
        ("image/png", "desc")
    );
    assert_eq!(visual.data, b"\x89PNG");
    // A compressed ID3v2.2 tag is skipped.
    assert_eq!(
        parse(&tag(2, FLAG_EXTENDED, &body)).unwrap(),
        Id3v2Tag::default()
    );

    // ID3v2.3 with an ID3v2.2 identifier and APIC with a link.
    let mut body = b"TT2\x00\x00\x00\x00\x03\x00\x00\x00Hi".to_vec();
    body.extend(frame(3, b"APIC", 0, b"\x00-->\x00\x08\x00http://x/y.jpg"));
    body.extend(frame(3, b"APIC", 0, b"\x00image/JPG\x00\x00\x00\xFF\xD8"));
    let parsed = parse(&tag(3, 0, &body)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [StandardTag::TrackTitle("Hi".into())]
    );
    let visuals = &parsed.revision.visuals;
    assert_eq!(
        (visuals[0].media_type.as_str(), visuals[0].kind),
        ("-->", VisualKind::Artist)
    );
    assert_eq!(visuals[0].data, b"http://x/y.jpg");
    assert_eq!(visuals[1].media_type, "image/jpeg");
}

#[test]
fn field_limit() {
    let one = frame(3, b"TIT2", 0, &text("x"));
    let body: Vec<u8> = one
        .iter()
        .copied()
        .cycle()
        .take(one.len() * 65_537)
        .collect();
    let err = parse(&tag(3, 0, &body)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
    let values = "\0".repeat(70_000);
    let err = parse(&tag(
        4,
        0,
        &frame(4, b"TPE1", 0, &text(&format!("a{values}b"))),
    ))
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
}

#[test]
fn bpm_compilation_and_chapter_limit() {
    let mut body = Vec::new();
    for (id, value) in [
        (b"TBPM", "128"),
        (b"TBPM", " 128.5 "),
        (b"TBPM", "0"),
        (b"TBPM", "-120"),
        (b"TBPM", "inf"),
        (b"TBPM", "1e39"),
        (b"TCMP", "1"),
        (b"TCMP", "0"),
        (b"TCMP", "yes"),
    ] {
        body.extend(frame(4, id, 0, &text(value)));
    }
    let parsed = parse(&tag(4, 0, &body)).unwrap();
    assert_eq!(
        standard(&parsed.revision.tags),
        [
            StandardTag::Bpm(128.0),
            StandardTag::Bpm(128.5),
            StandardTag::Compilation(true),
            StandardTag::Compilation(false),
        ]
    );
    assert_eq!(parsed.revision.tags.len(), 9);

    // 65 536 chapters are kept; one more fails like any other field.
    let short = frame(
        4,
        b"CHAP",
        0,
        b"c\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
    );
    let body: Vec<u8> = short
        .iter()
        .copied()
        .cycle()
        .take(short.len() * 65_536)
        .collect();
    let parsed = parse(&tag(4, 0, &body)).unwrap();
    assert_eq!(parsed.chapters.len(), 65_536);
    let body: Vec<u8> = short
        .iter()
        .copied()
        .cycle()
        .take(short.len() * 65_537)
        .collect();
    let err = parse(&tag(4, 0, &body)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
}

#[test]
fn chapter_sub_frames_share_the_entries() {
    use akousma_core::FormatLimits;
    use akousma_core::meta::MetadataBudget;

    // Two chapters with two titles each: 6 entries.
    let mut chap = b"c\x00".to_vec();
    chap.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    chap.extend_from_slice(&[0xFF; 8]);
    for title in ["a", "b"] {
        chap.extend(frame(4, b"TIT2", 0, &text(title)));
    }
    let body = [frame(4, b"CHAP", 0, &chap), frame(4, b"CHAP", 0, &chap)].concat();
    let tag = tag(4, 0, &body);
    let with_room = |entries: usize| {
        let mut budget = MetadataBudget::new(&FormatLimits::new());
        budget
            .take_entries(MetadataBudget::MAX_ENTRIES - entries)
            .unwrap();
        (
            parse_with_budget(&tag, &mut budget),
            budget.remaining_entries(),
        )
    };
    let (parsed, left) = with_room(6);
    let parsed = parsed.unwrap();
    assert_eq!(left, 0);
    assert_eq!(parsed.chapters.len(), 2);
    assert_eq!(parsed.chapters[1].tags.len(), 2);
    // One entry short: the last sub-frame does not fit, and the budget is
    // left as it was.
    let (err, left) = with_room(5);
    assert_eq!(err.unwrap_err().kind, ErrorKind::LimitExceeded);
    assert_eq!(left, 5);

    // Without a budget, 32 768 chapters with one title each and one more
    // field exceed the 65 536 entries of a tag.
    let mut chap = b"c\x00".to_vec();
    chap.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    chap.extend_from_slice(&[0xFF; 8]);
    chap.extend(frame(4, b"TIT2", 0, &text("t")));
    let one = frame(4, b"CHAP", 0, &chap);
    let mut body: Vec<u8> = one
        .iter()
        .copied()
        .cycle()
        .take(one.len() * 32_768)
        .collect();
    assert_eq!(parse(&tag_v4(&body)).unwrap().chapters.len(), 32_768);
    body.extend(frame(4, b"TIT2", 0, &text("x")));
    let err = parse(&tag_v4(&body)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
}

/// An ID3v2.4 tag around `body`.
fn tag_v4(body: &[u8]) -> Vec<u8> {
    tag(4, 0, body)
}
