//! Seeks around a file and decodes a few packets after every seek.
//!
//! Input: one byte `n`, then `n % 5` seeks of five bytes each, then the file.
//! A seek is a flags byte (bit 0: accurate, else coarse; bit 1: the value is a
//! time in milliseconds, else a frame) and a little-endian `u32`. A frame
//! position is taken modulo twice the length of the track plus one when the
//! length is known, so that most seeks land inside the track and some past
//! its end. Before the seeks two packets are read; after each successful seek
//! the decoder is reset and three packets are read; at the end the track is
//! read to its end and the decoder is finalized.

#![no_main]

mod common;

use std::time::Duration;

use akousma::{SeekMode, SeekTarget};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&count, rest)) = data.split_first() else {
        return;
    };
    let Some((seeks, file)) = rest.split_at_checked(usize::from(count % 5) * 5) else {
        return;
    };
    let Some(mut session) = common::Session::open(file) else {
        return;
    };
    session.read(2);
    for seek in seeks.as_chunks::<5>().0 {
        let [flags, value @ ..] = *seek;
        let value = u64::from(u32::from_le_bytes(value));
        let mode = if flags & 1 != 0 {
            SeekMode::Accurate
        } else {
            SeekMode::Coarse
        };
        let target = if flags & 2 != 0 {
            SeekTarget::Time {
                track_id: None,
                time: Duration::from_millis(value),
            }
        } else {
            let span = session
                .track
                .params
                .total_frames
                .map_or(u64::MAX, |total| total.saturating_mul(2).saturating_add(1));
            SeekTarget::Frame {
                track_id: session.track.id,
                frame: value % span,
            }
        };
        if session.demuxer.seek(mode, target).is_ok() {
            session.decoder.reset();
            session.read(3);
        }
    }
    if session.read(usize::MAX) {
        let _ = session.decoder.finalize();
    }
});
