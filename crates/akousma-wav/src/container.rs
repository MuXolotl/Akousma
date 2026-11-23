//! The five members of the WAVE family and their chunk headers.
//!
//! | Container | Start of the stream | Chunk header | Chunk alignment |
//! | :--- | :--- | :--- | :--- |
//! | RIFF | `"RIFF"`, `u32` LE size, `"WAVE"` | identifier, `u32` LE size | 2 bytes |
//! | RIFX | `"RIFX"`, `u32` BE size, `"WAVE"` | identifier, `u32` BE size | 2 bytes |
//! | RF64 (EBU Tech 3306) | `"RF64"`, `u32` LE size, `"WAVE"` | identifier, `u32` LE size | 2 bytes |
//! | BW64 (ITU-R BS.2088) | `"BW64"`, `u32` LE size, `"WAVE"` | identifier, `u32` LE size | 2 bytes |
//! | Sony Wave64 | `riff` GUID, `u64` LE size, `wave` GUID | GUID, `u64` LE size including the 24-byte header | 8 bytes |
//!
//! Sizes are not padded: a chunk with an odd payload is followed by one pad
//! byte (RIFF family), a Wave64 chunk by up to 7. In RF64 and BW64 a 32-bit
//! size of `0xFFFF_FFFF` is replaced by the 64-bit size from the `ds64`
//! chunk.
//!
//! The size of the root chunk bounds the container. A RIFF or RIFX root
//! size of 0 or `0xFFFF_FFFF` (and an RF64 or BW64 root size that resolves
//! to 0) is left by writers that stream or stop early and declares no
//! bound: the container then runs to the end of the source.

use std::io::{Read, Seek, SeekFrom};

use akousma_core::io::ByteReader;
use akousma_core::probe::{Extent as ProbeExtent, ProbeReader};
use akousma_core::{Error, ErrorKind};

/// Member of the WAVE family.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WavContainer {
    /// Little-endian RIFF WAVE.
    Riff,
    /// Big-endian RIFX WAVE.
    Rifx,
    /// RF64 (EBU Tech 3306): RIFF with 64-bit sizes in a `ds64` chunk.
    Rf64,
    /// BW64 (ITU-R BS.2088): like RF64, without the sample count of `ds64`.
    Bw64,
    /// Sony Wave64: 16-byte GUID chunk identifiers and 64-bit sizes.
    Wave64,
}

impl WavContainer {
    /// Returns `true` for RIFX, whose numbers are big-endian.
    #[must_use]
    pub const fn is_big_endian(self) -> bool {
        matches!(self, Self::Rifx)
    }

    /// Returns `true` for RF64 and BW64, which need a `ds64` chunk.
    pub(crate) const fn has_ds64(self) -> bool {
        matches!(self, Self::Rf64 | Self::Bw64)
    }

    const fn header_len(self) -> u64 {
        match self {
            Self::Wave64 => W64_CHUNK_HEADER_LEN,
            _ => RIFF_CHUNK_HEADER_LEN,
        }
    }
}

/// Wave64 GUID of the root chunk (`riff`), as stored on disk.
pub(crate) const GUID_W64_RIFF: [u8; 16] = [
    0x72, 0x69, 0x66, 0x66, 0x2E, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00,
];
/// Wave64 GUID of a `list` chunk.
const GUID_W64_LIST: [u8; 16] = [
    0x6C, 0x69, 0x73, 0x74, 0x2F, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00,
];
/// Wave64 GUID of the form type `wave`.
pub(crate) const GUID_W64_WAVE: [u8; 16] = [
    0x77, 0x61, 0x76, 0x65, 0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
];
/// Last 12 bytes shared by the Wave64 GUIDs of `wave`, `fmt `, `fact`,
/// `data`, `levl`, `junk` and `bext`, whose first 4 bytes are the ASCII
/// name.
const W64_NAMED_SUFFIX: [u8; 12] = [
    0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
];

/// Identifier given to a Wave64 `list` chunk, whose sub-chunks have GUID
/// headers and must not reach the RIFF `LIST` parsers.
pub(crate) const W64_LIST_ID: [u8; 4] = *b"wlst";

/// Identifier given to Wave64 chunks with other GUIDs: skipped.
const W64_UNKNOWN_ID: [u8; 4] = [0; 4];

/// Length of the RIFF, RIFX, RF64 and BW64 stream header.
const RIFF_ROOT_LEN: u64 = 12;
/// Length of the Wave64 stream header: GUID, `u64` size, GUID.
pub(crate) const W64_ROOT_LEN: u64 = 40;
const RIFF_CHUNK_HEADER_LEN: u64 = 8;
const W64_CHUNK_HEADER_LEN: u64 = 24;

/// The stream header.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Root {
    pub container: WavContainer,
    /// Position of the stream header.
    pub base: u64,
    /// Position of the first chunk.
    pub body_start: u64,
    /// The root size field: the 32-bit size after the magic (RIFF family),
    /// or the 64-bit size including the header (Wave64).
    pub size: u64,
}

/// Smallest root size of the RIFF family: the form type `WAVE`.
const RIFF_MIN_ROOT_SIZE: u64 = 4;

impl Root {
    /// End of the container declared by the root size, or `None` if the
    /// root declares no bound. `ds64_riff_size` is the RIFF size of the
    /// `ds64` chunk of RF64 and BW64, once read.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidHeader`] for a root size that cannot hold the
    /// form type.
    pub(crate) fn declared_end(&self, ds64_riff_size: Option<u64>) -> Result<Option<u64>, Error> {
        let invalid = Error::at(ErrorKind::InvalidHeader, self.base);
        let len = match riff_extent(self.container, self.size, ds64_riff_size) {
            Extent::Bounded(len) => len,
            Extent::Unbounded => return Ok(None),
            Extent::Invalid => return Err(invalid),
        };
        self.base.checked_add(len).map(Some).ok_or(invalid)
    }
}

/// Length of a container from its root size.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Extent {
    /// The container is this many bytes long, from its first byte.
    Bounded(u64),
    /// The root declares no bound.
    Unbounded,
    /// The root size cannot hold the form type.
    Invalid,
}

/// Length of a container from its root size field and, for RF64 and BW64,
/// the RIFF size of the `ds64` chunk (`None` before it is read).
pub(crate) fn riff_extent(
    container: WavContainer,
    size: u64,
    ds64_riff_size: Option<u64>,
) -> Extent {
    let size = match container {
        // The size field includes the 40-byte header, checked when read.
        WavContainer::Wave64 => return Extent::Bounded(size),
        WavContainer::Riff | WavContainer::Rifx if size == 0 || size == u64::from(u32::MAX) => {
            return Extent::Unbounded;
        }
        WavContainer::Rf64 | WavContainer::Bw64 if size == u64::from(u32::MAX) => {
            match ds64_riff_size {
                Some(0) | None => return Extent::Unbounded,
                Some(size) => size,
            }
        }
        WavContainer::Rf64 | WavContainer::Bw64 if size == 0 => return Extent::Unbounded,
        _ => size,
    };
    if size < RIFF_MIN_ROOT_SIZE {
        return Extent::Invalid;
    }
    size.checked_add(RIFF_CHUNK_HEADER_LEN)
        .map_or(Extent::Invalid, Extent::Bounded)
}

/// Length of the container whose stream header starts the range of
/// `reader`, as its root size declares it; see [`riff_extent`].
///
/// For RF64 and BW64 with a root size of `0xFFFF_FFFF`, the `ds64` chunk is
/// found by following the chunk headers from the first one: 8 bytes are
/// read per header, and the chunks in between (`JUNK`, `PAD `, `FLLR` or any
/// other) are skipped by seeking, however long they are. The demuxer
/// accepts other chunks before `ds64` too, so they do not end the search;
/// only `fmt ` or `data` does.
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] if the range does not start with a WAVE
///   stream header, if the root size cannot hold the form type, if a `fmt `
///   or `data` chunk or the end of the range comes before `ds64`, if the
///   `ds64` chunk is shorter than its fixed part, or if the resolved size
///   overflows. The demuxer rejects such a stream with the same error.
/// - [`ErrorKind::UnexpectedEof`] if the header or the size field of `ds64`
///   is cut by the end of the range.
/// - [`ErrorKind::LimitExceeded`] if the reads leave the window and need
///   more than what is left of the probe limit.
/// - The errors of the source.
pub(crate) fn extent(reader: &mut ProbeReader<'_>) -> Result<ProbeExtent, Error> {
    let invalid = || Error::at(ErrorKind::InvalidHeader, 0);
    let mut head = [0u8; RIFF_ROOT_LEN as usize];
    reader.read_exact_at(0, &mut head)?;
    let container = match &head[..4] {
        b"RIFF" => WavContainer::Riff,
        b"RIFX" => WavContainer::Rifx,
        b"RF64" => WavContainer::Rf64,
        b"BW64" => WavContainer::Bw64,
        _ => {
            let mut root = [0u8; W64_ROOT_LEN as usize];
            reader.read_exact_at(0, &mut root)?;
            let (riff, rest) = root.split_at(16);
            let (size, wave) = rest.split_at(8);
            let size = u64::from_le_bytes([
                size[0], size[1], size[2], size[3], size[4], size[5], size[6], size[7],
            ]);
            if riff != GUID_W64_RIFF || wave != GUID_W64_WAVE || size < W64_ROOT_LEN {
                return Err(invalid());
            }
            return Ok(ProbeExtent::Bounded(size));
        }
    };
    if &head[8..12] != b"WAVE" {
        return Err(invalid());
    }
    let size_bytes = [head[4], head[5], head[6], head[7]];
    let size = u64::from(if container.is_big_endian() {
        u32::from_be_bytes(size_bytes)
    } else {
        u32::from_le_bytes(size_bytes)
    });
    let ds64_riff_size = if container.has_ds64() && size == u64::from(u32::MAX) {
        Some(ds64_riff_size(reader)?)
    } else {
        None
    };
    match riff_extent(container, size, ds64_riff_size) {
        Extent::Bounded(len) => Ok(ProbeExtent::Bounded(len)),
        Extent::Unbounded => Ok(ProbeExtent::Unbounded),
        Extent::Invalid => Err(invalid()),
    }
}

/// RIFF size of the `ds64` chunk of the RF64 or BW64 stream that starts the
/// range of `reader`; see [`extent`].
fn ds64_riff_size(reader: &mut ProbeReader<'_>) -> Result<u64, Error> {
    let mut pos = RIFF_ROOT_LEN;
    loop {
        if reader.len().saturating_sub(pos) < RIFF_CHUNK_HEADER_LEN {
            return Err(Error::at(ErrorKind::InvalidHeader, 0));
        }
        let mut header = [0u8; RIFF_CHUNK_HEADER_LEN as usize];
        reader.read_exact_at(pos, &mut header)?;
        let size = u64::from(u32::from_le_bytes([
            header[4], header[5], header[6], header[7],
        ]));
        let start = pos + RIFF_CHUNK_HEADER_LEN;
        match &header[..4] {
            b"ds64" => {
                if size < DS64_MIN_LEN {
                    return Err(Error::at(ErrorKind::InvalidHeader, start));
                }
                let mut field = [0u8; 8];
                reader.read_exact_at(start, &mut field)?;
                return Ok(u64::from_le_bytes(field));
            }
            b"fmt " | b"data" => return Err(Error::at(ErrorKind::InvalidHeader, start)),
            _ => pos = next_chunk(WavContainer::Rf64, start, size)?,
        }
    }
}

/// Reads the stream header at `base`.
pub(crate) fn read_root<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    base: u64,
) -> Result<Root, Error> {
    reader.seek(SeekFrom::Start(base))?;
    let head = reader.read_array::<12>()?;
    let (magic, rest) = head.split_at(4);
    let container = match magic {
        b"RIFF" => WavContainer::Riff,
        b"RIFX" => WavContainer::Rifx,
        b"RF64" => WavContainer::Rf64,
        b"BW64" => WavContainer::Bw64,
        _ if head.starts_with(&GUID_W64_RIFF[..4]) => {
            reader.seek(SeekFrom::Start(base))?;
            let riff = reader.read_array::<16>()?;
            let size = reader.read_u64_le()?;
            let wave = reader.read_array::<16>()?;
            if riff != GUID_W64_RIFF || wave != GUID_W64_WAVE || size < W64_ROOT_LEN {
                return Err(Error::at(ErrorKind::InvalidHeader, base));
            }
            return Ok(Root {
                container: WavContainer::Wave64,
                base,
                body_start: base + W64_ROOT_LEN,
                size,
            });
        }
        _ => return Err(Error::at(ErrorKind::InvalidHeader, base)),
    };
    if &rest[4..] != b"WAVE" {
        return Err(Error::at(ErrorKind::InvalidHeader, base));
    }
    let size_bytes = [rest[0], rest[1], rest[2], rest[3]];
    let size = if container.is_big_endian() {
        u32::from_be_bytes(size_bytes)
    } else {
        u32::from_le_bytes(size_bytes)
    };
    Ok(Root {
        container,
        base,
        body_start: base + RIFF_ROOT_LEN,
        size: u64::from(size),
    })
}

/// A chunk header.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ChunkHeader {
    /// Four-character identifier; for Wave64, the name in the GUID.
    pub id: [u8; 4],
    /// The 32-bit size field (RIFF family), before any `ds64` replacement.
    pub size32: Option<u32>,
    /// Payload size (Wave64: the size field minus the header).
    pub size: u64,
    /// Position of the payload.
    pub start: u64,
}

/// Reads the chunk header at `pos`. Returns `None` if fewer bytes than a
/// header are left before `end`: the normal end of the chunk list, also
/// when the pad byte of the last chunk is missing.
pub(crate) fn read_chunk_header<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    container: WavContainer,
    pos: u64,
    end: u64,
) -> Result<Option<ChunkHeader>, Error> {
    let header_len = container.header_len();
    if end.saturating_sub(pos) < header_len {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(pos))?;
    let start = pos + header_len;
    if container == WavContainer::Wave64 {
        let guid = reader.read_array::<16>()?;
        let total = reader.read_u64_le()?;
        let size = total
            .checked_sub(W64_CHUNK_HEADER_LEN)
            .ok_or(Error::at(ErrorKind::InvalidHeader, pos))?;
        let (name, suffix) = guid.split_at(4);
        let id = if suffix == W64_NAMED_SUFFIX {
            [name[0], name[1], name[2], name[3]]
        } else if guid == GUID_W64_LIST {
            W64_LIST_ID
        } else {
            W64_UNKNOWN_ID
        };
        return Ok(Some(ChunkHeader {
            id,
            size32: None,
            size,
            start,
        }));
    }
    let id = reader.read_array::<4>()?;
    let size32 = if container.is_big_endian() {
        reader.read_u32_be()?
    } else {
        reader.read_u32_le()?
    };
    Ok(Some(ChunkHeader {
        id,
        size32: Some(size32),
        size: u64::from(size32),
        start,
    }))
}

/// Position after a chunk of `size` payload bytes starting at `start`,
/// including its alignment padding.
pub(crate) fn next_chunk(container: WavContainer, start: u64, size: u64) -> Result<u64, Error> {
    let pad = match container {
        WavContainer::Wave64 => size.wrapping_neg() & 7,
        _ => size & 1,
    };
    start
        .checked_add(size)
        .and_then(|end| end.checked_add(pad))
        .ok_or(Error::at(ErrorKind::InvalidHeader, start))
}

/// Contents of a `ds64` chunk.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ds64 {
    /// Root size replacing a 32-bit root size of `0xFFFF_FFFF`.
    pub riff_size: u64,
    pub data_size: u64,
    /// RF64 sample count; zero in BW64, where the field is unused.
    pub sample_count: u64,
    /// `ChunkSize64` entries for other chunks larger than 4 GiB.
    pub table: Vec<([u8; 4], u64)>,
}

/// Fixed part of a `ds64` payload: three `u64` and the table length.
pub(crate) const DS64_MIN_LEN: u64 = 28;
/// Size of one `ChunkSize64` entry: identifier and `u64` size.
const DS64_ENTRY_LEN: u64 = 12;
/// Largest number of `ChunkSize64` entries read; a file has only a few
/// chunks over 4 GiB, the rest of a longer table is skipped.
const DS64_MAX_ENTRIES: u32 = 256;

/// Reads the `ds64` payload of `size` bytes at the reader's position.
pub(crate) fn read_ds64<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    container: WavContainer,
    size: u64,
) -> Result<Ds64, Error> {
    let at = reader.position();
    if size < DS64_MIN_LEN {
        return Err(Error::at(ErrorKind::InvalidHeader, at));
    }
    let riff_size = reader.read_u64_le()?;
    let data_size = reader.read_u64_le()?;
    let sample_count = reader.read_u64_le()?;
    let entries = reader.read_u32_le()?;
    if DS64_MIN_LEN + u64::from(entries) * DS64_ENTRY_LEN > size {
        return Err(Error::at(ErrorKind::InvalidHeader, at));
    }
    let mut table = Vec::with_capacity(entries.min(DS64_MAX_ENTRIES) as usize);
    for _ in 0..entries.min(DS64_MAX_ENTRIES) {
        let id = reader.read_array::<4>()?;
        table.push((id, reader.read_u64_le()?));
    }
    Ok(Ds64 {
        riff_size,
        data_size,
        sample_count: if container == WavContainer::Bw64 {
            0
        } else {
            sample_count
        },
        table,
    })
}
