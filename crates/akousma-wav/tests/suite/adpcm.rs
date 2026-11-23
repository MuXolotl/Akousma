//! IMA and Microsoft ADPCM against straightforward models of their block
//! layouts, hostile headers and `fact` trimming.

use akousma_core::codec::{CodecParams, Decoder};
use akousma_core::probe::Demuxer;
use akousma_core::{ChannelMask, CodecId, DecodeOptions, DemuxOptions, ErrorKind};
use akousma_wav::AdpcmMsDecoder;

use crate::common::{chunk, decode, drain, fmt, fmt_extensible, open, open_with, riff, subtype};

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const MS_ADAPT: [i64; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
const MS_COEFFS: [[i64; 2]; 7] = [
    [256, 0],
    [512, -256],
    [0, 0],
    [192, 64],
    [240, 0],
    [460, -208],
    [392, -232],
];

/// Deterministic bytes.
pub fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Frame `n` of channel `c` addressed nibble by nibble.
pub fn ima_block(block: &[u8], ch: usize, frames: usize) -> Vec<Vec<i16>> {
    (0..ch)
        .map(|c| {
            let mut pred = i32::from(i16::from_le_bytes([block[4 * c], block[4 * c + 1]]));
            let mut index = i32::from(block[4 * c + 2]);
            let mut out = vec![pred as i16];
            for n in 0..frames - 1 {
                let byte = block[4 * ch + (n / 8 * ch + c) * 4 + n % 8 / 2];
                let nib = if n % 2 == 0 { byte & 0xF } else { byte >> 4 };
                let step = IMA_STEPS[index as usize];
                let mut diff = step >> 3;
                if nib & 1 != 0 {
                    diff += step >> 2;
                }
                if nib & 2 != 0 {
                    diff += step >> 1;
                }
                if nib & 4 != 0 {
                    diff += step;
                }
                if nib & 8 != 0 {
                    diff = -diff;
                }
                pred = (pred + diff).clamp(-32768, 32767);
                index = (index + IMA_INDEX[usize::from(nib)]).clamp(0, 88);
                out.push(pred as i16);
            }
            out
        })
        .collect()
}

fn ms_block(block: &[u8], ch: usize, frames: usize, coeffs: &[[i64; 2]]) -> Vec<Vec<i16>> {
    let field = |at: usize| i64::from(i16::from_le_bytes([block[at], block[at + 1]]));
    let mut state: Vec<_> = (0..ch)
        .map(|c| {
            (
                coeffs[usize::from(block[c])],
                field(ch + 2 * c),
                field(3 * ch + 2 * c),
                field(5 * ch + 2 * c),
            )
        })
        .collect();
    let mut out: Vec<Vec<i16>> = state.iter().map(|s| vec![s.3 as i16, s.2 as i16]).collect();
    for k in 0..(frames - 2) * ch {
        let byte = block[7 * ch + k / 2];
        let nib = if k % 2 == 0 { byte >> 4 } else { byte & 0xF };
        let signed = i64::from(if nib >= 8 { nib as i8 - 16 } else { nib as i8 });
        let (coeff, delta, s1, s2) = &mut state[k % ch];
        // The prediction is the sum divided by 256, truncated toward zero.
        let pred = (*s1 * coeff[0] + *s2 * coeff[1]) / 256;
        let sample = (pred + signed * *delta).clamp(-32768, 32767);
        *s2 = *s1;
        *s1 = sample;
        *delta = ((MS_ADAPT[usize::from(nib)] * *delta) >> 8).clamp(16, 0x7FFF_FFFF);
        out[k % ch].push(sample as i16);
    }
    for plane in &mut out {
        plane.truncate(frames);
    }
    out
}

fn ima_file(ch: u16, align: u16, spb: u16, data: &[u8], fact: Option<u32>) -> Vec<u8> {
    let mut f = fmt(0x11, ch, 8000, align, 4);
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&spb.to_le_bytes());
    let mut chunks = vec![chunk(b"fmt ", &f)];
    if let Some(fact) = fact {
        chunks.push(chunk(b"fact", &fact.to_le_bytes()));
    }
    chunks.push(chunk(b"data", data));
    riff(&chunks)
}

/// `blocks` random IMA blocks with valid step indices.
pub fn ima_data(ch: usize, align: usize, blocks: usize, seed: u32) -> Vec<u8> {
    let mut data = noise(seed, align * blocks);
    for block in data.chunks_mut(align) {
        for c in 0..ch {
            block[4 * c + 2] %= 89;
        }
    }
    data
}

fn append(planes: &mut [Vec<i16>], more: Vec<Vec<i16>>) {
    for (plane, more) in planes.iter_mut().zip(more) {
        plane.extend(more);
    }
}

#[test]
fn ima_adpcm_mono_stereo_multichannel_and_truncated() {
    // (channels, block_align, samples per block, bytes of a short last block, its frames)
    for (ch, align, spb, tail, tail_frames) in
        [(1, 36, 65, 10, 13), (2, 72, 65, 12, 1), (3, 36, 17, 30, 9)]
    {
        let mut data = ima_data(ch, align, 4, ch as u32);
        data.truncate(3 * align + tail);
        let mut expected = vec![Vec::new(); ch];
        for block in data.chunks(align) {
            let frames = if block.len() == align {
                spb
            } else {
                tail_frames
            };
            append(&mut expected, ima_block(block, ch, frames));
        }
        let file = ima_file(ch as u16, align as u16, spb as u16, &data, None);
        let params = open(file.clone()).unwrap().tracks()[0].params.clone();
        assert_eq!(params.total_frames, Some((3 * spb + tail_frames) as u64));
        assert_eq!(decode::<i16>(file).unwrap(), expected, "{ch} channels");
    }

    // The 3-channel layout holds 17 frames: a larger claim is invalid.
    let data = ima_data(3, 36, 1, 7);
    let err = open(ima_file(3, 36, 18, &data, None)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidHeader);

    // Step index above 88.
    for index in [89, 255] {
        let mut data = ima_data(1, 36, 2, 9);
        data[36 + 2] = index;
        let mut demuxer = open(ima_file(1, 36, 65, &data, None)).unwrap();
        assert_eq!(
            drain::<i16>(&mut demuxer).unwrap_err().kind,
            ErrorKind::InvalidData
        );
    }

    // `fact` trims the padding of the last block with gapless playback.
    let data = ima_data(1, 36, 3, 11);
    let full = decode::<i16>(ima_file(1, 36, 65, &data, None)).unwrap();
    let gapless = DemuxOptions::new().with_gapless(true);
    let mut demuxer = open_with(ima_file(1, 36, 65, &data, Some(150)), &gapless).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(150));
    assert_eq!(demuxer.tracks()[0].params.padding, 45);
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0], full[0][..150]);
    // Without gapless playback, every frame.
    let gapless_off = DemuxOptions::new().with_gapless(false);
    let mut demuxer = open_with(ima_file(1, 36, 65, &data, Some(150)), &gapless_off).unwrap();
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0].len(), 195);
    // A count above the blocks is clamped; 0 is ignored.
    for fact in [100_000, 0] {
        let mut demuxer = open_with(ima_file(1, 36, 65, &data, Some(fact)), &gapless).unwrap();
        assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0].len(), 195);
    }
}

#[test]
fn ima_adpcm_every_step_index_and_code() {
    // One mono block per step index and code: the predictor starts at the
    // far end of the range, so `predictor + diff` is never clamped and the
    // second frame minus the predictor is the whole difference.
    let mut data = Vec::new();
    for index in 0..89u8 {
        for code in 0..16u8 {
            let predictor = if code < 8 { i16::MIN } else { i16::MAX };
            data.extend_from_slice(&predictor.to_le_bytes());
            data.extend_from_slice(&[index, 0, code, 0, 0, 0]);
        }
    }
    let mut expected = vec![Vec::new()];
    for block in data.chunks(8) {
        append(&mut expected, ima_block(block, 1, 9));
    }
    let decoded = decode::<i16>(ima_file(1, 8, 9, &data, None)).unwrap();
    assert_eq!(decoded, expected);
    let diffs: Vec<i32> = decoded[0]
        .chunks(9)
        .map(|frames| i32::from(frames[1]) - i32::from(frames[0]))
        .collect();
    assert_eq!(diffs.len(), 89 * 16);
    // Index 0 (step 7), codes 0 and 7: 7 >> 3 = 0, and 0 + 1 + 3 + 7 = 11.
    assert_eq!((diffs[0], diffs[7], diffs[15]), (0, 11, -11));
    // Index 82 (step 18500), code 7: 2312 + 4625 + 9250 + 18500 = 34687,
    // the first difference that does not fit in an `i16`.
    assert_eq!(diffs[82 * 16 + 7], 34_687);
    // Index 88 (step 32767), code 7: 4095 + 8191 + 16383 + 32767 = 61436.
    assert_eq!(diffs.iter().map(|d| d.abs()).max(), Some(61_436));
    let wide = diffs.iter().filter(|&&d| i16::try_from(d).is_err());
    assert_eq!(wide.count(), 38);
}

/// FNV-1a over the little-endian bytes of every plane in turn.
fn fnv1a(planes: &[Vec<i16>]) -> u64 {
    let bytes = planes.iter().flatten().flat_map(|s| s.to_le_bytes());
    bytes.fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

#[test]
fn ima_adpcm_output_is_frozen() {
    // 300 random stereo blocks of 2048 bytes: random nibbles drive the step
    // index to 88, where the differences exceed the `i16` range. The hash is
    // that of the arithmetic model, so a change to both the decoder and the
    // model still fails.
    let data = ima_data(2, 2048, 300, 2041);
    let mut expected = vec![Vec::new(); 2];
    for block in data.chunks(2048) {
        append(&mut expected, ima_block(block, 2, 2041));
    }
    let decoded = decode::<i16>(ima_file(2, 2048, 2041, &data, None)).unwrap();
    assert_eq!(decoded, expected);
    assert_eq!(decoded[0].len(), 300 * 2041);
    // A step inside a block larger than `i16::MAX` needs such a difference.
    let wide = decoded
        .iter()
        .flat_map(|plane| plane.chunks(2041))
        .any(|block| {
            block
                .windows(2)
                .any(|w| (i32::from(w[1]) - i32::from(w[0])).abs() > 32_767)
        });
    assert!(wide);
    assert_eq!(fnv1a(&decoded), 0xEA94_0BFF_FB50_E1B2);
}

fn ms_params(ch: u8, align: u16, extra: Vec<u8>) -> CodecParams {
    CodecParams {
        codec: CodecId::AdpcmMs,
        sample_rate: 8000,
        channels: ch,
        channel_mask: ChannelMask::EMPTY,
        bits_per_sample: 16,
        block_align: align,
        delay: 0,
        padding: 0,
        total_frames: None,
        extra_data: extra,
    }
}

fn ms_extension(spb: u16, coeffs: &[[i16; 2]]) -> Vec<u8> {
    let mut ext = spb.to_le_bytes().to_vec();
    ext.extend_from_slice(&(coeffs.len() as u16).to_le_bytes());
    for c in coeffs.as_flattened() {
        ext.extend_from_slice(&c.to_le_bytes());
    }
    ext
}

/// The seven standard coefficient pairs.
fn standard_pairs() -> Vec<[i16; 2]> {
    MS_COEFFS
        .iter()
        .map(|c| [c[0] as i16, c[1] as i16])
        .collect()
}

fn ms_file(ch: u16, align: u16, ext: &[u8], data: &[u8]) -> Vec<u8> {
    let mut f = fmt(2, ch, 8000, align, 4);
    f.extend_from_slice(&(ext.len() as u16).to_le_bytes());
    f.extend_from_slice(ext);
    riff(&[chunk(b"fmt ", &f), chunk(b"data", data)])
}

/// `blocks` random Microsoft ADPCM blocks with predictor indices below `count`.
fn ms_data(ch: usize, align: usize, blocks: usize, count: u8, seed: u32) -> Vec<u8> {
    let mut data = noise(seed, align * blocks);
    for block in data.chunks_mut(align) {
        for predictor in &mut block[..ch] {
            *predictor %= count;
        }
    }
    data
}

#[test]
fn ms_adpcm_custom_coeffs_odd_spb_and_i64_safety() {
    let standard = standard_pairs();
    // Mono with an odd count (27 of 28 frames: the last nibble is unused),
    // stereo with the standard table written out.
    for (ch, align, spb) in [(1usize, 20usize, 27usize), (2, 32, 11)] {
        let data = ms_data(ch, align, 3, 7, 3 + ch as u32);
        let mut expected = vec![Vec::new(); ch];
        for block in data.chunks(align) {
            append(&mut expected, ms_block(block, ch, spb, &MS_COEFFS));
        }
        let file = ms_file(
            ch as u16,
            align as u16,
            &ms_extension(spb as u16, &standard),
            &data,
        );
        assert_eq!(decode::<i16>(file).unwrap(), expected, "{ch} channels");
    }

    // The declared samples per block are kept below the largest count: 20
    // of 28 frames per block.
    let data = ms_data(1, 20, 2, 7, 21);
    let mut expected = vec![Vec::new()];
    for block in data.chunks(20) {
        append(&mut expected, ms_block(block, 1, 20, &MS_COEFFS));
    }
    let file = ms_file(1, 20, &ms_extension(20, &standard), &data);
    assert_eq!(decode::<i16>(file).unwrap(), expected);

    // Extreme custom coefficients and samples, with the delta growing to its
    // ceiling through 30 nibbles 0x7 and 0x8.
    let extreme = [[i16::MIN, i16::MIN]];
    let mut block = vec![0, 0xFF, 0x7F, 0x00, 0x80, 0x00, 0x80];
    block.extend_from_slice(&[0x78; 15]);
    let file = ms_file(1, 22, &ms_extension(32, &extreme), &block);
    let expected = ms_block(&block, 1, 32, &[[-32768, -32768]]);
    assert_eq!(decode::<i16>(file).unwrap(), expected);
    let mut block = vec![0, 0xFF, 0x7F, 0x00, 0x00, 0x00, 0x00];
    block.extend_from_slice(&[0x88; 15]);
    let file = ms_file(1, 22, &ms_extension(32, &standard), &block);
    assert_eq!(
        decode::<i16>(file).unwrap(),
        ms_block(&block, 1, 32, &MS_COEFFS)
    );

    // A predictor index outside the table.
    let mut data = ms_data(1, 20, 1, 7, 5);
    data[0] = 7;
    let mut demuxer = open(ms_file(1, 20, &ms_extension(28, &standard), &data)).unwrap();
    assert_eq!(
        drain::<i16>(&mut demuxer).unwrap_err().kind,
        ErrorKind::InvalidData
    );
    data[0] = 1;
    let mut demuxer = open(ms_file(1, 20, &ms_extension(28, &[[256, 0]]), &data)).unwrap();
    assert_eq!(
        drain::<i16>(&mut demuxer).unwrap_err().kind,
        ErrorKind::InvalidData
    );

    // More than two channels.
    let file = ms_file(3, 63, &[], &[0; 63]);
    assert_eq!(open(file).unwrap_err().kind, ErrorKind::UnsupportedFeature);
    let err =
        AdpcmMsDecoder::new(&ms_params(3, 63, Vec::new()), &DecodeOptions::new()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::UnsupportedFeature);

    // One frame per block: only sample 2.
    let mut decoder = AdpcmMsDecoder::new(
        &ms_params(1, 7, ms_extension(1, &standard)),
        &DecodeOptions::new(),
    )
    .unwrap();
    let mut buf = akousma_core::AudioBuf::new(
        akousma_core::AudioPlanes::I16(vec![Vec::new()]),
        8000,
        ChannelMask::MONO,
        16,
    )
    .unwrap();
    let packet = akousma_core::Packet {
        data: vec![0, 16, 0, 1, 0, 2, 0],
        ..Default::default()
    };
    decoder.decode(&packet, &mut buf).unwrap();
    assert_eq!(buf.plane::<i16>(0), Some(&[2][..]));
}

#[test]
fn adpcm_reference_parity() {
    // The decoders and the models above agree on many random blocks.
    for seed in 1..40 {
        let data = ima_data(2, 512, 2, seed);
        let mut expected = vec![Vec::new(); 2];
        for block in data.chunks(512) {
            append(&mut expected, ima_block(block, 2, 505));
        }
        assert_eq!(
            decode::<i16>(ima_file(2, 512, 505, &data, None)).unwrap(),
            expected
        );

        let data = ms_data(2, 512, 2, 7, seed);
        let mut expected = vec![Vec::new(); 2];
        for block in data.chunks(512) {
            append(&mut expected, ms_block(block, 2, 500, &MS_COEFFS));
        }
        let ext = ms_extension(500, &standard_pairs());
        assert_eq!(
            decode::<i16>(ms_file(2, 512, &ext, &data)).unwrap(),
            expected
        );
    }
}

/// Decodes one mono Microsoft ADPCM block with the extension `ext`.
fn ms_decode(ext: Vec<u8>, block: &[u8]) -> Vec<i16> {
    let params = ms_params(1, block.len() as u16, ext);
    let mut decoder = AdpcmMsDecoder::new(&params, &DecodeOptions::new()).unwrap();
    let mut buf = akousma_core::AudioBuf::new(
        akousma_core::AudioPlanes::I16(vec![Vec::new()]),
        8000,
        ChannelMask::MONO,
        16,
    )
    .unwrap();
    let packet = akousma_core::Packet {
        data: block.to_vec(),
        ..Default::default()
    };
    decoder.decode(&packet, &mut buf).unwrap();
    buf.plane::<i16>(0).unwrap().to_vec()
}

#[test]
fn ms_adpcm_coefficient_tables_are_not_replaced() {
    let standard = standard_pairs();
    let data = ms_data(1, 20, 1, 1, 9);
    let invalid = |ext: &[u8]| {
        let header = open(ms_file(1, 20, ext, &data)).unwrap_err().kind;
        let params = ms_params(1, 20, ext.to_vec());
        let decoder = AdpcmMsDecoder::new(&params, &DecodeOptions::new()).unwrap_err();
        (header, decoder.kind)
    };
    let both = (ErrorKind::InvalidHeader, ErrorKind::InvalidHeader);
    // Missing, shorter than its fixed part, 0 and 257 pairs.
    assert_eq!(invalid(&[]), both);
    assert_eq!(invalid(&[28, 0]), both);
    assert_eq!(invalid(&ms_extension(28, &[])), both);
    let mut oversized = ms_extension(28, &[[256, 0]; 257]);
    assert_eq!(&oversized[2..4], &257u16.to_le_bytes());
    assert_eq!(invalid(&oversized), both);
    // 257 is rejected for its count, not for its length: 256 pass.
    oversized = ms_extension(28, &[[256, 0]; 256]);
    assert!(open(ms_file(1, 20, &oversized, &data)).is_ok());
    // Seven pairs declared, six and a half present.
    let mut truncated = ms_extension(28, &standard);
    truncated.truncate(truncated.len() - 2);
    assert_eq!(invalid(&truncated), both);
    // Samples per block of 0 or beyond the block.
    assert_eq!(invalid(&ms_extension(0, &standard)), both);
    assert_eq!(invalid(&ms_extension(29, &standard)), both);
    // Bytes after the declared pairs are ignored.
    let mut longer = ms_extension(28, &standard);
    longer.extend_from_slice(&[0xEE; 6]);
    assert!(open(ms_file(1, 20, &longer, &data)).is_ok());

    // Custom pairs, worked out by hand: predictor 0 is (512, -256), delta
    // 16, samples 200 and 100, nibbles 0x1 and 0xF.
    // (200 * 512 - 100 * 256) / 256 = 300, plus 1 * 16 = 316; the delta
    // stays at 16 (230 * 16 >> 8 = 14, raised to 16).
    // (316 * 512 - 200 * 256) / 256 = 432, minus 16 = 416.
    let block = [0, 16, 0, 200, 0, 100, 0, 0x1F];
    let ext = ms_extension(4, &[[512, -256], [0, 0]]);
    assert_eq!(ms_decode(ext, &block), [100, 200, 316, 416]);
    // Predictor 1 of the same table (0, 0): only the nibbles count, 1 * 16
    // and then -1 * 16.
    let block = [1, 16, 0, 200, 0, 100, 0, 0x1F];
    let ext = ms_extension(4, &[[512, -256], [0, 0]]);
    assert_eq!(ms_decode(ext, &block), [100, 200, 16, -16]);
    // A negative prediction is truncated toward zero: pair (1, 0) and
    // sample -1 give -1 / 256 = 0, where a shift by 8 would give -1.
    // Nibble 0.
    let block = [0, 16, 0, 0xFF, 0xFF, 0, 0, 0x00];
    let ext = ms_extension(3, &[[1, 0]]);
    assert_eq!(ms_decode(ext, &block), [0, -1, 0]);
    // The same with a standard pair, predictor 4 (240, 0): -300 * 240 =
    // -72 000, and -72 000 / 256 = -281.25 is truncated to -281 (a shift
    // would give -282); nibble 1 adds the delta 16: -265.
    let block = [4, 16, 0, 0xD4, 0xFE, 0, 0, 0x10];
    let ext = ms_extension(3, &standard);
    assert_eq!(ms_decode(ext, &block), [0, -300, -265]);
}

#[test]
fn ms_adpcm_in_wave_format_extensible_uses_the_standard_table() {
    // The extensible header has room for the samples per block only, in
    // place of the valid bits; the standard pairs are the only ones it can
    // describe.
    let data = ms_data(2, 32, 2, 7, 13);
    let mut expected = vec![Vec::new(); 2];
    for block in data.chunks(32) {
        append(&mut expected, ms_block(block, 2, 11, &MS_COEFFS));
    }
    let ext = fmt_extensible(2, 8000, 32, 4, 11, 0x3, subtype(2));
    let file = riff(&[chunk(b"fmt ", &ext), chunk(b"data", &data)]);
    let demuxer = open(file.clone()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.codec, CodecId::AdpcmMs);
    assert_eq!(decode::<i16>(file).unwrap(), expected);
    // Samples per block of 0 or beyond the block.
    for spb in [0, 25] {
        let ext = fmt_extensible(2, 8000, 32, 4, spb, 0x3, subtype(2));
        let file = riff(&[chunk(b"fmt ", &ext), chunk(b"data", &data)]);
        assert_eq!(open(file).unwrap_err().kind, ErrorKind::InvalidHeader);
    }
}

/// A Microsoft ADPCM file produced by an actual encoder (ffmpeg's native
/// `adpcm_ms`), with the file's own coefficient table, stereo and an eleven
/// block stream; see `../fixtures/README.md` for the provenance of the
/// vector. Decoding is bit-identical to ffmpeg's decode of the same file.
#[test]
fn ms_adpcm_ffmpeg_vector() {
    let bytes = include_bytes!("../fixtures/msadpcm-stereo-22050-ffmpeg.wav");
    let mut demuxer = open(bytes.to_vec()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(11_132));
    let planes = drain::<i16>(&mut demuxer).unwrap();
    assert_eq!(planes[0].len(), 11_132);
    assert_eq!(planes[1].len(), 11_132);
    assert_eq!(fnv1a(&planes), 0xA032_A5B7_1D44_B3D8);
}
