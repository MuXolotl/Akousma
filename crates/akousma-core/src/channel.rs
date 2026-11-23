//! Speaker positions, channel masks and the canonical plane order.
//!
//! Planes of an [`AudioBuf`](crate::audio::AudioBuf) are always stored in the
//! canonical order: first one plane per bit set in the [`ChannelMask`], in
//! ascending bit order, then the planes without a speaker position. Decoders
//! of formats that use another order (Vorbis, Opus, AAC) write each decoded
//! channel straight into plane [`ChannelMask::plane_index`] of its position.

/// Speaker position, numbered as the bits of the Microsoft
/// `WAVEFORMATEXTENSIBLE` `dwChannelMask` field.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum ChannelPosition {
    /// Front left (`SPEAKER_FRONT_LEFT`, bit 0).
    FrontLeft = 0,
    /// Front right (bit 1).
    FrontRight = 1,
    /// Front center (bit 2).
    FrontCenter = 2,
    /// Low-frequency effects (bit 3).
    Lfe1 = 3,
    /// Back left, also called rear left or surround left (bit 4).
    BackLeft = 4,
    /// Back right (bit 5).
    BackRight = 5,
    /// Front left of center (bit 6).
    FrontLeftCenter = 6,
    /// Front right of center (bit 7).
    FrontRightCenter = 7,
    /// Back center (bit 8).
    BackCenter = 8,
    /// Side left (bit 9).
    SideLeft = 9,
    /// Side right (bit 10).
    SideRight = 10,
    /// Top center (bit 11).
    TopCenter = 11,
    /// Top front left (bit 12).
    TopFrontLeft = 12,
    /// Top front center (bit 13).
    TopFrontCenter = 13,
    /// Top front right (bit 14).
    TopFrontRight = 14,
    /// Top back left (bit 15).
    TopBackLeft = 15,
    /// Top back center (bit 16).
    TopBackCenter = 16,
    /// Top back right (bit 17).
    TopBackRight = 17,
}

impl ChannelPosition {
    /// Every position in ascending bit order.
    pub const ALL: [Self; 18] = [
        Self::FrontLeft,
        Self::FrontRight,
        Self::FrontCenter,
        Self::Lfe1,
        Self::BackLeft,
        Self::BackRight,
        Self::FrontLeftCenter,
        Self::FrontRightCenter,
        Self::BackCenter,
        Self::SideLeft,
        Self::SideRight,
        Self::TopCenter,
        Self::TopFrontLeft,
        Self::TopFrontCenter,
        Self::TopFrontRight,
        Self::TopBackLeft,
        Self::TopBackCenter,
        Self::TopBackRight,
    ];

    /// The single mask bit of this position.
    #[must_use]
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// Set of speaker positions carried by the first planes of a buffer.
///
/// A mask may describe fewer positions than there are channels: the first
/// `count()` planes then carry the positions in ascending bit order and the
/// remaining planes are discrete channels without a position. An empty mask
/// means that no plane has a position (multitrack recordings, Ambisonics).
///
/// ```
/// use akousma_core::channel::{ChannelMask, ChannelPosition};
///
/// let mask = ChannelMask::from_raw(0x3F); // 5.1
/// assert_eq!(mask.count(), 6);
/// assert_eq!(mask.plane_index(ChannelPosition::Lfe1), Some(3));
/// assert_eq!(mask.plane_index(ChannelPosition::SideLeft), None);
/// assert_eq!(mask.fit(2), ChannelMask::EMPTY);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChannelMask(u32);

impl ChannelMask {
    /// No positions.
    pub const EMPTY: Self = Self(0);
    /// One front center speaker.
    pub const MONO: Self = Self(ChannelPosition::FrontCenter.bit());
    /// Front left and front right.
    pub const STEREO: Self =
        Self(ChannelPosition::FrontLeft.bit() | ChannelPosition::FrontRight.bit());
    /// Bits that have a [`ChannelPosition`].
    pub const DEFINED_BITS: u32 = (1 << ChannelPosition::ALL.len()) - 1;

    /// Creates a mask from `dwChannelMask`-style bits. Bits without a
    /// [`ChannelPosition`] (including `SPEAKER_ALL`, bit 31) are dropped.
    #[must_use]
    pub const fn from_raw(bits: u32) -> Self {
        Self(bits & Self::DEFINED_BITS)
    }

    /// Creates a mask from a list of positions in any order, or returns `None`
    /// if a position appears twice.
    #[must_use]
    pub fn from_positions(positions: &[ChannelPosition]) -> Option<Self> {
        let mut bits = 0;
        for pos in positions {
            if bits & pos.bit() != 0 {
                return None;
            }
            bits |= pos.bit();
        }
        Some(Self(bits))
    }

    /// The mask bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Number of positions in the mask.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// Returns `true` if the mask has no positions.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if the mask contains `pos`.
    #[must_use]
    pub const fn contains(self, pos: ChannelPosition) -> bool {
        self.0 & pos.bit() != 0
    }

    /// Returns the mask unchanged if it fits in `channels` planes, otherwise
    /// [`ChannelMask::EMPTY`].
    ///
    /// A mask with more positions than channels is malformed; the channel
    /// count stays authoritative and the planes lose their positions.
    #[must_use]
    pub const fn fit(self, channels: u8) -> Self {
        if self.count() <= channels as u32 {
            self
        } else {
            Self::EMPTY
        }
    }

    /// Index of the plane that carries `pos`, or `None` if the mask does not
    /// contain it. The index is the number of mask bits below the position's
    /// bit.
    #[must_use]
    pub const fn plane_index(self, pos: ChannelPosition) -> Option<usize> {
        if self.contains(pos) {
            Some((self.0 & (pos.bit() - 1)).count_ones() as usize)
        } else {
            None
        }
    }

    /// Positions of the mask in plane order.
    pub fn positions(self) -> impl Iterator<Item = ChannelPosition> {
        ChannelPosition::ALL
            .into_iter()
            .filter(move |&pos| self.contains(pos))
    }
}

use ChannelPosition::{
    BackCenter as BC, BackLeft as BL, BackRight as BR, FrontCenter as FC, FrontLeft as FL,
    FrontRight as FR, Lfe1 as LFE, SideLeft as SL, SideRight as SR,
};

/// Vorbis I channel order for 1 to 8 channels (Vorbis I specification,
/// section 4.3.9), also used by Opus channel mapping family 1.
const VORBIS_ORDER: [&[ChannelPosition]; 8] = [
    &[FC],
    &[FL, FR],
    &[FL, FC, FR],
    &[FL, FR, BL, BR],
    &[FL, FC, FR, BL, BR],
    &[FL, FC, FR, BL, BR, LFE],
    &[FL, FC, FR, SL, SR, BC, LFE],
    &[FL, FC, FR, SL, SR, BL, BR, LFE],
];

/// Speaker positions of Vorbis channels in stream order, or `None` for a
/// channel count outside `1..=8`, where the order is application-defined.
///
/// ```
/// use akousma_core::channel::{vorbis_order, ChannelMask};
///
/// let order = vorbis_order(6).expect("5.1 is defined");
/// let mask = ChannelMask::from_positions(order).expect("no duplicates");
/// let planes: Vec<_> = order.iter().map(|&p| mask.plane_index(p)).collect();
/// assert_eq!(planes, [Some(0), Some(2), Some(1), Some(4), Some(5), Some(3)]);
/// ```
#[must_use]
pub fn vorbis_order(channels: u8) -> Option<&'static [ChannelPosition]> {
    usize::from(channels)
        .checked_sub(1)
        .and_then(|i| VORBIS_ORDER.get(i).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_mask_popcnt_and_reorder() {
        // Bit numbers from the WAVEFORMATEXTENSIBLE speaker list.
        assert_eq!(ChannelPosition::FrontLeft.bit(), 0x1);
        assert_eq!(ChannelPosition::Lfe1.bit(), 0x8);
        assert_eq!(ChannelPosition::BackCenter.bit(), 0x100);
        assert_eq!(ChannelPosition::SideRight.bit(), 0x400);
        assert_eq!(ChannelPosition::TopBackRight.bit(), 0x2_0000);
        assert_eq!(ChannelMask::DEFINED_BITS, 0x3_FFFF);
        assert_eq!(ChannelMask::from_raw(0x8000_0003), ChannelMask::STEREO);
        assert_eq!(ChannelMask::MONO.bits(), 0x4);

        // 7.1 with side channels: FL FR FC LFE BL BR SL SR = 0x63F.
        let mask = ChannelMask::from_raw(0x63F);
        let expected = [
            (FL, 0),
            (FR, 1),
            (FC, 2),
            (LFE, 3),
            (BL, 4),
            (BR, 5),
            (SL, 6),
            (SR, 7),
        ];
        for (pos, idx) in expected {
            assert_eq!(mask.plane_index(pos), Some(idx), "{pos:?}");
        }
        assert_eq!(mask.plane_index(BC), None);
        assert_eq!(
            mask.positions().collect::<Vec<_>>(),
            [FL, FR, FC, LFE, BL, BR, SL, SR]
        );

        // Microsoft example: three channels with FLC | FRC; the third plane
        // has no position.
        let partial = ChannelMask::from_raw(0xC0).fit(3);
        assert_eq!(partial.count(), 2);
        assert_eq!(
            partial.plane_index(ChannelPosition::FrontLeftCenter),
            Some(0)
        );
        assert_eq!(
            partial.plane_index(ChannelPosition::FrontRightCenter),
            Some(1)
        );
        assert_eq!(partial.plane_index(FL), None);

        // More positions than channels: the mask is dropped.
        assert_eq!(ChannelMask::from_raw(0x3F).fit(2), ChannelMask::EMPTY);
        assert_eq!(ChannelMask::from_raw(0x3F).fit(6).bits(), 0x3F);
        assert_eq!(ChannelMask::from_raw(0x3_FFFF).fit(u8::MAX).count(), 18);
        assert_eq!(ChannelMask::EMPTY.fit(0), ChannelMask::EMPTY);
        assert_eq!(ChannelMask::MONO.fit(0), ChannelMask::EMPTY);

        // An empty mask labels nothing.
        for pos in ChannelPosition::ALL {
            assert_eq!(ChannelMask::EMPTY.plane_index(pos), None);
        }
        assert!(ChannelMask::EMPTY.is_empty());
        assert_eq!(ChannelMask::EMPTY.positions().count(), 0);

        // Highest bit in a full mask.
        let full = ChannelMask::from_raw(u32::MAX);
        assert_eq!(full.plane_index(ChannelPosition::TopBackRight), Some(17));

        assert_eq!(
            ChannelMask::from_positions(&[FR, FL]),
            Some(ChannelMask::STEREO)
        );
        assert_eq!(ChannelMask::from_positions(&[FL, FC, FL]), None);
        assert_eq!(ChannelMask::from_positions(&[]), Some(ChannelMask::EMPTY));
    }

    #[test]
    fn vorbis_order_to_canonical_planes() {
        // Vorbis I section 4.3.9; canonical plane of each stream channel.
        let cases: [(u8, &[usize]); 8] = [
            (1, &[0]),
            (2, &[0, 1]),
            (3, &[0, 2, 1]),
            (4, &[0, 1, 2, 3]),
            (5, &[0, 2, 1, 3, 4]),
            (6, &[0, 2, 1, 4, 5, 3]),
            (7, &[0, 2, 1, 5, 6, 4, 3]),
            (8, &[0, 2, 1, 6, 7, 4, 5, 3]),
        ];
        for (channels, planes) in cases {
            let order = vorbis_order(channels).unwrap();
            assert_eq!(order.len(), usize::from(channels));
            let mask = ChannelMask::from_positions(order).unwrap();
            let got: Vec<usize> = order
                .iter()
                .map(|&p| mask.plane_index(p).unwrap())
                .collect();
            assert_eq!(got, planes, "{channels} channels");
        }
        assert_eq!(vorbis_order(0), None);
        assert_eq!(vorbis_order(9), None);
        assert_eq!(vorbis_order(u8::MAX), None);
        assert_eq!(
            ChannelMask::from_positions(vorbis_order(6).unwrap())
                .unwrap()
                .bits(),
            0x3F
        );
    }
}
