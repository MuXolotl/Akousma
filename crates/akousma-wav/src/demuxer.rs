//! The WAVE demuxer.

use std::io::{Read, Seek, SeekFrom};

use akousma_core::codec::{CodecParams, Packet, Track, TrackFlags};
use akousma_core::io::ByteReader;
use akousma_core::meta::{Chapter, MetadataBudget, MetadataLog};
use akousma_core::probe::Demuxer;
use akousma_core::units::{SeekMode, SeekTarget, SeekedTo, TimeBase};
use akousma_core::{DemuxOptions, Error, ErrorKind};
use akousma_meta::id3v2::{self, Id3v2Header};

use crate::chunks::ChunkMeta;
use crate::container::{self, Ds64, Root, W64_LIST_ID, WavContainer};
use crate::fmt::{self, Format, Layout};

/// Frames per packet of uncompressed audio.
const PCM_FRAMES_PER_PACKET: u64 = 4096;
/// Target bytes per packet of ADPCM blocks and of compressed streams.
const PACKET_BYTES: u64 = 16_384;
/// Largest `fmt ` payload read: `WAVEFORMATEX` with the largest extension.
const FMT_MAX_LEN: u64 = 18 + u16::MAX as u64;
/// Identifier of the only track.
const TRACK_ID: u32 = 0;

/// Demuxer of the WAVE family: RIFF, RIFX, RF64, BW64 and Sony Wave64.
///
/// The file has one track. Uncompressed audio is cut into packets of 4096
/// frames, ADPCM into packets of whole blocks (about 16 KiB), other codecs
/// into 16 KiB pieces of the `data` chunk. Seeking takes one source seek for
/// uncompressed audio and ADPCM; compressed streams can only seek to the
/// start. After an ADPCM seek the first packet is the block that holds the
/// target, and each following packet doubles until it reaches the usual
/// size: a jump decodes one block before its first frame is out, and
/// reading on soon runs at full packets again. A target past the end of the
/// audio lands at the end of the audio; [`SeekedTo::actual_ts`] reports
/// where. Such a target is not an error.
///
/// The number of frames of uncompressed audio is computed from the size of
/// the `data` chunk, without a trailing partial frame; a `fact` chunk is
/// ignored. For ADPCM, the frame count of a `fact` chunk (or of the `ds64`
/// chunk of RF64) that is not larger than the blocks hold marks the padding
/// of the last block, which is trimmed with [`DemuxOptions::gapless`].
///
/// A root size of 0 or `0xFFFF_FFFF` (left by writers that stream or stop
/// early) declares no bound, and the container runs to the end of the
/// source. A container whose declared end lies past the end of the source is
/// truncated: its audio ends with the source. A container whose declared end
/// lies before the end of the source is read to the end of the source: a
/// chunk that is fully present in the source extends the declared end (some
/// muxers write the root size with the RIFF arithmetic instead of the whole
/// stream), and only a chunk that is not fully present is
/// [`ErrorKind::InvalidHeader`].
///
/// In RIFF and RIFX, a `data` size of `0xFFFF_FFFF` means that the audio
/// runs to the end of the container. A `data` size of 0 means the same only
/// in a container without a declared bound (a recording that was never
/// finished); otherwise the `data` chunk is empty. Chunks after audio that
/// runs to the end are not read. A bounded file holding an empty `data`
/// chunk and nothing else after the header thus reads as empty, while a
/// file with both sizes at 0 reads everything after the `data` header as
/// audio, even if it is in fact an empty `data` chunk followed by other
/// chunks: the header cannot tell these apart. The same applies at the end
/// of a file whose `data` size is `0xFFFF_FFFF`: a trailing `LIST`, an ID3v1
/// or an APE tag there is decoded as audio, because the header cannot tell
/// it from the audio either; the decoder does not guess. Rewriting such a
/// file with a correct size (or as RF64) is the fix on the writer's side.
///
/// See the crate documentation for the metadata chunks read with
/// [`DemuxOptions::read_metadata`]. They share one [`MetadataBudget`] with a
/// leading ID3v2 tag and, through [`Probe::open`](akousma_core::Probe::open),
/// with the tags around the container: the bytes of every chunk read count
/// against [`FormatLimits::max_metadata_bytes`](akousma_core::FormatLimits),
/// and every field, picture, chapter, cue point and label against
/// [`MetadataBudget::MAX_ENTRIES`]. A known metadata chunk that does not fit
/// in what is left is [`ErrorKind::LimitExceeded`], not skipped. Unknown
/// chunks are skipped without being read and cost nothing.
///
/// Without `read_metadata`, the chunk list is read only up to the `data`
/// chunk once the format (and, for ADPCM, the `fact` chunk) is known: the
/// chunks after it, metadata or not, are neither read nor checked, so a
/// later chunk that crosses the end of the container is not noticed. The
/// audio does not depend on them.
#[derive(Debug)]
pub struct WavDemuxer<R> {
    reader: ByteReader<R>,
    container: WavContainer,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Vec<Chapter>,
    layout: Layout,
    data_start: u64,
    data_len: u64,
    /// Frames delivered: the `fact` count with gapless trimming, else the
    /// frames the `data` chunk holds.
    total_frames: u64,
    gapless: bool,
    packet_bytes: u64,
    /// Length of the next packet while it is still growing after an ADPCM
    /// seek: one block, then doubling while below `packet_bytes`.
    next_packet_bytes: Option<u64>,
    next_byte: u64,
    next_frame: u64,
    pending_trim: u32,
    positioned: bool,
}

/// The chunks that define the audio stream.
#[derive(Default)]
struct Scan {
    ds64: Option<Ds64>,
    format: Option<Format>,
    fact: Option<u32>,
    /// Start and available length.
    data: Option<(u64, u64)>,
}

impl<R: Read + Seek> WavDemuxer<R> {
    /// Opens a WAVE stream.
    ///
    /// A leading ID3v2 tag (which [`Probe::open`](akousma_core::Probe::open)
    /// removes before the demuxer sees the stream) is skipped and, with
    /// [`DemuxOptions::read_metadata`], read.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::InvalidHeader`] if the stream does not start with a
    ///   WAVE header, has a root size too small for the form type, has no
    ///   `fmt ` or `data` chunk, has a chunk that crosses the declared end of
    ///   the container and is not fully present in the source, has an RF64 or
    ///   BW64 `fmt ` or `data` chunk before
    ///   `ds64` or a 32-bit size of `0xFFFF_FFFF` not resolved by `ds64`, or
    ///   has invalid format fields.
    /// - [`ErrorKind::UnsupportedCodec`] and [`ErrorKind::UnsupportedFeature`]
    ///   for formats this crate does not know.
    /// - [`ErrorKind::UnexpectedEof`] for a truncated header or metadata
    ///   chunk.
    /// - [`ErrorKind::LimitExceeded`] if
    ///   [`FormatLimits::max_packet_bytes`](akousma_core::FormatLimits) is
    ///   smaller than one PCM frame or one ADPCM block (or is 0),
    ///   [`FormatLimits::max_tracks`](akousma_core::FormatLimits) is 0, or,
    ///   with `read_metadata`, the metadata does not fit in a full
    ///   [`MetadataBudget`].
    /// - The errors of the source and of the tag parsers.
    pub fn open(source: R, options: &DemuxOptions) -> Result<Self, Error> {
        Self::open_with_budget(source, options, &mut MetadataBudget::new(&options.limits))
    }

    /// Opens a WAVE stream whose metadata takes from `budget`, shared with
    /// the tags that [`Probe::open`](akousma_core::Probe::open) found around
    /// it. On error the budget may be partly used.
    pub(crate) fn open_with_budget(
        source: R,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<Self, Error> {
        if options.limits.max_tracks == 0 {
            return Err(ErrorKind::LimitExceeded.into());
        }
        let mut reader = ByteReader::new(source);
        let end = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut meta = ChunkMeta::new(budget);
        let base = leading_id3v2(&mut reader, end, options, &mut meta)?;
        let root = container::read_root(&mut reader, base)?;
        let container = root.container;
        let scan = scan_chunks(&mut reader, root, end, options, &mut meta)?;

        let Format { mut params, layout } = scan
            .format
            .ok_or(Error::at(ErrorKind::InvalidHeader, base))?;
        let (data_start, available) = scan.data.ok_or(Error::at(ErrorKind::InvalidHeader, base))?;
        let time_base = TimeBase::new(1, params.sample_rate).ok_or(ErrorKind::InvalidHeader)?;
        // A packet holds at least one frame or block, which must fit.
        let limit = options.limits.max_packet_bytes as u64;
        let too_large = || Error::from(ErrorKind::LimitExceeded);
        let (data_len, raw_frames, packet_bytes) = match layout {
            Layout::Pcm { frame_bytes } => {
                let frame_bytes = frame_bytes as u64;
                if frame_bytes > limit {
                    return Err(too_large());
                }
                let frames = available / frame_bytes;
                let per_packet = PCM_FRAMES_PER_PACKET.min(limit / frame_bytes);
                (frames * frame_bytes, frames, per_packet * frame_bytes)
            }
            Layout::Adpcm(blocks) => {
                let align = blocks.block_align as u64;
                if align > limit {
                    return Err(too_large());
                }
                let frames = blocks
                    .frames_in_run(available)
                    .ok_or(ErrorKind::InvalidHeader)?;
                let per_packet = (PACKET_BYTES / align).max(1).min(limit / align);
                (available, frames, per_packet * align)
            }
            Layout::Stream if limit == 0 => return Err(too_large()),
            Layout::Stream => (available, 0, PACKET_BYTES.min(limit)),
        };
        let mut total_frames = raw_frames;
        if let Layout::Adpcm(_) = layout {
            let fact = match (scan.fact, &scan.ds64) {
                (Some(u32::MAX) | None, Some(ds64)) if ds64.sample_count > 0 => {
                    Some(ds64.sample_count)
                }
                (fact, _) => fact.map(u64::from),
            };
            if options.gapless
                && let Some(fact) = fact.filter(|&f| f > 0)
            {
                // A count above what the blocks hold is clamped to them.
                total_frames = fact.min(raw_frames);
                params.padding = u32::try_from(raw_frames - total_frames).unwrap_or(u32::MAX);
            }
        }
        if layout != Layout::Stream {
            params.total_frames = Some(total_frames);
        }

        let (revisions, chapters) = meta.finish(time_base, total_frames)?;
        let mut metadata = MetadataLog::new();
        for revision in revisions {
            metadata.push(revision);
        }
        Ok(Self {
            reader,
            container,
            tracks: vec![track(params, time_base)],
            metadata,
            chapters,
            layout,
            data_start,
            data_len,
            total_frames,
            gapless: options.gapless,
            packet_bytes,
            next_packet_bytes: None,
            next_byte: 0,
            next_frame: 0,
            pending_trim: 0,
            positioned: false,
        })
    }

    /// Member of the WAVE family read.
    #[must_use]
    pub const fn container(&self) -> WavContainer {
        self.container
    }
}

fn track(params: CodecParams, time_base: TimeBase) -> Track {
    Track {
        id: TRACK_ID,
        params,
        time_base,
        start_ts: 0,
        language: None,
        flags: TrackFlags::NONE,
    }
}

/// Skips an ID3v2 tag at the start of the stream, reading it into `meta`
/// with [`DemuxOptions::read_metadata`]. Returns the position after it.
fn leading_id3v2<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    options: &DemuxOptions,
    meta: &mut ChunkMeta<'_>,
) -> Result<u64, Error> {
    if end < id3v2::HEADER_LEN as u64 {
        return Ok(0);
    }
    let head = reader.read_array::<{ id3v2::HEADER_LEN }>()?;
    if head[..3] != id3v2::MAGIC {
        return Ok(0);
    }
    let len = Id3v2Header::parse(&head)?.total_len();
    if options.read_metadata {
        meta.budget()
            .take_bytes(len)
            .map_err(|kind| Error::at(kind, 0))?;
        reader.seek(SeekFrom::Start(0))?;
        meta.id3(&reader.read_bounded_vec(len, usize::MAX)?)?;
    }
    Ok(len)
}

/// Bytes before the root size field of the RIFF family: the magic and the
/// size itself are what a Wave64 muxer that reused RIFF arithmetic subtracts.
const RIFF_ROOT_PREFIX_LEN: u64 = 8;

/// Resolves the size of a chunk of RF64 and BW64 from `ds64`.
fn chunk_size(
    id: [u8; 4],
    size32: Option<u32>,
    size: u64,
    ds64: Option<&Ds64>,
    at: u64,
) -> Result<u64, Error> {
    let Some(ds64) = ds64 else {
        return Ok(size);
    };
    match size32 {
        Some(u32::MAX) if &id == b"data" => Ok(ds64.data_size),
        // Some writers leave 0 in `data` and the size in `ds64`.
        Some(0) if &id == b"data" && ds64.data_size > 0 => Ok(ds64.data_size),
        Some(u32::MAX) => ds64
            .table
            .iter()
            .find(|(table_id, _)| *table_id == id)
            .map(|&(_, size)| size)
            .ok_or(Error::at(ErrorKind::InvalidHeader, at)),
        _ => Ok(size),
    }
}

/// Reads the chunk list up to the end of the container, or up to the
/// `data` chunk when nothing after it is needed.
fn scan_chunks<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    root: Root,
    source_end: u64,
    options: &DemuxOptions,
    meta: &mut ChunkMeta<'_>,
) -> Result<Scan, Error> {
    let container = root.container;
    let big_endian = container.is_big_endian();
    let mut scan = Scan::default();
    // RF64 and BW64 may only know their end once `ds64` is read.
    let mut container_end = root.declared_end(None)?;
    // A Wave64 muxer that reused the RIFF arithmetic wrote the root size as
    // `file size - 8` instead of the whole file, so the file declares a root
    // eight bytes shorter than itself and its `data` chunk ends past that
    // root. The generic extension below covers the same shape, but this
    // recognises it in one step.
    if container == WavContainer::Wave64
        && container_end == source_end.checked_sub(RIFF_ROOT_PREFIX_LEN)
    {
        container_end = Some(source_end);
    }
    let mut pos = root.body_start;
    loop {
        let mut walk_end = container_end.map_or(source_end, |end| end.min(source_end));
        let Some(header) = container::read_chunk_header(reader, container, pos, walk_end)? else {
            break;
        };
        let at = header.start;
        let size = chunk_size(
            header.id,
            header.size32,
            header.size,
            scan.ds64.as_ref(),
            at,
        )?;
        let is_data = &header.id == b"data" && scan.data.is_none();
        // Audio that runs to the end of the container: `0xFFFF_FFFF` in
        // RIFF and RIFX, or 0 in a container without a declared end.
        let streamed = is_data
            && match container {
                WavContainer::Riff | WavContainer::Rifx => {
                    header.size32 == Some(u32::MAX) || (size == 0 && container_end.is_none())
                }
                WavContainer::Rf64 | WavContainer::Bw64 => size == 0 && container_end.is_none(),
                WavContainer::Wave64 => false,
            };
        if !streamed
            && let Some(end) = container_end
            && at.checked_add(size).is_none_or(|chunk_end| chunk_end > end)
        {
            // A muxer that miscounts the root wrote a declared end shorter
            // than the chunks it emitted (an ffmpeg IMA ADPCM encoder wrote
            // the root 16 bytes short, with the `data` chunk ending exactly
            // at the end of the file). The first chunk that does not fit the
            // declared end proves the declaration is fiction, so the file is
            // walked to the end of the source; only a chunk that is not
            // fully present in the source is an error.
            match at.checked_add(size) {
                Some(chunk_end) if chunk_end <= source_end => {
                    container_end = Some(source_end);
                    walk_end = source_end;
                }
                _ => return Err(Error::at(ErrorKind::InvalidHeader, at)),
            }
        }
        let before_ds64 = container.has_ds64() && scan.ds64.is_none();
        match &header.id {
            b"ds64" if before_ds64 => {
                if scan.format.is_some() || scan.data.is_some() {
                    return Err(Error::at(ErrorKind::InvalidHeader, at));
                }
                let ds64 = container::read_ds64(reader, container, size)?;
                container_end = root.declared_end(Some(ds64.riff_size))?;
                scan.ds64 = Some(ds64);
            }
            b"fmt " | b"data" if before_ds64 => {
                return Err(Error::at(ErrorKind::InvalidHeader, at));
            }
            // A repeated `fmt ` is ignored: the first one wins.
            b"fmt " if scan.format.is_none() => {
                reader.seek(SeekFrom::Start(at))?;
                let len = size.min(FMT_MAX_LEN);
                let bytes = reader.read_bounded_vec(len, FMT_MAX_LEN as usize)?;
                let format = fmt::parse(&bytes, big_endian).map_err(|err| err.with_offset(at))?;
                scan.format = Some(format);
            }
            b"fact" if size >= 4 && scan.fact.is_none() => {
                reader.seek(SeekFrom::Start(at))?;
                scan.fact = Some(if big_endian {
                    reader.read_u32_be()?
                } else {
                    reader.read_u32_le()?
                });
            }
            b"data" if is_data => {
                // A truncated source ends the audio early.
                let available = walk_end.saturating_sub(at);
                let len = if streamed {
                    available
                } else {
                    size.min(available)
                };
                scan.data = Some((at, len));
                let needs_fact = matches!(
                    scan.format,
                    Some(Format {
                        layout: Layout::Adpcm(_),
                        ..
                    })
                );
                let complete = scan.format.is_some() && (scan.fact.is_some() || !needs_fact);
                if streamed || (complete && !options.read_metadata) {
                    break;
                }
            }
            // Wave64 lists have GUID sub-chunk headers: not parsed.
            &W64_LIST_ID => {}
            id if options.read_metadata => {
                read_metadata_chunk(reader, *id, at, size, big_endian, meta)?;
            }
            _ => {}
        }
        pos = container::next_chunk(container, at, size)?;
    }
    Ok(scan)
}

/// Reads a metadata chunk into `meta`, taking its bytes from the budget.
/// Unknown chunks are skipped.
fn read_metadata_chunk<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    id: [u8; 4],
    at: u64,
    size: u64,
    big_endian: bool,
    meta: &mut ChunkMeta<'_>,
) -> Result<(), Error> {
    let known = matches!(
        &id,
        b"LIST"
            | b"id3 "
            | b"ID3 "
            | b"id32"
            | b"bext"
            | b"cue "
            | b"smpl"
            | b"acid"
            | b"inst"
            | b"iXML"
            | b"ixml"
            | b"axml"
            | b"cart"
    );
    if !known {
        return Ok(());
    }
    meta.budget()
        .take_bytes(size)
        .map_err(|kind| Error::at(kind, at))?;
    reader.seek(SeekFrom::Start(at))?;
    let bytes = reader.read_bounded_vec(size, usize::MAX)?;
    match &id {
        b"LIST" => meta.list(&bytes, big_endian),
        b"bext" => meta.bext(&bytes),
        b"cue " => meta.cue(&bytes, big_endian),
        b"smpl" => meta.smpl(&bytes, big_endian),
        b"acid" => meta.acid(&bytes, big_endian),
        b"inst" => meta.inst(&bytes),
        b"iXML" | b"ixml" => meta.xml("IXML", &bytes),
        b"axml" => meta.xml("AXML", &bytes),
        b"cart" => meta.cart(&bytes),
        _ => meta.id3(&bytes),
    }
}

impl<R: Read + Seek + Send> Demuxer for WavDemuxer<R> {
    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    fn chapters_mut(&mut self) -> &mut Vec<Chapter> {
        &mut self.chapters
    }

    fn metadata(&mut self) -> &mut MetadataLog {
        &mut self.metadata
    }

    fn next_packet(&mut self, packet: &mut Packet) -> Result<bool, Error> {
        let framed = self.layout != Layout::Stream;
        if self.next_byte >= self.data_len || (framed && self.next_frame >= self.total_frames) {
            return Ok(false);
        }
        let packet_bytes = self.next_packet_bytes.unwrap_or(self.packet_bytes);
        // Below `packet_bytes`, which fits in `usize`.
        let len = (self.data_len - self.next_byte).min(packet_bytes) as usize;
        if !self.positioned {
            self.reader
                .seek(SeekFrom::Start(self.data_start + self.next_byte))?;
            self.positioned = true;
        }
        packet.data.resize(len, 0);
        if let Err(err) = self.reader.read_exact(&mut packet.data) {
            self.positioned = false;
            return Err(err);
        }
        let frames = match self.layout {
            Layout::Pcm { frame_bytes } => (len / frame_bytes) as u64,
            Layout::Adpcm(blocks) => blocks
                .frames_in_run(len as u64)
                .ok_or(ErrorKind::LimitExceeded)?,
            Layout::Stream => 0,
        };
        let duration = u32::try_from(frames).map_err(|_| ErrorKind::LimitExceeded)?;
        let trim_start = core::mem::take(&mut self.pending_trim).min(duration);
        let end = self.next_frame + frames;
        let trim_end = if self.gapless && framed && end > self.total_frames {
            u32::try_from(end - self.total_frames)
                .unwrap_or(u32::MAX)
                .min(duration - trim_start)
        } else {
            0
        };
        *packet = Packet {
            track_id: TRACK_ID,
            pts: self.next_frame,
            duration,
            trim_start,
            trim_end,
            data: core::mem::take(&mut packet.data),
        };
        self.next_byte += len as u64;
        self.next_frame = end;
        self.next_packet_bytes = self
            .next_packet_bytes
            .map(|bytes| bytes * 2)
            .filter(|&bytes| bytes < self.packet_bytes);
        Ok(true)
    }

    fn seek(&mut self, mode: SeekMode, target: SeekTarget) -> Result<SeekedTo, Error> {
        let required = match target {
            SeekTarget::Frame { track_id, frame } if track_id == TRACK_ID => frame,
            SeekTarget::Time {
                track_id: None | Some(TRACK_ID),
                time,
            } => self.tracks[0].time_base.duration_to_ts(time),
            _ => return Err(ErrorKind::TrackNotFound.into()),
        };
        let target = required.min(self.total_frames);
        let mut next_packet_bytes = None;
        let (frame, byte, trim) = match self.layout {
            Layout::Stream if required == 0 => (0, 0, 0),
            Layout::Stream => return Err(ErrorKind::UnsupportedFeature.into()),
            _ if target == self.total_frames => (target, self.data_len, 0),
            // `target` is below the frame count, so the offset is inside `data`.
            Layout::Pcm { frame_bytes } => (target, target * frame_bytes as u64, 0),
            Layout::Adpcm(blocks) => {
                let per_block = u64::from(blocks.samples_per_block);
                let index = target / per_block;
                let start = index * per_block;
                let trim = match mode {
                    // Below `samples_per_block`.
                    SeekMode::Accurate => (target - start) as u32,
                    SeekMode::Coarse => 0,
                };
                // A whole packet would decode up to `K` blocks before the
                // first frame is out. Start with one block and double, so
                // that a short read after the jump stays short and a long
                // one is soon back at full packets.
                next_packet_bytes = Some(blocks.block_align as u64);
                (start, index * blocks.block_align as u64, trim)
            }
        };
        self.next_frame = frame;
        self.next_byte = byte;
        self.pending_trim = trim;
        self.next_packet_bytes = next_packet_bytes;
        self.positioned = false;
        Ok(SeekedTo {
            track_id: TRACK_ID,
            required_ts: required,
            actual_ts: frame + u64::from(trim),
        })
    }
}
