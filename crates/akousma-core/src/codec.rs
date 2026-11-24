//! Codec identifiers, stream parameters, tracks, compressed packets, the
//! [`Decoder`] trait and the [`CodecRegistry`].

use crate::audio::AudioBuf;
use crate::channel::ChannelMask;
use crate::checksum::IntegrityStatus;
use crate::error::{Error, ErrorKind};
use crate::options::DecodeOptions;
use crate::units::TimeBase;

/// Identifier of an audio coding format.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodecId {
    /// PCM, unsigned 8-bit.
    PcmU8,
    /// PCM, signed 16-bit little-endian.
    PcmS16Le,
    /// PCM, signed 16-bit big-endian.
    PcmS16Be,
    /// PCM, signed 24-bit little-endian.
    PcmS24Le,
    /// PCM, signed 24-bit big-endian.
    PcmS24Be,
    /// PCM, signed 32-bit little-endian.
    PcmS32Le,
    /// PCM, signed 32-bit big-endian.
    PcmS32Be,
    /// PCM, signed 64-bit little-endian.
    PcmS64Le,
    /// PCM, signed 64-bit big-endian.
    PcmS64Be,
    /// PCM, IEEE 754 32-bit float little-endian.
    PcmF32Le,
    /// PCM, IEEE 754 32-bit float big-endian.
    PcmF32Be,
    /// PCM, IEEE 754 64-bit float little-endian.
    PcmF64Le,
    /// PCM, IEEE 754 64-bit float big-endian.
    PcmF64Be,
    /// ITU-T G.711 A-law.
    G711Alaw,
    /// ITU-T G.711 mu-law.
    G711Mulaw,
    /// Microsoft ADPCM.
    AdpcmMs,
    /// IMA ADPCM as stored in WAV files.
    AdpcmImaWav,
    /// Free Lossless Audio Codec.
    Flac,
    /// MPEG-1/2 Audio Layer I.
    Mp1,
    /// MPEG-1/2 Audio Layer II.
    Mp2,
    /// MPEG-1/2 Audio Layer III.
    Mp3,
    /// MPEG-4 Advanced Audio Coding.
    Aac,
    /// Apple Lossless Audio Codec.
    Alac,
    /// Vorbis.
    Vorbis,
    /// Opus.
    Opus,
    /// Quite OK Audio.
    Qoa,
    /// WavPack.
    WavPack,
}

/// Parameters used to create a decoder for one track.
///
/// The struct is not non-exhaustive, unlike the option structs: it is part of
/// the [`Decoder`] contract, and a decoder implementation may build one to
/// report the parameters of its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecParams {
    /// Coding format.
    pub codec: CodecId,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u8,
    /// Speaker positions of the first channels.
    pub channel_mask: ChannelMask,
    /// Significant bits per decoded sample, `0` if the format has no fixed
    /// width (lossy codecs).
    pub bits_per_sample: u8,
    /// Bytes per interleaved PCM frame or ADPCM block; `0` when the format
    /// does not use a fixed unit.
    pub block_align: u16,
    /// Frames to drop at the start of the stream (encoder delay).
    pub delay: u32,
    /// Frames to drop at the end of the stream (encoder padding).
    pub padding: u32,
    /// Number of frames in the stream after trimming, if known.
    pub total_frames: Option<u64>,
    /// Codec-specific setup data (for example an AAC `AudioSpecificConfig` or
    /// a FLAC `STREAMINFO` block).
    pub extra_data: Vec<u8>,
}

/// Container-level hints about a track, combined with `|`.
///
/// ```
/// use akousma_core::codec::TrackFlags;
///
/// let flags = TrackFlags::ENABLED | TrackFlags::DEFAULT;
/// assert!(flags.contains(TrackFlags::DEFAULT));
/// assert!(!flags.contains(TrackFlags::FORCED));
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrackFlags(u8);

impl TrackFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// The container marks the track as enabled for playback.
    pub const ENABLED: Self = Self(1);
    /// The container marks the track as the default choice among tracks of
    /// its kind.
    pub const DEFAULT: Self = Self(1 << 1);
    /// The container marks the track as one the player must play.
    pub const FORCED: Self = Self(1 << 2);

    /// The flag bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` if every flag of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for TrackFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for TrackFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One elementary stream of a container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// Identifier, unique within the container.
    pub id: u32,
    /// Decoder parameters.
    pub params: CodecParams,
    /// Unit of packet timestamps.
    pub time_base: TimeBase,
    /// Timestamp of the first packet.
    pub start_ts: u64,
    /// Language tag as stored by the container (for example `"eng"` or
    /// `"en-US"`), if any.
    pub language: Option<String>,
    /// Container hints.
    pub flags: TrackFlags,
}

/// A compressed packet. Demuxers refill the same packet, so the capacity of
/// `data` is reused for the whole track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Packet {
    /// Identifier of the track the packet belongs to.
    pub track_id: u32,
    /// Timestamp of the first frame, in the track's time base.
    pub pts: u64,
    /// Duration in the track's time base.
    pub duration: u32,
    /// Decoded frames to drop from the start of this packet.
    pub trim_start: u32,
    /// Decoded frames to drop from the end of this packet.
    pub trim_end: u32,
    /// Compressed bytes.
    pub data: Vec<u8>,
}

/// A decoder of one codec: turns compressed packets into audio.
pub trait Decoder: Send {
    /// Parameters the decoder was created with.
    fn params(&self) -> &CodecParams;

    /// Decodes one whole packet into `out`, replacing its previous content.
    ///
    /// When [`DecodeOptions::apply_trim`] is set, the decoder also drops the
    /// [`Packet::trim_start`] and [`Packet::trim_end`] frames.
    ///
    /// # Errors
    ///
    /// Any error of the codec: [`ErrorKind::CorruptFrame`] for a packet that
    /// cannot be decoded, [`ErrorKind::ResetRequired`] if the stream changed
    /// its parameters. No decoder in 0.1.0 returns
    /// [`ErrorKind::ResetRequired`].
    fn decode(&mut self, packet: &Packet, out: &mut AudioBuf) -> Result<(), Error>;

    /// Forgets the state carried from packet to packet (for example the
    /// overlap of a transform codec), as needed after a seek.
    fn reset(&mut self);

    /// Ends decoding and reports whether the decoded audio matched the
    /// checksum of the whole stream, if the stream has one.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::ChecksumMismatch`] if the stream checksum does not match
    /// the decoded audio. No decoder in 0.1.0 returns this.
    fn finalize(&mut self) -> Result<IntegrityStatus, Error> {
        Ok(IntegrityStatus::NotPresent)
    }
}

/// Creates a decoder for the given parameters.
pub type DecoderFactory = fn(&CodecParams, &DecodeOptions) -> Result<Box<dyn Decoder>, Error>;

/// A decoder implementation that can be registered in a [`CodecRegistry`].
#[derive(Copy, Clone, Debug)]
pub struct CodecDescriptor {
    /// Short name of the implementation, for example `"pcm"`.
    pub name: &'static str,
    /// Codecs the implementation decodes.
    pub codecs: &'static [CodecId],
    /// Constructor.
    pub make: DecoderFactory,
}

/// A set of decoder implementations, looked up by [`CodecId`].
///
/// A descriptor registered later takes precedence over earlier ones for the
/// codecs it shares with them, so an application can replace a built-in
/// decoder.
///
/// ```
/// use akousma_core::codec::{CodecDescriptor, CodecId, CodecParams, CodecRegistry, Decoder};
/// use akousma_core::{DecodeOptions, Error, ErrorKind};
///
/// fn unavailable(_: &CodecParams, _: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
///     Err(ErrorKind::UnsupportedFeature.into())
/// }
///
/// let mut registry = CodecRegistry::new();
/// registry.register(CodecDescriptor { name: "stub", codecs: &[CodecId::Qoa], make: unavailable });
/// assert_eq!(registry.get(CodecId::Qoa).map(|d| d.name), Some("stub"));
/// assert!(registry.get(CodecId::Flac).is_none());
/// ```
#[derive(Clone, Debug, Default)]
pub struct CodecRegistry {
    descriptors: Vec<CodecDescriptor>,
}

impl CodecRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            descriptors: Vec::new(),
        }
    }

    /// Adds a decoder implementation.
    pub fn register(&mut self, descriptor: CodecDescriptor) {
        self.descriptors.push(descriptor);
    }

    /// The most recently registered implementation of `codec`, if any.
    #[must_use]
    pub fn get(&self, codec: CodecId) -> Option<&CodecDescriptor> {
        self.descriptors
            .iter()
            .rev()
            .find(|descriptor| descriptor.codecs.contains(&codec))
    }

    /// Registered implementations, in registration order.
    #[must_use]
    pub fn descriptors(&self) -> &[CodecDescriptor] {
        &self.descriptors
    }

    /// Creates a decoder for `params` with the implementation returned by
    /// [`CodecRegistry::get`].
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedCodec`] if no implementation decodes
    /// `params.codec`, otherwise the errors of the constructor.
    pub fn make(
        &self,
        params: &CodecParams,
        options: &DecodeOptions,
    ) -> Result<Box<dyn Decoder>, Error> {
        let descriptor = self
            .get(params.codec)
            .ok_or(Error::from(ErrorKind::UnsupportedCodec))?;
        (descriptor.make)(params, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioPlanes;

    /// Decoder that outputs one frame per packet byte, each frame the byte
    /// value, and records its constructor in the sample rate.
    struct ByteDecoder {
        params: CodecParams,
    }

    impl Decoder for ByteDecoder {
        fn params(&self) -> &CodecParams {
            &self.params
        }

        fn decode(&mut self, packet: &Packet, out: &mut AudioBuf) -> Result<(), Error> {
            out.reset_window(packet.data.len())?;
            let plane = out.plane_mut::<i16>(0).ok_or(ErrorKind::InvalidData)?;
            for (sample, &byte) in plane.iter_mut().zip(&packet.data) {
                *sample = i16::from(byte);
            }
            Ok(())
        }

        fn reset(&mut self) {}
    }

    fn params(codec: CodecId, sample_rate: u32) -> CodecParams {
        CodecParams {
            codec,
            sample_rate,
            channels: 1,
            channel_mask: ChannelMask::MONO,
            bits_per_sample: 8,
            block_align: 1,
            delay: 0,
            padding: 0,
            total_frames: None,
            extra_data: Vec::new(),
        }
    }

    fn first(params: &CodecParams, _: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
        let mut params = params.clone();
        params.sample_rate = 1;
        Ok(Box::new(ByteDecoder { params }))
    }

    fn second(params: &CodecParams, _: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
        let mut params = params.clone();
        params.sample_rate = 2;
        Ok(Box::new(ByteDecoder { params }))
    }

    #[test]
    fn codec_registry_lookup_and_override() {
        let mut registry = CodecRegistry::new();
        let options = DecodeOptions::new();
        let err = registry
            .make(&params(CodecId::PcmU8, 8000), &options)
            .err()
            .map(|err| err.kind);
        assert_eq!(err, Some(ErrorKind::UnsupportedCodec));

        registry.register(CodecDescriptor {
            name: "first",
            codecs: &[CodecId::PcmU8, CodecId::G711Alaw],
            make: first,
        });
        registry.register(CodecDescriptor {
            name: "second",
            codecs: &[CodecId::PcmU8],
            make: second,
        });
        assert_eq!(registry.descriptors().len(), 2);
        assert_eq!(registry.get(CodecId::PcmU8).map(|d| d.name), Some("second"));
        assert_eq!(
            registry.get(CodecId::G711Alaw).map(|d| d.name),
            Some("first")
        );

        let mut decoder = registry
            .make(&params(CodecId::PcmU8, 8000), &options)
            .unwrap();
        assert_eq!(decoder.params().sample_rate, 2);
        let mut out = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new()]),
            8000,
            ChannelMask::MONO,
            8,
        )
        .unwrap();
        let packet = Packet {
            data: vec![1, 2, 3],
            ..Packet::default()
        };
        decoder.decode(&packet, &mut out).unwrap();
        assert_eq!(out.plane::<i16>(0), Some(&[1, 2, 3][..]));
        decoder.reset();
        assert_eq!(decoder.finalize(), Ok(IntegrityStatus::NotPresent));
    }

    #[test]
    fn track_flags_combine() {
        let mut flags = TrackFlags::NONE;
        assert!(flags.contains(TrackFlags::NONE));
        assert!(!flags.contains(TrackFlags::ENABLED));
        flags |= TrackFlags::FORCED;
        flags |= TrackFlags::ENABLED;
        assert_eq!(flags.bits(), 0b101);
        assert!(flags.contains(TrackFlags::ENABLED | TrackFlags::FORCED));
        assert!(!flags.contains(TrackFlags::ENABLED | TrackFlags::DEFAULT));
    }
}
