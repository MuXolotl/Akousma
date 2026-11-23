//! Rational time bases, seek requests and the progressive seek index.

use core::num::NonZeroU32;
use std::time::Duration;

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Length of one timestamp tick, `numer / denom` seconds.
///
/// Conversions use 128-bit integer arithmetic, so they never accumulate
/// floating-point error. `(2^64 - 1) * (2^32 - 1) * 10^9` is below `2^128`, so
/// the intermediate products cannot overflow for any timestamp.
///
/// Equality is structural: `1/2` and `2/4` compare unequal.
///
/// ```
/// use akousma_core::units::TimeBase;
/// use std::time::Duration;
///
/// let tb = TimeBase::new(1, 48_000).expect("non-zero terms");
/// assert_eq!(tb.ts_to_duration(72_000), Duration::from_millis(1500));
/// assert_eq!(tb.duration_to_ts(Duration::from_secs(2)), 96_000);
/// assert!(TimeBase::new(1, 0).is_none());
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimeBase {
    numer: NonZeroU32,
    denom: NonZeroU32,
}

impl TimeBase {
    /// Creates a time base of `numer / denom` seconds per tick, or `None` if
    /// either term is zero.
    #[must_use]
    pub const fn new(numer: u32, denom: u32) -> Option<Self> {
        match (NonZeroU32::new(numer), NonZeroU32::new(denom)) {
            (Some(numer), Some(denom)) => Some(Self { numer, denom }),
            _ => None,
        }
    }

    /// Numerator of the tick length in seconds.
    #[must_use]
    pub const fn numer(self) -> u32 {
        self.numer.get()
    }

    /// Denominator of the tick length in seconds.
    #[must_use]
    pub const fn denom(self) -> u32 {
        self.denom.get()
    }

    /// Converts a timestamp to a duration, rounding down to whole nanoseconds.
    ///
    /// Saturates to [`Duration::MAX`] when the number of seconds does not fit
    /// in a `u64` (possible only with `numer > 1`).
    #[must_use]
    pub fn ts_to_duration(self, ts: u64) -> Duration {
        let total_ns = u128::from(ts) * u128::from(self.numer.get()) * NANOS_PER_SEC
            / u128::from(self.denom.get());
        let Ok(secs) = u64::try_from(total_ns / NANOS_PER_SEC) else {
            return Duration::MAX;
        };
        // The remainder is below 10^9 and fits in `u32`.
        Duration::new(secs, (total_ns % NANOS_PER_SEC) as u32)
    }

    /// Converts a duration to a timestamp, rounding down to whole ticks.
    ///
    /// Saturates to `u64::MAX`. Rounding down means that a duration produced
    /// by [`TimeBase::ts_to_duration`] may convert back to the previous tick
    /// when the tick is not a whole number of nanoseconds.
    #[must_use]
    pub fn duration_to_ts(self, duration: Duration) -> u64 {
        // Seconds and nanoseconds are combined here instead of calling
        // `as_nanos` so the numerator bound stays explicit:
        // `(2^64 * 10^9) * (2^32 - 1) < 2^128`.
        let total_ns =
            u128::from(duration.as_secs()) * NANOS_PER_SEC + u128::from(duration.subsec_nanos());
        let ts = total_ns * u128::from(self.denom.get())
            / (u128::from(self.numer.get()) * NANOS_PER_SEC);
        ts.min(u128::from(u64::MAX)) as u64
    }
}

/// Precision of a seek.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SeekMode {
    /// Land exactly on the target: the demuxer positions on the last frame
    /// that starts at or before the target and sets `trim_start` on the first
    /// packet so that decoding resumes at the target sample.
    Accurate,
    /// Land on the nearest index point or key frame at or before the target,
    /// without trimming.
    Coarse,
}

/// Position requested from a demuxer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SeekTarget {
    /// A position in frames (samples per channel) of one track.
    Frame {
        /// Identifier of the track.
        track_id: u32,
        /// Frame index from the start of the track.
        frame: u64,
    },
    /// A position in time.
    Time {
        /// Identifier of the track, or `None` for the default track.
        track_id: Option<u32>,
        /// Time from the start of the track.
        time: Duration,
    },
}

/// Result of a completed seek.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SeekedTo {
    /// Identifier of the track the timestamps refer to.
    pub track_id: u32,
    /// Requested timestamp, in the track's time base.
    pub required_ts: u64,
    /// Timestamp of the first packet that the demuxer will return next.
    pub actual_ts: u64,
}

/// An entry of a [`SeekIndex`]: a timestamp and the byte offset where the
/// frame starting at that timestamp begins.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SeekPoint {
    /// Timestamp in the track's time base.
    pub ts: u64,
    /// Byte offset of the frame in the source.
    pub byte_offset: u64,
}

/// Seek points sorted by timestamp, filled from a container's index or
/// progressively while packets are read.
///
/// Appending a point after the last one is `O(1)`; inserting in the middle is
/// `O(n)`. Timestamps are unique: a point whose timestamp is already present
/// is rejected.
///
/// ```
/// use akousma_core::units::{SeekIndex, SeekPoint};
///
/// let mut index = SeekIndex::new();
/// assert!(index.insert(SeekPoint { ts: 0, byte_offset: 44 }));
/// assert!(index.insert(SeekPoint { ts: 4096, byte_offset: 16_428 }));
/// assert_eq!(index.search(5000), Some(SeekPoint { ts: 4096, byte_offset: 16_428 }));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeekIndex {
    points: Vec<SeekPoint>,
}

impl SeekIndex {
    /// Creates an empty index.
    #[must_use]
    pub const fn new() -> Self {
        Self { points: Vec::new() }
    }

    /// Adds a point. Returns `false`, leaving the index unchanged, if a point
    /// with the same timestamp is already present.
    pub fn insert(&mut self, point: SeekPoint) -> bool {
        match self.points.last() {
            None => {
                self.points.push(point);
                return true;
            }
            Some(last) if last.ts < point.ts => {
                self.points.push(point);
                return true;
            }
            Some(_) => {}
        }
        match self.points.binary_search_by_key(&point.ts, |p| p.ts) {
            Ok(_) => false,
            Err(pos) => {
                self.points.insert(pos, point);
                true
            }
        }
    }

    /// Returns the last point with a timestamp at or before `ts`, or `None` if
    /// every point lies after `ts`.
    #[must_use]
    pub fn search(&self, ts: u64) -> Option<SeekPoint> {
        let after = self.points.partition_point(|p| p.ts <= ts);
        after.checked_sub(1).map(|i| self.points[i])
    }

    /// All points in ascending timestamp order.
    #[must_use]
    pub fn points(&self) -> &[SeekPoint] {
        &self.points
    }

    /// Number of points.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Returns `true` if the index holds no points.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Removes every point, keeping the allocated capacity.
    pub fn clear(&mut self) {
        self.points.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(numer: u32, denom: u32) -> TimeBase {
        TimeBase::new(numer, denom).unwrap()
    }

    #[test]
    fn timebase_u128_extremes() {
        // Expected values: floor((2^64 - 1) * 10^9 / rate), split into seconds
        // and nanoseconds with Python integers.
        assert_eq!(
            tb(1, 48_000).ts_to_duration(u64::MAX),
            Duration::new(384_307_168_202_282, 325_312_500)
        );
        assert_eq!(
            tb(1, 90_000).ts_to_duration(u64::MAX),
            Duration::new(204_963_823_041_217, 240_166_666)
        );
        // (2^64 - 1) * (2^32 - 1) seconds do not fit in a u64.
        assert_eq!(tb(u32::MAX, 1).ts_to_duration(u64::MAX), Duration::MAX);
        // 2^64 seconds is the first value past the boundary; 2^64 - 1 fits.
        assert_eq!(tb(2, 1).ts_to_duration(1 << 63), Duration::MAX);
        assert_eq!(
            tb(2, 1).ts_to_duration((1 << 63) - 1),
            Duration::from_secs(u64::MAX - 1)
        );
        assert_eq!(
            tb(1, 1).ts_to_duration(u64::MAX),
            Duration::from_secs(u64::MAX)
        );
        assert_eq!(tb(u32::MAX, u32::MAX).ts_to_duration(0), Duration::ZERO);

        assert_eq!(tb(1, 48_000).duration_to_ts(Duration::MAX), u64::MAX);
        assert_eq!(tb(1, u32::MAX).duration_to_ts(Duration::MAX), u64::MAX);
        assert_eq!(tb(u32::MAX, 1).duration_to_ts(Duration::MAX), 4_294_967_297);
        assert_eq!(tb(1, 48_000).duration_to_ts(Duration::ZERO), 0);
    }

    #[test]
    fn timebase_rounding_and_common_scales() {
        assert!(TimeBase::new(0, 1).is_none());
        assert!(TimeBase::new(1, 0).is_none());
        assert_eq!(tb(3, 7).numer(), 3);
        assert_eq!(tb(3, 7).denom(), 7);

        // One tick of 1/3 s is 333_333_333.33 ns: rounded down both ways.
        assert_eq!(
            tb(1, 3).ts_to_duration(1),
            Duration::from_nanos(333_333_333)
        );
        assert_eq!(
            tb(1, 3).duration_to_ts(Duration::from_nanos(333_333_333)),
            0
        );
        assert_eq!(
            tb(1, 3).duration_to_ts(Duration::from_nanos(333_333_334)),
            1
        );

        // Matroska with the default TimecodeScale of 1 ms.
        let mkv = tb(1_000_000, 1_000_000_000);
        assert_eq!(mkv.ts_to_duration(1500), Duration::from_millis(1500));
        assert_eq!(mkv.duration_to_ts(Duration::from_millis(1500)), 1500);

        // MPEG-TS 90 kHz clock and a 44.1 kHz sample clock.
        assert_eq!(
            tb(1, 90_000).ts_to_duration(90_000 * 3600),
            Duration::from_secs(3600)
        );
        assert_eq!(
            tb(1, 44_100).ts_to_duration(44_101),
            Duration::new(1, 22_675)
        );
        assert_eq!(
            tb(1, 44_100).duration_to_ts(Duration::from_secs(10_000_000_000)),
            441_000_000_000_000
        );
    }

    fn point(ts: u64, byte_offset: u64) -> SeekPoint {
        SeekPoint { ts, byte_offset }
    }

    #[test]
    fn seek_index_insert_and_search() {
        let mut index = SeekIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.search(0), None);
        assert_eq!(index.search(u64::MAX), None);

        assert!(index.insert(point(100, 1000)));
        assert!(index.insert(point(300, 3000)));
        assert!(index.insert(point(200, 2000)));
        assert!(index.insert(point(0, 44)));
        assert!(!index.insert(point(200, 9999)));
        assert!(!index.insert(point(300, 9999)));
        assert_eq!(
            index.points(),
            &[
                point(0, 44),
                point(100, 1000),
                point(200, 2000),
                point(300, 3000)
            ]
        );
        assert_eq!(index.len(), 4);

        assert_eq!(index.search(0), Some(point(0, 44)));
        assert_eq!(index.search(99), Some(point(0, 44)));
        assert_eq!(index.search(100), Some(point(100, 1000)));
        assert_eq!(index.search(299), Some(point(200, 2000)));
        assert_eq!(index.search(u64::MAX), Some(point(300, 3000)));

        let mut late = SeekIndex::new();
        assert!(late.insert(point(u64::MAX, 7)));
        assert_eq!(late.search(u64::MAX - 1), None);
        assert_eq!(late.search(u64::MAX), Some(point(u64::MAX, 7)));

        index.clear();
        assert!(index.is_empty());
        assert_eq!(index.search(300), None);
    }
}
