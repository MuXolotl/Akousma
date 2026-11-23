//! Checks that the steady-state read and decode loop does not touch the
//! heap, for PCM and both ADPCM codecs.
//!
//! A counting global allocator needs an `unsafe impl`; it lives in this test
//! crate only, the library itself forbids `unsafe` code. Counting is limited
//! to the current thread, so the test harness cannot disturb it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use std::io::Cursor;

use akousma::{AudioBuf, AudioPlanes, ChannelMask, DecodeOptions, DemuxOptions, Hint, Packet};

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn record() {
    // `try_with` fails only while the thread is being torn down.
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// added bookkeeping only touches `const`-initialized thread-locals, which
// never allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Number of heap allocations made by `f` on this thread.
fn allocations_in(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|count| count.set(0));
    COUNTING.with(|counting| counting.set(true));
    f();
    COUNTING.with(|counting| counting.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// A stereo file at 44.1 kHz; `extension` follows `cbSize` if not empty.
fn wav(tag: u16, block_align: u16, bits: u16, extension: &[u8], data: &[u8]) -> Vec<u8> {
    let mut fmt = Vec::new();
    for field in [tag, 2] {
        fmt.extend_from_slice(&field.to_le_bytes());
    }
    fmt.extend_from_slice(&44_100u32.to_le_bytes());
    fmt.extend_from_slice(&(44_100 * u32::from(block_align)).to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if !extension.is_empty() {
        fmt.extend_from_slice(&(extension.len() as u16).to_le_bytes());
        fmt.extend_from_slice(extension);
    }
    let fmt_len = fmt.len() as u32;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(4 + 8 + fmt_len + 8 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&fmt_len.to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// `ADPCMWAVEFORMAT` for stereo blocks of 2048 bytes: 2036 samples per
/// block and the seven standard coefficient pairs.
fn ms_extension() -> Vec<u8> {
    let mut out = 2036u16.to_le_bytes().to_vec();
    out.extend_from_slice(&7u16.to_le_bytes());
    for coeff in [
        256, 0, 512, -256, 0, 0, 192, 64, 240, 0, 460, -208, 392, -232i16,
    ] {
        out.extend_from_slice(&coeff.to_le_bytes());
    }
    out
}

/// Bytes that keep ADPCM headers valid: predictor index 0 or step index 0.
fn body(len: usize, block_align: usize, header: usize) -> Vec<u8> {
    let mut data: Vec<u8> = (0..len).map(|i| (i * 131 % 253) as u8).collect();
    for block in data.chunks_mut(block_align) {
        let len = header.min(block.len());
        block[..len].fill(0);
    }
    data
}

#[test]
fn read_and_decode_loop_does_not_allocate() {
    // The counter itself works.
    assert!(allocations_in(|| drop(vec![0u8; 16])) >= 1);

    let files = [
        ("pcm", wav(1, 4, 16, &[], &body(4 * 4096 * 6 + 12, 4, 0))),
        (
            "ima",
            wav(0x11, 2048, 4, &[], &body(2048 * 40 + 100, 2048, 8)),
        ),
        (
            "ms",
            wav(
                0x02,
                2048,
                4,
                &ms_extension(),
                &body(2048 * 40 + 100, 2048, 14),
            ),
        ),
    ];
    for (name, file) in files {
        let mut probed = akousma::default_probe()
            .open(&Hint::new(), Cursor::new(file), &DemuxOptions::new())
            .unwrap();
        let params = probed.demuxer.tracks()[0].params.clone();
        let mut decoder = akousma::default_codecs()
            .make(&params, &DecodeOptions::new())
            .unwrap();
        let mut buf = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new()]),
            1,
            ChannelMask::EMPTY,
            16,
        )
        .unwrap();
        let mut packet = Packet::default();
        let mut interleaved = vec![0.0f32; 1 << 16];
        // The first packet sizes the buffers.
        assert!(probed.demuxer.next_packet(&mut packet).unwrap());
        decoder.decode(&packet, &mut buf).unwrap();

        let mut packets = 0;
        let count = allocations_in(|| {
            while probed.demuxer.next_packet(&mut packet).unwrap() {
                decoder.decode(&packet, &mut buf).unwrap();
                buf.copy_interleaved_f32(&mut interleaved);
                packets += 1;
            }
        });
        assert!(packets >= 4, "{name}: {packets} packets");
        assert_eq!(count, 0, "{name}");
    }
}
