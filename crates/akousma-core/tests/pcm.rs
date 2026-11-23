//! `PcmDecoder`: every sample format, G.711 against the reconstruction
//! values of ITU-T G.711, layouts and errors.

use akousma_core::audio::AudioBuf;
use akousma_core::channel::ChannelMask;
use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
use akousma_core::pcm::{ALAW_TO_I16, MULAW_TO_I16, PCM_CODECS, PcmDecoder, sample_bytes};
use akousma_core::{DecodeOptions, ErrorKind};

fn params(codec: CodecId, channels: u8, bits: u8) -> CodecParams {
    CodecParams {
        codec,
        sample_rate: 44_100,
        channels,
        channel_mask: ChannelMask::EMPTY,
        bits_per_sample: bits,
        block_align: 0,
        delay: 0,
        padding: 0,
        total_frames: None,
        extra_data: Vec::new(),
    }
}

fn empty_buf() -> AudioBuf {
    AudioBuf::f32(1, 1, ChannelMask::EMPTY).unwrap()
}

fn decode(codec: CodecId, channels: u8, bits: u8, data: &[u8]) -> AudioBuf {
    let mut decoder =
        PcmDecoder::new(&params(codec, channels, bits), &DecodeOptions::new()).unwrap();
    let mut buf = empty_buf();
    let packet = Packet {
        data: data.to_vec(),
        ..Packet::default()
    };
    decoder.decode(&packet, &mut buf).unwrap();
    buf
}

fn plane<T: akousma_core::audio::Sample>(buf: &AudioBuf, channel: usize) -> Vec<T> {
    buf.plane::<T>(channel).unwrap().to_vec()
}

#[test]
fn pcm_integer_depths_msb_aligned() {
    // Unsigned 8-bit: 0 -> -32768, 128 -> 0, 255 -> +32512.
    let buf = decode(CodecId::PcmU8, 1, 8, &[0, 128, 255, 1]);
    assert_eq!(plane::<i16>(&buf, 0), [-32_768, 0, 32_512, -32_512]);

    let buf = decode(
        CodecId::PcmS16Le,
        1,
        16,
        &[0x00, 0x80, 0xFF, 0x7F, 0x34, 0x12],
    );
    assert_eq!(plane::<i16>(&buf, 0), [i16::MIN, i16::MAX, 0x1234]);
    let buf = decode(CodecId::PcmS16Be, 1, 12, &[0x80, 0x00, 0x12, 0x30]);
    assert_eq!(plane::<i16>(&buf, 0), [i16::MIN, 0x1230]);
    assert_eq!(buf.bits_per_sample(), 12);

    // 24-bit: i24::MIN keeps its sign in the top 24 bits.
    let buf = decode(
        CodecId::PcmS24Le,
        1,
        24,
        &[0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF],
    );
    assert_eq!(plane::<i32>(&buf, 0), [i32::MIN, 0x7FFF_FF00, -256]);
    assert_eq!(-8_388_608_i32 << 8, i32::MIN);
    let buf = decode(
        CodecId::PcmS24Be,
        1,
        20,
        &[0x80, 0x00, 0x00, 0x12, 0x34, 0x50],
    );
    assert_eq!(plane::<i32>(&buf, 0), [i32::MIN, 0x1234_5000]);

    let buf = decode(
        CodecId::PcmS32Le,
        1,
        32,
        &[0, 0, 0, 0x80, 0x78, 0x56, 0x34, 0x12],
    );
    assert_eq!(plane::<i32>(&buf, 0), [i32::MIN, 0x1234_5678]);
    let buf = decode(CodecId::PcmS32Be, 1, 20, &[0x12, 0x34, 0x50, 0x00]);
    assert_eq!(plane::<i32>(&buf, 0), [0x1234_5000]);

    // 64-bit: the top 32 bits.
    let mut data = 0x1234_5678_9ABC_DEF0_i64.to_le_bytes().to_vec();
    data.extend_from_slice(&i64::MIN.to_le_bytes());
    let buf = decode(CodecId::PcmS64Le, 1, 32, &data);
    assert_eq!(plane::<i32>(&buf, 0), [0x1234_5678, i32::MIN]);
    let buf = decode(CodecId::PcmS64Be, 1, 32, &(-1_i64).to_be_bytes());
    assert_eq!(plane::<i32>(&buf, 0), [-1]);
}

#[test]
fn ieee_float_sanitized_not_clipped() {
    let values = [
        1.5_f32,
        -2.25,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.5,
    ];
    let le: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let be: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
    let expected = [1.5, -2.25, 0.0, 1.0, -1.0, -0.5];
    assert_eq!(
        plane::<f32>(&decode(CodecId::PcmF32Le, 1, 32, &le), 0),
        expected
    );
    assert_eq!(
        plane::<f32>(&decode(CodecId::PcmF32Be, 1, 32, &be), 0),
        expected
    );

    let values = [0.25_f64, f64::NAN, 1e300, -1e300, -3.0];
    let le: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let be: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
    let expected = [0.25, 0.0, 1.0, -1.0, -3.0];
    assert_eq!(
        plane::<f32>(&decode(CodecId::PcmF64Le, 1, 64, &le), 0),
        expected
    );
    assert_eq!(
        plane::<f32>(&decode(CodecId::PcmF64Be, 1, 64, &be), 0),
        expected
    );
}

/// A-law reconstruction value of ITU-T G.711 Table 1a in 13-bit units
/// (segment 0: `2m + 1`, segment `s`: `(2m + 33) << (s - 1)`), scaled to 16
/// bits.
fn alaw_reference(code: u8) -> i16 {
    let a = code ^ 0x55;
    let segment = u32::from((a >> 4) & 7);
    let m = i32::from(a & 0x0F);
    let level = if segment == 0 {
        2 * m + 1
    } else {
        (2 * m + 33) << (segment - 1)
    };
    let value = level * 8;
    i16::try_from(if a & 0x80 != 0 { value } else { -value }).unwrap()
}

/// Mu-law reconstruction value of ITU-T G.711 Table 2a in 14-bit units
/// (`((2m + 33) << e) - 33`), scaled to 16 bits.
fn mulaw_reference(code: u8) -> i16 {
    let u = !code;
    let e = u32::from((u >> 4) & 7);
    let m = i32::from(u & 0x0F);
    let value = (((2 * m + 33) << e) - 33) * 4;
    i16::try_from(if u & 0x80 != 0 { -value } else { value }).unwrap()
}

#[test]
fn g711_alaw_and_mulaw_bit_exact() {
    for code in 0..=255u8 {
        assert_eq!(
            ALAW_TO_I16[usize::from(code)],
            alaw_reference(code),
            "A-law {code:#04x}"
        );
        assert_eq!(
            MULAW_TO_I16[usize::from(code)],
            mulaw_reference(code),
            "mu-law {code:#04x}"
        );
    }
    assert_eq!(
        (ALAW_TO_I16[0x00], ALAW_TO_I16[0x55], ALAW_TO_I16[0xD5]),
        (-5504, -8, 8)
    );
    assert_eq!(
        (MULAW_TO_I16[0x00], MULAW_TO_I16[0x7F], MULAW_TO_I16[0xFF]),
        (-32_124, 0, 0)
    );
    let all: Vec<u8> = (0..=255).collect();
    let buf = decode(CodecId::G711Alaw, 1, 16, &all);
    assert_eq!(plane::<i16>(&buf, 0), ALAW_TO_I16);
    let buf = decode(CodecId::G711Mulaw, 2, 16, &all);
    let even: Vec<i16> = MULAW_TO_I16.iter().copied().step_by(2).collect();
    assert_eq!(plane::<i16>(&buf, 0), even);
}

#[test]
fn layouts_one_to_eight_channels() {
    for channels in [1u8, 2, 3, 6, 8] {
        let frames = 11usize;
        let samples: Vec<i16> = (0..frames * usize::from(channels))
            .map(|i| i16::try_from(i).unwrap() * 37 - 900)
            .collect();
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let buf = decode(CodecId::PcmS16Le, channels, 16, &data);
        assert_eq!(buf.frames(), frames);
        assert_eq!(buf.channels(), channels);
        let mut interleaved = vec![0i16; samples.len()];
        buf.copy_interleaved_i16(&mut interleaved);
        assert_eq!(interleaved, samples, "{channels} channels");
    }
}

#[test]
fn buffer_reuse_trim_and_errors() {
    let mut decoder =
        PcmDecoder::new(&params(CodecId::PcmS16Le, 2, 16), &DecodeOptions::new()).unwrap();
    let mut buf = empty_buf();
    // 0 and 1 frames.
    let mut packet = Packet::default();
    decoder.decode(&packet, &mut buf).unwrap();
    assert_eq!((buf.frames(), buf.channels()), (0, 2));
    packet.data = vec![1, 0, 2, 0];
    decoder.decode(&packet, &mut buf).unwrap();
    assert_eq!(
        (plane::<i16>(&buf, 0), plane::<i16>(&buf, 1)),
        (vec![1], vec![2])
    );
    // Trims larger than the packet leave an empty window.
    packet.data = vec![0; 12];
    packet.trim_start = 2;
    packet.trim_end = 5;
    decoder.decode(&packet, &mut buf).unwrap();
    assert!(buf.is_empty());
    packet.trim_start = 1;
    packet.trim_end = 1;
    decoder.decode(&packet, &mut buf).unwrap();
    assert_eq!(buf.frames(), 1);
    let mut untrimmed = PcmDecoder::new(
        &params(CodecId::PcmS16Le, 2, 16),
        &DecodeOptions::new().with_apply_trim(false),
    )
    .unwrap();
    untrimmed.decode(&packet, &mut buf).unwrap();
    assert_eq!(buf.frames(), 3);
    // Half a frame.
    packet.data = vec![0; 6];
    let err = decoder.decode(&packet, &mut buf).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CorruptFrame);

    for bad in [
        params(CodecId::Flac, 2, 16),
        params(CodecId::PcmS16Le, 0, 16),
        params(CodecId::PcmS16Le, 65, 16),
        params(CodecId::PcmS16Le, 2, 0),
        params(CodecId::PcmU8, 1, 9),
        params(CodecId::PcmS16Le, 1, 17),
        params(CodecId::PcmS24Le, 1, 33),
        CodecParams {
            sample_rate: 0,
            ..params(CodecId::PcmS16Le, 1, 16)
        },
    ] {
        assert!(
            PcmDecoder::new(&bad, &DecodeOptions::new()).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(
        PcmDecoder::new(&params(CodecId::Mp3, 2, 16), &DecodeOptions::new())
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedCodec
    );
    assert!(
        PCM_CODECS
            .iter()
            .all(|&codec| sample_bytes(codec).is_some())
    );
    assert_eq!(decoder.params().codec, CodecId::PcmS16Le);
}
