//! Adversarial files: one table of hostile or unusual inputs and what each
//! of them must do. Each expectation follows a stated rule of the format,
//! not the output of the implementation; a decoded file must give exactly the
//! listed samples.

use akousma_core::ErrorKind;

use crate::adpcm::{ima_block, ima_data};
use crate::common::{chunk, decode, fmt, riff, wave};
use crate::containers::{ds64, raw_chunk, w64, w64_chunk};

/// What a file must do when it is opened and decoded to the end.
#[derive(Debug)]
pub enum Expect {
    /// It decodes without an error, to the samples of the case.
    Decodes,
    /// Opening or decoding it fails with this kind.
    Rejects(ErrorKind),
}

pub struct Case {
    pub name: &'static str,
    pub file: Vec<u8>,
    pub expect: Expect,
    /// The output of a decoded file, plane by plane; empty when rejected.
    pub planes: Vec<Vec<i16>>,
}

fn decodes(name: &'static str, file: Vec<u8>, planes: Vec<Vec<i16>>) -> Case {
    let expect = Expect::Decodes;
    Case {
        name,
        file,
        expect,
        planes,
    }
}

fn rejects(name: &'static str, file: Vec<u8>, kind: ErrorKind) -> Case {
    let expect = Expect::Rejects(kind);
    Case {
        name,
        file,
        expect,
        planes: Vec::new(),
    }
}

/// Mono 16-bit samples 1 and 2.
const PCM: [u8; 4] = [1, 0, 2, 0];

/// `fmt ` chunk of mono 16-bit PCM at 8 kHz.
fn fmt16() -> Vec<u8> {
    chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16))
}

/// `fmt ` chunk of mono 8-bit PCM at 8 kHz.
fn fmt8() -> Vec<u8> {
    chunk(b"fmt ", &fmt(1, 1, 8000, 1, 8))
}

/// 8-bit samples 0, 128 and 255 and what they decode to.
const PCM8: [u8; 3] = [0, 128, 255];
const PCM8_DECODED: [i16; 3] = [-32768, 0, 32512];

/// A `LIST` of type `INFO` with an `INAM` field.
fn info(title: &str) -> Vec<u8> {
    let mut name = title.as_bytes().to_vec();
    name.push(0);
    chunk(b"LIST", &[b"INFO".to_vec(), chunk(b"INAM", &name)].concat())
}

fn set_u32(file: &mut [u8], at: usize, value: u32) {
    file[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// An IMA ADPCM `fmt ` payload, with `wSamplesPerBlock` when given.
fn ima_fmt(channels: u16, align: u16, samples_per_block: Option<u16>) -> Vec<u8> {
    let mut out = fmt(0x11, channels, 8000, align, 4);
    match samples_per_block {
        Some(spb) => [2u16, spb].iter().for_each(|v| out.extend(v.to_le_bytes())),
        None => out.extend(0u16.to_le_bytes()),
    }
    out
}

/// The model output of IMA ADPCM `data` cut in blocks of `align` bytes, with
/// `frames` frames in every block.
fn ima_model(data: &[u8], channels: usize, align: usize, frames: &[usize]) -> Vec<Vec<i16>> {
    let mut planes = vec![Vec::new(); channels];
    for (block, &frames) in data.chunks(align).zip(frames) {
        for (plane, more) in planes.iter_mut().zip(ima_block(block, channels, frames)) {
            plane.extend(more);
        }
    }
    planes
}

/// The table, with the rule behind each expectation.
pub fn cases() -> Vec<Case> {
    let mut cases = Vec::new();

    // `data` before `fmt ` on a seekable source: the walk finds both.
    let file = riff(&[chunk(b"data", &PCM), fmt16()]);
    cases.push(decodes("fmt_after_data", file, vec![vec![1, 2]]));

    // Unknown and metadata chunks before `fmt ` are skipped.
    let file = riff(&[
        chunk(b"JUNK", &[0; 28]),
        info("Before"),
        fmt16(),
        chunk(b"data", &PCM),
    ]);
    cases.push(decodes("junk_and_list_before_fmt", file, vec![vec![1, 2]]));

    // Odd chunks are followed by a pad byte that is not part of them: an
    // unknown chunk of 3 bytes before `data`, odd `data`, a list after it.
    let odd = chunk(b"odd ", &[7; 3]);
    let file = riff(&[fmt8(), odd, chunk(b"data", &PCM8), info("After")]);
    cases.push(decodes(
        "odd_chunk_padding",
        file,
        vec![PCM8_DECODED.to_vec()],
    ));

    // The last odd chunk without its pad byte, the root size counting the
    // bytes that are there: the source ends inside the root.
    let mut file = riff(&[fmt8(), chunk(b"data", &PCM8)]);
    file.pop();
    let size = file.len() as u32 - 8;
    set_u32(&mut file, 4, size);
    cases.push(decodes(
        "odd_chunk_padding_at_eof",
        file,
        vec![PCM8_DECODED.to_vec()],
    ));

    // A truncated file: root and `data` declare 1000 bytes of audio, 5 are
    // there. The audio is cut to the source and to whole frames.
    let mut file = riff(&[fmt16(), raw_chunk(b"data", 1000, &[1, 0, 2, 0, 3])]);
    set_u32(&mut file, 4, 4 + 24 + 8 + 1000);
    cases.push(decodes("data_larger_than_file", file, vec![vec![1, 2]]));

    // `data` larger than the root that bounds the file.
    let file = riff(&[fmt16(), raw_chunk(b"data", 1000, &PCM)]);
    cases.push(rejects(
        "data_larger_than_root",
        file,
        ErrorKind::InvalidHeader,
    ));

    // An empty `data` in a bounded root is empty audio; only an unbounded
    // root makes it an unfinished recording.
    let file = riff(&[fmt16(), chunk(b"data", &[])]);
    cases.push(decodes("data_zero_length", file, vec![Vec::new()]));

    // RF64 whose sizes point to a `ds64` that is not there: `fmt ` and
    // `data` before `ds64`, which must come first.
    let mut file = wave(b"RF64", &[fmt16(), raw_chunk(b"data", u32::MAX, &PCM)]);
    set_u32(&mut file, 4, u32::MAX);
    cases.push(rejects("rf64_without_ds64", file, ErrorKind::InvalidHeader));

    // `ds64` declares more audio than its own root size holds.
    let rf64 = |data_size, sample_count| {
        let ds64 = chunk(b"ds64", &ds64(data_size, sample_count));
        let mut file = wave(
            b"RF64",
            &[ds64, fmt16(), raw_chunk(b"data", u32::MAX, &PCM)],
        );
        set_u32(&mut file, 4, u32::MAX);
        let size = file.len() as u64 - 8;
        file[20..28].copy_from_slice(&size.to_le_bytes());
        file
    };
    let file = rf64(1_000_000, 0);
    cases.push(rejects("ds64_lies", file, ErrorKind::InvalidHeader));
    // A sample count of 1 for 2 frames of PCM: PCM takes its length from the
    // bytes only.
    cases.push(decodes(
        "ds64_lies_sample_count",
        rf64(4, 1),
        vec![vec![1, 2]],
    ));

    // Wave64: 18-byte `fmt ` (6 pad bytes), a 5-byte chunk (3), 6 bytes of
    // audio (2) and a chunk after them, all on 8-byte boundaries.
    let mut fmt18 = fmt(1, 1, 8000, 2, 16);
    fmt18.extend([0, 0]);
    let file = w64(&[
        w64_chunk(b"fmt ", &fmt18),
        w64_chunk(b"junk", &[9; 5]),
        w64_chunk(b"data", &[1, 0, 2, 0, 3, 0]),
        w64_chunk(b"junk", &[9; 3]),
    ]);
    cases.push(decodes("w64_odd_alignment", file, vec![vec![1, 2, 3]]));

    // The file ends inside `fmt `: a truncated `fmt ` is an unexpected end,
    // as a truncated metadata chunk is.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM)]);
    file.truncate(12 + 8 + 10);
    cases.push(rejects("fmt_past_eof", file, ErrorKind::UnexpectedEof));

    // A little-endian file labelled RIFX: read as big-endian, the root ends
    // far past the source (a truncated file) and so does `fmt `: the rule of
    // the case above.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM)]);
    file[..4].copy_from_slice(b"RIFX");
    cases.push(rejects("rifx_le_data", file, ErrorKind::UnexpectedEof));

    // `nAvgBytesPerSec` is informative only.
    let mut f = fmt(1, 1, 8000, 2, 16);
    set_u32(&mut f, 8, 128_000);
    let file = riff(&[chunk(b"fmt ", &f), chunk(b"data", &PCM)]);
    cases.push(decodes("byterate_wrong", file, vec![vec![1, 2]]));

    // IMA ADPCM without `wSamplesPerBlock` (`cbSize` 0): the block holds
    // (36 - 4) * 2 + 1 = 65 frames, the formula of Microsoft.
    let data = ima_data(1, 36, 2, 7);
    let planes = ima_model(&data, 1, 36, &[65, 65]);
    let file = riff(&[chunk(b"fmt ", &ima_fmt(1, 36, None)), chunk(b"data", &data)]);
    cases.push(decodes("adpcm_cbsize_zero", file, planes));
    // MS ADPCM needs its extension with the coefficients: there is
    // no table to fall back to outside `WAVE_FORMAT_EXTENSIBLE`.
    let mut f = fmt(2, 1, 8000, 32, 4);
    f.extend(0u16.to_le_bytes());
    let file = riff(&[chunk(b"fmt ", &f), chunk(b"data", &[0; 32])]);
    cases.push(rejects(
        "adpcm_cbsize_zero_ms",
        file,
        ErrorKind::InvalidHeader,
    ));

    // An odd block size. Mono: 37 bytes hold 1 + 33 * 2 = 67 frames. Stereo:
    // 41 bytes hold 1 + 4 * 8 = 33 frames and one unused byte.
    let data = ima_data(1, 37, 2, 11);
    let planes = ima_model(&data, 1, 37, &[67, 67]);
    let file = riff(&[
        chunk(b"fmt ", &ima_fmt(1, 37, Some(67))),
        chunk(b"data", &data),
    ]);
    cases.push(decodes("adpcm_blockalign_odd", file, planes));
    let data = ima_data(2, 41, 2, 13);
    let planes = ima_model(&data, 2, 41, &[33, 33]);
    let file = riff(&[
        chunk(b"fmt ", &ima_fmt(2, 41, Some(33))),
        chunk(b"data", &data),
    ]);
    cases.push(decodes("adpcm_blockalign_odd_stereo", file, planes));

    // The last stereo block cut to 19 of 40 bytes: header 8 and one whole
    // group of 4 bytes per channel, 1 + 8 = 9 frames; the 3 bytes after it
    // are not a whole group.
    let mut data = ima_data(2, 40, 2, 17);
    data.truncate(40 + 19);
    let planes = ima_model(&data, 2, 40, &[33, 9]);
    let file = riff(&[
        chunk(b"fmt ", &ima_fmt(2, 40, Some(33))),
        chunk(b"data", &data),
    ]);
    cases.push(decodes("adpcm_last_block_cut", file, planes));

    // A step index of 89 in a block header: past the end of the step table.
    let mut data = ima_data(1, 36, 1, 19);
    data[2] = 89;
    let file = riff(&[
        chunk(b"fmt ", &ima_fmt(1, 36, Some(65))),
        chunk(b"data", &data),
    ]);
    cases.push(rejects("ima_index_over_88", file, ErrorKind::InvalidData));

    // No channels: `nChannels` must be in `1..=64`.
    let file = riff(&[
        chunk(b"fmt ", &fmt(1, 0, 8000, 2, 16)),
        chunk(b"data", &PCM),
    ]);
    cases.push(rejects("channels_zero", file, ErrorKind::InvalidHeader));

    cases
}

#[test]
fn adversarial_files() {
    let cases = cases();
    let mut names: Vec<_> = cases.iter().map(|case| case.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "case names must be unique");

    let mut wrong = Vec::new();
    for case in cases {
        let size = case.file.len();
        let got = decode::<i16>(case.file);
        let right = match (&case.expect, &got) {
            (Expect::Decodes, Ok(planes)) => *planes == case.planes,
            (Expect::Rejects(kind), Err(err)) => err.kind == *kind,
            _ => false,
        };
        if !right {
            let got = got.map(|planes| planes.iter().map(Vec::len).collect::<Vec<_>>());
            let expect = &case.expect;
            wrong.push(format!(
                "{} ({size} bytes): expected {expect:?}, got {got:?} (frames per plane)",
                case.name
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
