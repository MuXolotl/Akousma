//! Four stereo WAVE files generated in memory: a deterministic, non-silent
//! signal (three tones per channel plus `xorshift64*` noise) stored as 16-bit
//! PCM, 24-bit PCM, 32-bit float and IMA ADPCM.

pub const RATE: u32 = 44_100;
const SECONDS: usize = 30;
/// IMA ADPCM block of 2048 bytes for two channels: one header sample and
/// 2040 coded samples per channel.
const IMA_BLOCK: u16 = 2048;
const IMA_FRAMES_PER_BLOCK: usize = 2041;

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const IMA_INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Interleaved stereo samples in `-0.75..0.75`.
fn signal(frames: usize) -> Vec<f32> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut noise = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let bits = state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        bits as f64 / f64::from(1u32 << 23) - 1.0
    };
    let tau = std::f64::consts::TAU / f64::from(RATE);
    let mut out = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let t = n as f64 * tau;
        for (base, phase) in [(220.0, 0.0), (329.63, 1.0)] {
            let tones = 0.35 * (base * t).sin()
                + 0.2 * (2.5 * base * t + phase).sin()
                + 0.1 * (7.1 * base * t).sin();
            out.push((tones + 0.08 * noise()) as f32);
        }
    }
    out
}

/// A RIFF WAVE file: `fmt ` with `cbSize` and `extra`, `fact` and `data`.
fn riff(format: u16, bits: u16, align: u16, extra: &[u8], frames: usize, data: &[u8]) -> Vec<u8> {
    let per_block = if format == 0x11 {
        IMA_FRAMES_PER_BLOCK
    } else {
        1
    };
    let byte_rate = (RATE as usize * usize::from(align) / per_block) as u32;
    let fmt_len = 18 + extra.len() as u32;
    let riff_len = 4 + 8 + fmt_len + 12 + 8 + data.len() as u32;
    [
        b"RIFF".as_slice(),
        &riff_len.to_le_bytes(),
        b"WAVEfmt ",
        &fmt_len.to_le_bytes(),
        &format.to_le_bytes(),
        &2u16.to_le_bytes(),
        &RATE.to_le_bytes(),
        &byte_rate.to_le_bytes(),
        &align.to_le_bytes(),
        &bits.to_le_bytes(),
        &(extra.len() as u16).to_le_bytes(),
        extra,
        b"fact\x04\x00\x00\x00",
        &(frames as u32).to_le_bytes(),
        b"data",
        &(data.len() as u32).to_le_bytes(),
        data,
    ]
    .concat()
}

/// Codes one sample and updates the predictor `(sample, step index)`.
fn ima_nibble(state: &mut (i32, i32), sample: i16) -> u8 {
    let step = IMA_STEPS[state.1 as usize];
    let (mut diff, mut code) = (i32::from(sample) - state.0, 0u8);
    if diff < 0 {
        (diff, code) = (-diff, 8);
    }
    let mut delta = step >> 3;
    for (bit, part) in [(4, step), (2, step >> 1), (1, step >> 2)] {
        if diff >= part {
            (code, diff, delta) = (code | bit, diff - part, delta + part);
        }
    }
    let delta = if code & 8 == 0 { delta } else { -delta };
    state.0 = (state.0 + delta).clamp(-32_768, 32_767);
    state.1 = (state.1 + IMA_INDEX[usize::from(code & 7)]).clamp(0, 88);
    code
}

fn ima(pcm: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut states = [(0, 0); 2];
    for block in pcm.as_chunks::<{ IMA_FRAMES_PER_BLOCK * 2 }>().0 {
        for (ch, state) in states.iter_mut().enumerate() {
            state.0 = i32::from(block[ch]);
            out.extend_from_slice(&block[ch].to_le_bytes());
            out.extend_from_slice(&[state.1 as u8, 0]);
        }
        // Eight frames per group: four bytes per channel, low nibble first.
        for group in block[2..].as_chunks::<16>().0 {
            for (ch, state) in states.iter_mut().enumerate() {
                for k in 0..4 {
                    let low = ima_nibble(state, group[ch + 4 * k]);
                    out.push(low | ima_nibble(state, group[ch + 4 * k + 2]) << 4);
                }
            }
        }
    }
    out
}

/// A file for each of the four encodings: 16-bit PCM, 24-bit PCM, 32-bit
/// float and IMA ADPCM.
pub fn inputs() -> Vec<Vec<u8>> {
    let frames = RATE as usize * SECONDS;
    let signal = signal(frames);
    let pcm16: Vec<i16> = signal
        .iter()
        .map(|s| (s * 32_767.0).round() as i16)
        .collect();
    let s16: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
    let s24: Vec<u8> = (signal.iter())
        .flat_map(|s| ((s * 8_388_607.0).round() as i32).to_le_bytes()[..3].to_vec())
        .collect();
    let f32: Vec<u8> = signal.iter().flat_map(|s| s.to_le_bytes()).collect();
    let blocks = frames / IMA_FRAMES_PER_BLOCK;
    let ima_frames = blocks * IMA_FRAMES_PER_BLOCK;
    let ima_extra = (IMA_FRAMES_PER_BLOCK as u16).to_le_bytes();
    vec![
        riff(1, 16, 4, &[], frames, &s16),
        riff(1, 24, 6, &[], frames, &s24),
        riff(3, 32, 8, &[], frames, &f32),
        riff(0x11, 4, IMA_BLOCK, &ima_extra, ima_frames, &ima(&pcm16)),
    ]
}
