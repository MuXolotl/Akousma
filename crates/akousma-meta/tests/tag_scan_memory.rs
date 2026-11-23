//! Checks that the tag scan without `read_metadata` does not read a large
//! tag into memory: an APEv2 tag with a 20 MB picture is checked through a
//! fixed buffer, within a few KiB of heap and of scanned bytes.
//!
//! A counting global allocator needs an `unsafe impl`; it lives in this test
//! crate only, the library itself forbids `unsafe` code. Counting is limited
//! to the current thread, so the test harness cannot disturb it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Cursor;

use akousma_core::io::ByteReader;
use akousma_core::meta::MetadataBudget;
use akousma_core::{DemuxOptions, FormatLimits};
use akousma_meta::anchors;

struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// Adds `grown` bytes to the live heap of this thread and removes `shrunk`.
fn record(grown: usize, shrunk: usize) {
    // `try_with` fails only while the thread is being torn down.
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = LIVE.try_with(|live| {
                let now = live.get().saturating_add(grown).saturating_sub(shrunk);
                live.set(now);
                let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
            });
        }
    });
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// added bookkeeping only touches `const`-initialized thread-locals, which
// never allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(0, layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size, layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Largest heap in use on this thread while `f` runs, beyond what was in
/// use before it.
fn peak_heap<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    COUNTING.with(|counting| counting.set(true));
    let result = f();
    COUNTING.with(|counting| counting.set(false));
    (result, PEAK.with(Cell::get))
}

#[test]
fn large_picture_is_checked_in_place() {
    const PICTURE: usize = 20_000_000;
    let mut value = b"front.jpg\0\xFF\xD8".to_vec();
    value.resize(PICTURE, 0x55);
    let mut items = u32::try_from(value.len()).unwrap().to_le_bytes().to_vec();
    items.extend_from_slice(&(1u32 << 1).to_le_bytes()); // binary
    items.extend_from_slice(b"Cover Art (Front)\0");
    items.extend_from_slice(&value);
    drop(value);
    let mut file = vec![0u8; 1000]; // audio
    let size = u32::try_from(items.len() + 32).unwrap();
    file.extend_from_slice(&items);
    drop(items);
    file.extend_from_slice(b"APETAGEX");
    for field in [2000u32, size, 1, 0, 0, 0] {
        file.extend_from_slice(&field.to_le_bytes());
    }
    let len = file.len() as u64;

    let scan = |options: &DemuxOptions| {
        let mut budget = MetadataBudget::new(&options.limits);
        let mut reader = ByteReader::new(Cursor::new(&file));
        let anchors = anchors::scan(&mut reader, len, options, &mut budget).unwrap();
        let scanned = options.limits.max_tag_scan_bytes - budget.remaining_scan_bytes();
        (anchors.audio_end, anchors.trailing.len(), scanned)
    };

    // Without metadata: the default limits, the tag ends the audio, and
    // neither the heap nor the scanned bytes grow with the picture.
    let quiet = DemuxOptions::new().with_read_metadata(false);
    let ((end, revisions, scanned), peak) = peak_heap(|| scan(&quiet));
    assert_eq!((end, revisions), (1000, 0));
    assert!(peak < 64 * 1024, "{peak} bytes of heap");
    assert!(scanned <= 2 * 8192, "{scanned} bytes scanned");
    // With a limit of scanned bytes far below the tag.
    let tight = DemuxOptions::new()
        .with_read_metadata(false)
        .with_limits(FormatLimits::new().with_max_tag_scan_bytes(scanned));
    assert_eq!(scan(&tight).0, 1000);

    // With metadata the picture is read: this is what the check avoids.
    let ((end, revisions, _), peak) = peak_heap(|| scan(&DemuxOptions::new()));
    assert_eq!((end, revisions), (1000, 1));
    assert!(peak > PICTURE, "{peak} bytes of heap");
}
