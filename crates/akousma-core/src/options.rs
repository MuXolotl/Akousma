//! Explicit limits and switches for demuxers and decoders.
//!
//! Every field is public and every default is documented; there are no hidden
//! presets. The `const fn` setters allow building options in `const` items.

/// Upper bounds on sizes declared by untrusted streams.
///
/// A declared size above a limit makes the reader fail with
/// [`ErrorKind::LimitExceeded`](crate::ErrorKind::LimitExceeded) before any
/// memory is reserved.
///
/// ```
/// use akousma_core::FormatLimits;
///
/// const LIMITS: FormatLimits = FormatLimits::new().with_max_packet_bytes(4 << 20);
/// assert_eq!(LIMITS.max_packet_bytes, 4 * 1024 * 1024);
/// assert_eq!(LIMITS.max_tracks, FormatLimits::default().max_tracks);
/// ```
///
/// The struct is non-exhaustive: a limit may be added in a later release, so
/// it is built with [`FormatLimits::new`] and the `with_` setters rather than
/// a struct literal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormatLimits {
    /// Largest single packet or chunk payload, in bytes. Default: 16 MiB.
    ///
    /// Packets are not cut inside a frame or a block: a demuxer whose
    /// smallest unit does not fit (including with a limit of 0) fails with
    /// [`ErrorKind::LimitExceeded`](crate::ErrorKind::LimitExceeded).
    pub max_packet_bytes: usize,
    /// Total stored size of the metadata read from one source, in bytes:
    /// every tag block in front of, inside and after the container (cover
    /// art and chapters included). Default: 32 MiB.
    ///
    /// It is the byte limit of the [`MetadataBudget`](crate::meta::MetadataBudget)
    /// shared by these blocks. A block that is read and does not fit in what
    /// is left fails with
    /// [`ErrorKind::LimitExceeded`](crate::ErrorKind::LimitExceeded); it is
    /// not skipped. Without [`DemuxOptions::read_metadata`] no block is
    /// read into memory and this limit is not used: the tags checked to find
    /// the audio fall under [`FormatLimits::max_tag_scan_bytes`]. It bounds
    /// the input, not the memory of the parsed result; see
    /// [`MetadataBudget`](crate::meta::MetadataBudget).
    pub max_metadata_bytes: usize,
    /// Largest number of tracks accepted from one container. Default: 64.
    pub max_tracks: usize,
    /// Largest number of bytes [`Probe::open`](crate::Probe::open) reads
    /// from the source to detect the format and the length of its container.
    /// Default: 1 MiB.
    ///
    /// It counts the bytes read, not the bytes skipped by seeking: the
    /// window given to the format signatures (at most
    /// [`PROBE_WINDOW`](crate::probe::PROBE_WINDOW) bytes), then the reads of
    /// [`FormatDescriptor::extent`](crate::probe::FormatDescriptor::extent)
    /// outside that window. The tags in front of the container are not
    /// counted: they fall under [`FormatLimits::max_metadata_bytes`]. A limit
    /// too small to recognize the format or to find the length of its
    /// container, `0` included, fails with
    /// [`ErrorKind::LimitExceeded`](crate::ErrorKind::LimitExceeded).
    pub max_probe_bytes: usize,
    /// Largest number of bytes a tag scanner reads from one source to check
    /// the APEv2 and Lyrics3 v2.00 tags around the audio when
    /// [`DemuxOptions::read_metadata`] is `false`. Default: 32 MiB, as
    /// [`FormatLimits::max_metadata_bytes`].
    ///
    /// Whether such a tag ends the audio depends on its items, so it is
    /// checked even when it is not kept: by the same rules as with
    /// `read_metadata`, so that the audio is the same with either value. The
    /// check reads through a fixed buffer and seeks over the bytes it does
    /// not need (pictures and other binary values, the text of Lyrics3
    /// fields); it counts the bytes it reads, read-ahead included, which
    /// never exceed the stored size of the tags. The reads of fixed size
    /// that every scan makes (headers and footers, ID3v1 and `TAG+` tags,
    /// the search for a Lyrics3 v1.00 tag), a few KiB per source, are not
    /// counted.
    ///
    /// Running out fails with
    /// [`ErrorKind::LimitExceeded`](crate::ErrorKind::LimitExceeded) at the
    /// start of the tag being checked: the tag is neither left in the audio
    /// nor taken unchecked. With `read_metadata` this limit is not used.
    /// When it is not smaller than [`FormatLimits::max_metadata_bytes`], a
    /// tag scan that succeeds with `read_metadata` also succeeds without it
    /// and finds the same audio range.
    pub max_tag_scan_bytes: usize,
}

impl FormatLimits {
    /// Default for [`FormatLimits::max_packet_bytes`]: 16 MiB.
    pub const DEFAULT_MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
    /// Default for [`FormatLimits::max_metadata_bytes`]: 32 MiB.
    pub const DEFAULT_MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
    /// Default for [`FormatLimits::max_tracks`].
    pub const DEFAULT_MAX_TRACKS: usize = 64;
    /// Default for [`FormatLimits::max_probe_bytes`]: 1 MiB.
    pub const DEFAULT_MAX_PROBE_BYTES: usize = 1024 * 1024;
    /// Default for [`FormatLimits::max_tag_scan_bytes`]: 32 MiB.
    pub const DEFAULT_MAX_TAG_SCAN_BYTES: usize = 32 * 1024 * 1024;

    /// Returns the default limits.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_packet_bytes: Self::DEFAULT_MAX_PACKET_BYTES,
            max_metadata_bytes: Self::DEFAULT_MAX_METADATA_BYTES,
            max_tracks: Self::DEFAULT_MAX_TRACKS,
            max_probe_bytes: Self::DEFAULT_MAX_PROBE_BYTES,
            max_tag_scan_bytes: Self::DEFAULT_MAX_TAG_SCAN_BYTES,
        }
    }

    /// Sets [`FormatLimits::max_packet_bytes`].
    #[must_use]
    pub const fn with_max_packet_bytes(mut self, bytes: usize) -> Self {
        self.max_packet_bytes = bytes;
        self
    }

    /// Sets [`FormatLimits::max_metadata_bytes`].
    #[must_use]
    pub const fn with_max_metadata_bytes(mut self, bytes: usize) -> Self {
        self.max_metadata_bytes = bytes;
        self
    }

    /// Sets [`FormatLimits::max_tracks`].
    #[must_use]
    pub const fn with_max_tracks(mut self, tracks: usize) -> Self {
        self.max_tracks = tracks;
        self
    }

    /// Sets [`FormatLimits::max_probe_bytes`].
    #[must_use]
    pub const fn with_max_probe_bytes(mut self, bytes: usize) -> Self {
        self.max_probe_bytes = bytes;
        self
    }

    /// Sets [`FormatLimits::max_tag_scan_bytes`].
    #[must_use]
    pub const fn with_max_tag_scan_bytes(mut self, bytes: usize) -> Self {
        self.max_tag_scan_bytes = bytes;
        self
    }
}

impl Default for FormatLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for opening and reading a container.
///
/// ```
/// use akousma_core::DemuxOptions;
///
/// let options = DemuxOptions::new().with_read_metadata(false);
/// assert!(!options.read_metadata);
/// assert!(options.gapless);
/// ```
///
/// The struct is non-exhaustive: an option may be added in a later release,
/// so it is built with [`DemuxOptions::new`] and the `with_` setters rather
/// than a struct literal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DemuxOptions {
    /// Size limits applied to untrusted stream fields. Default:
    /// [`FormatLimits::new`].
    pub limits: FormatLimits,
    /// Parse tags, cover art and chapters. Default: `true`.
    ///
    /// When `false`, no revision or chapter is returned and metadata blocks
    /// are skipped without being read into memory, except that the blocks
    /// whose extent depends on their contents are checked: a tag scanner
    /// checks the APEv2 and Lyrics3 v2.00 tags around the audio by the same
    /// rules as with `true`, through a fixed buffer and within
    /// [`FormatLimits::max_tag_scan_bytes`], so that the audio is the same
    /// with either value. [`FormatLimits::max_metadata_bytes`] is then not
    /// used. A demuxer may also stop reading its container where the audio
    /// starts, without checking what follows; its documentation says so.
    pub read_metadata: bool,
    /// Mark encoder delay and padding on packets (`trim_start` / `trim_end`)
    /// from container or codec gapless information. Default: `true`.
    pub gapless: bool,
}

impl DemuxOptions {
    /// Returns the default options.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: FormatLimits::new(),
            read_metadata: true,
            gapless: true,
        }
    }

    /// Sets [`DemuxOptions::limits`].
    #[must_use]
    pub const fn with_limits(mut self, limits: FormatLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets [`DemuxOptions::read_metadata`].
    #[must_use]
    pub const fn with_read_metadata(mut self, read_metadata: bool) -> Self {
        self.read_metadata = read_metadata;
        self
    }

    /// Sets [`DemuxOptions::gapless`].
    #[must_use]
    pub const fn with_gapless(mut self, gapless: bool) -> Self {
        self.gapless = gapless;
        self
    }
}

impl Default for DemuxOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for decoding packets.
///
/// ```
/// use akousma_core::DecodeOptions;
///
/// let options = DecodeOptions::new().with_verify_checksums(false);
/// assert!(!options.verify_checksums);
/// assert!(options.apply_trim);
/// ```
///
/// The struct is non-exhaustive: an option may be added in a later release,
/// so it is built with [`DecodeOptions::new`] and the `with_` setters rather
/// than a struct literal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Verify per-frame checksums and whole-stream checksums where the format
    /// stores them. Default: `true`.
    pub verify_checksums: bool,
    /// Remove the `trim_start` / `trim_end` frames marked on packets from the
    /// decoded output. Default: `true`.
    pub apply_trim: bool,
}

impl DecodeOptions {
    /// Returns the default options.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            verify_checksums: true,
            apply_trim: true,
        }
    }

    /// Sets [`DecodeOptions::verify_checksums`].
    #[must_use]
    pub const fn with_verify_checksums(mut self, verify_checksums: bool) -> Self {
        self.verify_checksums = verify_checksums;
        self
    }

    /// Sets [`DecodeOptions::apply_trim`].
    #[must_use]
    pub const fn with_apply_trim(mut self, apply_trim: bool) -> Self {
        self.apply_trim = apply_trim;
        self
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self::new()
    }
}
