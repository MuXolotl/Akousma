//! Decoding benchmark: akousma against symphonia, hound and the ffmpeg CLI.
//!
//! ```text
//! cargo run --release -- [--dir PATH] [--out BENCHMARKS.md] [--runs N] [--ffmpeg PATH]
//! ```
#![forbid(unsafe_code)]

#[path = "../../shared/markdown.rs"]
mod markdown;
mod synth;

use std::hint::black_box;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use akousma::{AudioBuf, ChannelMask, DecodeOptions, DecodedAudio, Decoder, Demuxer};
use akousma::{CodecId, Hint as AkousmaHint, Packet, SeekMode, SeekTarget};
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::formats::{FormatReader, SeekTo, TrackType, probe::Hint};

use markdown::{cell, format_label};

const USAGE: &str =
    "usage: akousma-benchmarks [--dir PATH] [--out FILE] [--runs N] [--ffmpeg PATH]";
/// The name of the ffmpeg pass, and the program run when `--ffmpeg` is not given.
const FFMPEG: &str = "ffmpeg";
/// The akousma pass that keeps the native planar output of the decoder.
const AKOUSMA_RAW: &str = "akousma (decode only)";
/// The decoding passes in timing order: the decode-only pass right after the
/// converting one, so that both see the same state of the machine.
const PASSES: [&str; 5] = ["akousma", AKOUSMA_RAW, "symphonia", "hound", FFMPEG];
/// The compared implementations, in the order of the capability matrix.
const LIBRARIES: [&str; 4] = ["akousma", "symphonia", "hound", FFMPEG];
const SEEKERS: [&str; 3] = ["akousma", "symphonia", "hound"];
const SEEKS: usize = 1000;
/// Frames that must be available from the target after every seek.
const SEEK_FRAMES: usize = 1024;
/// Frames per `hound` read, the size of an akousma PCM packet.
const HOUND_FRAMES: usize = 4096;
/// Prefix of the errors of a library that does not read the input at all.
const UNSUPPORTED: &str = "unsupported: ";

type Bytes = Arc<[u8]>;
type Res<T> = Result<T, String>;

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn symphonia_err(error: symphonia::core::errors::Error) -> String {
    match error {
        symphonia::core::errors::Error::Unsupported(_) => format!("{UNSUPPORTED}{error}"),
        _ => error.to_string(),
    }
}

/// `hound` reads integer PCM and 32-bit float in RIFF files only.
fn hound_reads(bytes: &[u8], codec: CodecId) -> Res<()> {
    use CodecId::*;
    let pcm = matches!(codec, PcmU8 | PcmS16Le | PcmS24Le | PcmS32Le | PcmF32Le);
    match bytes.starts_with(b"RIFF") && pcm {
        true => Ok(()),
        false => Err(format!(
            "{UNSUPPORTED}hound reads only integer PCM and 32-bit float in RIFF files"
        )),
    }
}

/// What one implementation did with one input in one measurement.
enum Outcome {
    /// It does not read the input at all.
    Unsupported,
    /// It reads the input but failed: the message.
    Failed(String),
    /// Seconds per frame or per jump, and the difference of its output from
    /// `akousma::decode_bytes` (`""` if equal).
    Timed(f64, String),
}

impl Outcome {
    fn new(result: Res<(f64, String)>) -> Self {
        match result {
            Ok((seconds, tag)) => Self::Timed(seconds, tag),
            Err(e) if e.starts_with(UNSUPPORTED) => Self::Unsupported,
            Err(e) => Self::Failed(e),
        }
    }
}

/// The measurements of one input.
struct Input {
    name: String,
    /// Container and encoding, such as `WAV · PCM 16-bit`.
    format: String,
    /// The container, such as `WAV`: the section of the input.
    container: String,
    /// The header of its column in the section: the encoding, or the file
    /// name with `--dir`.
    column: String,
    megabytes: f64,
    seconds: f64,
    /// The native output type of akousma, such as `i16 planar`.
    planar: &'static str,
    /// One outcome per pass of [`PASSES`] that ran.
    decode: Vec<(&'static str, Outcome)>,
    /// One outcome per library of [`SEEKERS`].
    seek: Vec<(&'static str, Outcome)>,
    /// Frames and seconds of audio of the largest first packet that akousma
    /// decoded after a jump, and of its usual packet, read without a jump.
    ahead: Option<[(usize, f64); 2]>,
}

impl Input {
    fn outcome<'a>(list: &'a [(&str, Outcome)], library: &str) -> Option<&'a Outcome> {
        list.iter().find(|(l, _)| *l == library).map(|(_, o)| o)
    }

    /// The cell of `library` in the capability matrix.
    fn capability(&self, library: &str) -> &'static str {
        let failed = |o: Option<&Outcome>| matches!(o, Some(Outcome::Failed(_)));
        let (decode, seek) = (
            Self::outcome(&self.decode, library),
            Self::outcome(&self.seek, library),
        );
        match (decode, failed(decode), failed(seek)) {
            (Some(Outcome::Unsupported), ..) => "no",
            (_, true, true) => "fails",
            (_, true, false) => "decode fails",
            (_, false, true) => "seek fails",
            _ => "yes",
        }
    }
}

/// Why the output of `library` may differ from akousma on `codec` by design.
fn known_difference(library: &str, codec: CodecId) -> Option<&'static str> {
    if codec != CodecId::AdpcmImaWav {
        return None;
    }
    // Shown only when the outputs differ, and 9.0.x matches bit for bit, so
    // the label names the versions that still multiply.
    if library == FFMPEG {
        return Some("multiply form, FFmpeg 8.0 and earlier");
    }
    (library == "symphonia").then_some("multiply form")
}

/// A value as shown: at most three significant digits, at least two decimals
/// below 10. The number of decimals follows the value rounded to the
/// precision of its range, so a value that rounds up into the next range
/// takes the decimals of that range (`9.996` is `10.0`, not `10.00`;
/// `0.0996` is `0.10`, not `0.100`). A value that shows as zero has no sign
/// (`-0.0` and `-0.0004` are `0.000`).
fn shown(value: f64) -> String {
    let decimals = |v: f64| match v {
        v if v >= 100.0 => 0,
        v if v >= 10.0 => 1,
        v if v >= 0.1 => 2,
        _ => 3,
    };
    let first = decimals(value);
    let rounded = format!("{value:.first$}").parse().unwrap_or(value);
    let decimals = decimals(rounded);
    let text = format!("{value:.decimals$}");
    match text.strip_prefix('-') {
        Some(zero) if zero.parse() == Ok(0.0) => zero.to_string(),
        _ => text,
    }
}

/// A conversion share as shown: whole percent, without the sign of a share
/// that rounds to zero from below.
fn percent(share: f64) -> String {
    match format!("{share:.0}") {
        zero if zero == "-0" => "0 %".into(),
        text => format!("{text} %"),
    }
}

/// A value as shown, read back: the base of the ratios and of the bold.
fn as_shown(value: f64) -> f64 {
    shown(value).parse().unwrap_or(value)
}

/// A ratio as shown in a summary line: one decimal below 10.
fn times(ratio: f64) -> String {
    match ratio {
        r if r >= 9.95 => format!("{r:.0}"),
        r => format!("{r:.1}"),
    }
}

/// `1.9–3.1`, or one value when both ends are shown the same.
fn span(ratios: &[f64]) -> String {
    let lo = times(ratios.iter().copied().fold(f64::MAX, f64::min));
    let hi = times(ratios.iter().copied().fold(f64::MIN, f64::max));
    match lo == hi {
        true => lo,
        false => format!("{lo}–{hi}"),
    }
}

fn label(library: &str) -> &str {
    match library {
        FFMPEG => "ffmpeg (CLI, end to end)",
        _ => library,
    }
}

/// A row of a container table: the implementation and its value in every
/// column, `None` where it has none.
type Row = (&'static str, Vec<Option<f64>>);

/// The ratios of `values` to those of akousma, column by column, as shown.
fn ratios(values: &[Option<f64>], akousma: &[Option<f64>]) -> Vec<f64> {
    (values.iter().zip(akousma))
        .filter_map(|(v, a)| Some((as_shown((*v)?), as_shown((*a)?))))
        .filter(|&(_, a)| a > 0.0)
        .map(|(v, a)| v / a)
        .collect()
}

/// Ratios of each in-process implementation to akousma, excluding ffmpeg.
fn summary(rows: &[Row]) -> Option<String> {
    let akousma = &rows.iter().find(|row| row.0 == "akousma")?.1;
    let others = rows
        .iter()
        .filter(|row| !matches!(row.0, "akousma" | FFMPEG));
    let parts: Vec<_> = (others.map(|(library, values)| (library, ratios(values, akousma))))
        .filter(|(_, r)| !r.is_empty())
        .map(|(library, ratios)| format!("{library} {}", span(&ratios)))
        .collect();
    (!parts.is_empty()).then(|| {
        format!(
            "Ratios to akousma (other / akousma; lower is faster): {}.",
            parts.join(", ")
        )
    })
}

/// A container table: one column per input and one row per implementation,
/// the in-process rows by their mean ratio to akousma and the ffmpeg row
/// last, the best value of every column in bold, `—` where a row has none.
fn grid(columns: &[&str], mut rows: Vec<Row>) -> String {
    rows.retain(|(_, values)| values.iter().any(Option::is_some));
    let akousma =
        (rows.iter().find(|row| row.0 == "akousma")).map_or(Vec::new(), |row| row.1.clone());
    let key = |(library, values): &Row| {
        let r = ratios(values, &akousma);
        let mean = r.iter().sum::<f64>() / r.len() as f64;
        (
            *library == FFMPEG,
            if r.is_empty() { f64::MAX } else { mean },
        )
    };
    rows.sort_by(|a, b| {
        let ((fa, ma), (fb, mb)) = (key(a), key(b));
        fa.cmp(&fb).then(ma.total_cmp(&mb))
    });
    let best: Vec<_> = (0..columns.len())
        .map(|c| {
            rows.iter()
                .filter_map(|row| row.1[c].map(as_shown))
                .reduce(f64::min)
        })
        .collect();
    let columns: Vec<_> = columns.iter().map(|column| cell(column)).collect();
    let mut table = format!(
        "| Implementation | {} |\n|---|{}",
        columns.join(" | "),
        "---:|".repeat(columns.len())
    );
    for (library, values) in &rows {
        let cells: Vec<_> = (values.iter().zip(&best))
            .map(|(value, best)| match value {
                None => "—".to_string(),
                Some(v) if Some(as_shown(*v)) == *best => format!("**{}**", shown(*v)),
                Some(v) => shown(*v),
            })
            .collect();
        table += &format!("\n| {} | {} |", label(library), cells.join(" | "));
    }
    table
}

/// The sections of one container: decoding speed and seek and read, one
/// table each, with one column per input.
fn container(name: &str, inputs: &[&Input], libraries: &[&'static str]) -> String {
    let columns: Vec<_> = inputs.iter().map(|input| input.column.as_str()).collect();
    let mut sections = String::new();
    let tables = [
        ("decoding speed", "ns per frame", 1e9),
        ("seek and read", "µs per jump", 1e6),
    ];
    for (title, unit, scale) in tables {
        let (mut rows, mut differences) = (Vec::new(), Vec::new());
        for &library in libraries {
            let values = inputs.iter().map(|input| {
                let list = match scale == 1e9 {
                    true => &input.decode,
                    false => &input.seek,
                };
                match Input::outcome(list, library) {
                    Some(Outcome::Timed(value, tag)) => {
                        if !tag.is_empty() {
                            differences.push(format!("`{library}` on {} ({tag})", input.column));
                        }
                        Some(value * scale)
                    }
                    _ => None,
                }
            });
            rows.push((library, values.collect::<Vec<_>>()));
        }
        let timed = |library: &str| {
            rows.iter()
                .any(|(l, v)| *l == library && v.iter().any(Option::is_some))
        };
        if !libraries.iter().any(|l| timed(l)) {
            continue;
        }
        sections += &format!(
            "\n## {name} · {title}\n\n{unit}, lower is better. **Bold** is the best value of a column."
        );
        if let Some(line) = summary(&rows) {
            sections += &format!("\n{line}");
        }
        if timed(FFMPEG) {
            sections += "\nThe ffmpeg row is end to end and is not comparable with the rest.";
        }
        sections += &format!("\n\n{}\n", grid(&columns, rows));
        if !differences.is_empty() {
            sections += &format!(
                "\nOutput differences from akousma: {}.\n",
                differences.join("; ")
            );
        }
    }
    sections
}

/// The akousma-only table: decoding with and without the conversion to
/// interleaved `f32`. It has no bold: decoding without the conversion is
/// always the lower value, so bold would mark nothing.
fn conversion(inputs: &[Input]) -> String {
    let mut table = "| Input | Decode only | Decode + interleaved f32 | Conversion share |\n|---|---:|---:|---:|".to_string();
    for input in inputs {
        let value = |pass| match Input::outcome(&input.decode, pass) {
            Some(Outcome::Timed(value, _)) => Some(value * 1e9),
            _ => None,
        };
        let (Some(raw), Some(full)) = (value(AKOUSMA_RAW), value("akousma")) else {
            continue;
        };
        let share = percent(100.0 * (1.0 - as_shown(raw) / as_shown(full)));
        let (raw, full) = (shown(raw), shown(full));
        table += &format!("\n| {} | {raw} | {full} | {share} |", cell(&input.name));
    }
    table
}

/// `a, b and c`.
fn and(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, first)) => format!("{} and {last}", first.join(", ")),
        None => String::new(),
    }
}

/// `x for A and B, y for C`: the inputs grouped by the value of `of`, which
/// skips an input by returning `None`.
fn grouped(inputs: &[Input], of: impl Fn(&Input) -> Option<String>) -> String {
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for input in inputs {
        let Some(value) = of(input) else { continue };
        match groups.iter_mut().find(|(v, _)| *v == value) {
            Some((_, names)) => names.push(&input.name),
            None => groups.push((value, vec![&input.name])),
        }
    }
    let parts: Vec<_> = groups
        .iter()
        .map(|(value, names)| format!("{value} {}", and(names)))
        .collect();
    parts.join(", ")
}

/// The native output of the decoder per input.
fn natives(inputs: &[Input]) -> String {
    grouped(inputs, |input| {
        Some(format!(
            "`{}` for",
            input.planar.trim_end_matches(" planar")
        ))
    })
}

/// How far akousma decodes ahead after a jump, per input.
fn read_ahead(inputs: &[Input]) -> String {
    let text = grouped(inputs, |input| {
        let [(first, seconds), (usual, usual_seconds)] = input.ahead?;
        let shown = format!("{first} frames ({seconds:.2} s of audio)");
        Some(match usual > first {
            true => format!(
                "{shown}, rising to {usual} ({usual_seconds:.2} s) in the packets after it, on"
            ),
            false => format!("{shown} on"),
        })
    });
    match text.is_empty() {
        true => "nothing, as no input was timed".into(),
        false => text,
    }
}

/// One opened file of one library.
trait Session {
    /// Decodes the next packet into `out`: its frames, `None` at the end.
    fn next(&mut self, out: &mut [f32]) -> Res<Option<usize>>;
    /// Seeks to `frame`: the number of frames to drop from the next output.
    fn seek(&mut self, frame: u64) -> Res<usize>;
    /// The most frames decoded in the first packet after a jump, if known.
    fn ahead(&self) -> Option<usize> {
        None
    }
}

struct Akousma {
    demuxer: Box<dyn Demuxer>,
    decoder: Box<dyn Decoder>,
    buf: AudioBuf,
    packet: Packet,
    /// `false` keeps the native planar output of the decoder.
    interleave: bool,
    /// Whether the next packet is the first after a jump.
    jumped: bool,
    /// The most frames of a first packet after a jump.
    ahead: Option<usize>,
}

impl Session for Akousma {
    fn next(&mut self, out: &mut [f32]) -> Res<Option<usize>> {
        if !self.demuxer.next_packet(&mut self.packet).map_err(err)? {
            return Ok(None);
        }
        self.decoder
            .decode(&self.packet, &mut self.buf)
            .map_err(err)?;
        if core::mem::take(&mut self.jumped) {
            let frames = self.packet.duration as usize;
            self.ahead = Some(self.ahead.map_or(frames, |most| most.max(frames)));
        }
        let buf = &self.buf;
        match self.interleave {
            true => _ = buf.copy_interleaved_f32(out.get_mut(..buf.samples()).ok_or("overflow")?),
            false => _ = black_box(buf),
        }
        Ok(Some(buf.frames()))
    }

    fn seek(&mut self, frame: u64) -> Res<usize> {
        let track_id = self.demuxer.default_track().ok_or("no track")?.id;
        let target = SeekTarget::Frame { track_id, frame };
        self.demuxer.seek(SeekMode::Accurate, target).map_err(err)?;
        self.decoder.reset();
        self.jumped = true;
        Ok(0)
    }

    fn ahead(&self) -> Option<usize> {
        self.ahead
    }
}

struct Symphonia(Box<dyn FormatReader>, Box<dyn AudioDecoder>, u32);

impl Session for Symphonia {
    fn next(&mut self, out: &mut [f32]) -> Res<Option<usize>> {
        while let Some(packet) = self.0.next_packet().map_err(err)? {
            if packet.track_id == self.2 {
                let decoded = self.1.decode(&packet).map_err(err)?;
                let slot = out
                    .get_mut(..decoded.samples_interleaved())
                    .ok_or("overflow")?;
                decoded.copy_to_slice_interleaved::<f32, _>(slot);
                return Ok(Some(decoded.frames()));
            }
        }
        Ok(None)
    }

    /// The reader stops at a packet boundary at or before `frame`.
    fn seek(&mut self, frame: u64) -> Res<usize> {
        let ts = symphonia::core::units::Timestamp::new(frame as i64);
        let to = SeekTo::Timestamp {
            ts,
            track_id: self.2,
        };
        let seeked = self
            .0
            .seek(symphonia::core::formats::SeekMode::Accurate, to);
        self.1.reset();
        Ok((frame as i64 - seeked.map_err(err)?.actual_ts.get()) as usize)
    }
}

struct Hound(hound::WavReader<Cursor<Bytes>>, usize);

fn fill<S>(
    samples: impl Iterator<Item = hound::Result<S>>,
    out: &mut [f32],
    f: impl Fn(S) -> f32,
) -> Res<usize> {
    let mut written = 0;
    for (slot, sample) in out.iter_mut().zip(samples) {
        (*slot, written) = (f(sample.map_err(err)?), written + 1);
    }
    Ok(written)
}

impl Session for Hound {
    /// Integer samples are scaled by `2^-(bits - 1)`, like akousma and symphonia.
    fn next(&mut self, out: &mut [f32]) -> Res<Option<usize>> {
        let (spec, len) = (self.0.spec(), (HOUND_FRAMES * self.1).min(out.len()));
        let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
        let written = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Float, _) => fill(self.0.samples(), &mut out[..len], |s: f32| s),
            (_, ..=16) => fill(self.0.samples(), &mut out[..len], |s: i16| {
                f32::from(s) * scale
            }),
            _ => fill(self.0.samples(), &mut out[..len], |s: i32| s as f32 * scale),
        };
        Ok(Some(written? / self.1).filter(|&frames| frames > 0))
    }

    fn seek(&mut self, frame: u64) -> Res<usize> {
        self.0.seek(frame as u32).map_err(err)?;
        Ok(0)
    }
}

fn open(library: &str, bytes: &Bytes, channels: usize) -> Res<Box<dyn Session>> {
    let source = Cursor::new(bytes.clone());
    if library == "hound" {
        return Ok(Box::new(Hound(
            hound::WavReader::new(source).map_err(err)?,
            channels,
        )));
    }
    if library == "symphonia" {
        let stream = symphonia::core::io::MediaSourceStream::new(Box::new(source), <_>::default());
        let probe = symphonia::default::get_probe();
        let format = probe.probe(&Hint::new(), stream, <_>::default(), <_>::default());
        let format = format.map_err(symphonia_err)?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("not audio")?;
        let decoder = symphonia::default::get_codecs().make_audio_decoder(params, &<_>::default());
        let track_id = track.id;
        return Ok(Box::new(Symphonia(
            format,
            decoder.map_err(symphonia_err)?,
            track_id,
        )));
    }
    let options = akousma::DemuxOptions::new();
    let probed = akousma::default_probe().open(&AkousmaHint::new(), source, &options);
    let demuxer = probed.map_err(err)?.demuxer;
    let params = &demuxer.default_track().ok_or("no track")?.params;
    let decoder = akousma::default_codecs()
        .make(params, &DecodeOptions::new())
        .map_err(err)?;
    let buf = AudioBuf::f32(1, 1, ChannelMask::EMPTY);
    let interleave = library != AKOUSMA_RAW;
    Ok(Box::new(Akousma {
        demuxer,
        decoder,
        buf: buf.map_err(err)?,
        packet: Packet::default(),
        interleave,
        jumped: false,
        ahead: None,
    }))
}

/// Decodes the whole file into `out`: the interleaved samples written.
fn decode(library: &str, bytes: &Bytes, channels: usize, out: &mut [f32]) -> Res<usize> {
    let (mut session, mut frames) = (open(library, bytes, channels)?, 0);
    while let Some(decoded) = session.next(&mut out[frames * channels..])? {
        frames += decoded;
    }
    Ok(frames * channels)
}

/// Every jump leaves the [`SEEK_FRAMES`] frames from its target at the start of `out`.
fn jump(
    session: &mut dyn Session,
    targets: &[u64],
    channels: usize,
    out: &mut [f32],
) -> Res<usize> {
    for &target in targets {
        let (mut skip, mut frames) = (session.seek(target)?, 0);
        while frames < SEEK_FRAMES {
            let Some(decoded) = session.next(&mut out[frames * channels..])? else {
                break;
            };
            let dropped = decoded.min(skip);
            let kept = (frames + dropped) * channels..(frames + decoded) * channels;
            out.copy_within(kept, frames * channels);
            (skip, frames) = (skip - dropped, frames + decoded - dropped);
        }
        if frames < SEEK_FRAMES {
            return Err(format!("seek to frame {target} gave {frames} frames"));
        }
    }
    Ok(targets.len() * SEEK_FRAMES * channels)
}

/// Runs the ffmpeg CLI `program` on `path`: the interleaved samples written
/// to `raw`.
fn ffmpeg(program: &Path, path: &Path, raw: &mut Vec<u8>) -> Res<usize> {
    let mut command = Command::new(program);
    command.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "1",
        "-i",
    ]);
    command
        .arg(path)
        .args(["-f", "f32le", "-"])
        .stdin(Stdio::null());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(err)?;
    raw.clear();
    child
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_end(raw)
        .map_err(err)?;
    let output = child.wait_with_output().map_err(err)?;
    match output.status.success() {
        true => Ok(raw.len() / 4),
        false => Err(String::from_utf8_lossy(&output.stderr).trim().into()),
    }
}

/// Median seconds of `runs` timed runs after one warm-up run, and the samples of a run.
fn measure(runs: usize, mut run: impl FnMut() -> Res<usize>) -> Res<(f64, usize)> {
    let samples = run()?;
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let produced = black_box(run()?);
        times.push(start.elapsed().as_secs_f64());
        if produced != samples {
            return Err(format!(
                "unstable output: {samples} then {produced} samples"
            ));
        }
    }
    times.sort_by(f64::total_cmp);
    Ok((times[runs / 2], samples))
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    let diffs = a.iter().zip(b).map(|(a, b)| (a - b).abs());
    diffs.fold(0.0, f32::max)
}

/// `Err` if the number of `samples` of `output` (`None`: only the length is
/// known) differs from `reference`; otherwise the difference: `""` if equal.
fn check(
    samples: usize,
    output: Option<&[f32]>,
    reference: &[f32],
    channels: usize,
    known: Option<&str>,
) -> Res<String> {
    if samples != reference.len() {
        let (frames, expected) = (samples / channels, reference.len() / channels);
        return Err(format!("{frames} frames instead of {expected}"));
    }
    let diff = output.map_or(0.0, |output| max_diff(output, reference));
    Ok(tag(diff, known))
}

/// The difference of an output from akousma, `""` if equal, with its reason
/// when it is `known`.
fn tag(diff: f32, known: Option<&str>) -> String {
    match (diff > 0.0, known) {
        (false, _) => String::new(),
        (true, Some(why)) => format!("{why}, max abs diff {diff:.1e}"),
        (true, None) => format!("max abs diff {diff:.1e}"),
    }
}

/// The container that akousma finds in `bytes`, the native output type of
/// its decoder and the frames of its first packet, a usual one.
fn native(bytes: &Bytes) -> Res<(&'static str, &'static str, usize)> {
    let source = Cursor::new(bytes.clone());
    let options = akousma::DemuxOptions::new();
    let probed = akousma::default_probe().open(&AkousmaHint::new(), source, &options);
    let probed = probed.map_err(err)?;
    let (container, mut demuxer) = (probed.format, probed.demuxer);
    let params = &demuxer.default_track().ok_or("no track")?.params;
    let mut decoder = (akousma::default_codecs())
        .make(params, &DecodeOptions::new())
        .map_err(err)?;
    let mut buf = AudioBuf::f32(1, 1, ChannelMask::EMPTY).map_err(err)?;
    let mut packet = Packet::default();
    demuxer.next_packet(&mut packet).map_err(err)?;
    decoder.decode(&packet, &mut buf).map_err(err)?;
    let planar = match () {
        _ if buf.plane::<i16>(0).is_some() => "i16 planar",
        _ if buf.plane::<i32>(0).is_some() => "i32 planar",
        _ => "f32 planar",
    };
    Ok((container, planar, packet.duration as usize))
}

/// Measures every implementation on a file that `akousma::decode_bytes`
/// decoded into `reference`, and the ffmpeg CLI `ffmpeg` if it is given.
/// `name` is the format label if `None`.
fn bench(
    name: Option<String>,
    bytes: Vec<u8>,
    reference: DecodedAudio,
    runs: usize,
    ffmpeg_program: Option<&Path>,
) -> Res<Input> {
    let ff = ffmpeg_program.is_some();
    let codec = reference.params.codec;
    let bytes: Bytes = bytes.into();
    let (container, planar, usual) = native(&bytes)?;
    let format = format_label(container, codec);
    let (container, encoding) = format.split_once(" · ").unwrap_or((&format, &format));
    let (container, column) = (
        container.to_string(),
        name.clone().unwrap_or(encoding.into()),
    );
    let name = name.unwrap_or_else(|| format.clone());
    eprintln!("benchmarking {name}");
    let channels = usize::from(reference.params.channels.max(1));
    let frames = reference.frames().max(1) as f64;
    let reference_rate = reference.params.sample_rate;
    let seconds = reference.frames() as f64 / f64::from(reference_rate.max(1));
    let hound = hound_reads(&bytes, codec);
    let supports = |library| match library {
        "hound" => hound.clone(),
        _ => Ok(()),
    };
    let reference = reference.samples;
    let megabytes = bytes.len() as f64 / 1e6;
    let mut out = vec![0f32; reference.len() + (1 << 16) * channels];
    let path = std::env::temp_dir().join(format!("akousma-bench-{}", std::process::id()));
    let mut piped = Vec::new();
    if ff {
        std::fs::write(&path, &bytes).map_err(err)?;
    }
    let mut decoded = Vec::new();
    for library in PASSES.into_iter().filter(|&l| ff || l != FFMPEG) {
        let known = known_difference(library, codec);
        let result = match library {
            FFMPEG => measure(runs, || {
                ffmpeg(ffmpeg_program.ok_or("no ffmpeg")?, &path, &mut piped)
            })
            .and_then(|(secs, samples)| {
                let floats: Vec<_> = (piped.as_chunks().0.iter())
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();
                let tag = check(samples, Some(&floats), &reference, channels, known)?;
                Ok((secs / frames, tag))
            }),
            _ => supports(library)
                .and_then(|()| measure(runs, || decode(library, &bytes, channels, &mut out)))
                .and_then(|(secs, samples)| {
                    let output = (library != AKOUSMA_RAW).then(|| &out[..samples]);
                    let tag = check(samples, output, &reference, channels, known)?;
                    Ok((secs / frames, tag))
                }),
        };
        decoded.push((library, Outcome::new(result)));
    }
    if ff {
        std::fs::remove_file(&path).map_err(err)?;
    }
    let span = (reference.len() / channels).saturating_sub(SEEK_FRAMES) as u64 + 1;
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut random = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D) % span
    };
    let targets: Vec<u64> = (0..SEEKS).map(|_| random()).collect();
    let (mut seek, mut ahead) = (Vec::new(), None);
    let rate = f64::from(reference_rate.max(1));
    for library in SEEKERS {
        let result = supports(library)
            .and_then(|()| open(library, &bytes, channels))
            .and_then(|mut session| {
                let (secs, _) =
                    measure(runs, || jump(&mut *session, &targets, channels, &mut out))?;
                let mut worst = 0f32;
                for &target in &targets {
                    jump(&mut *session, &[target], channels, &mut out)?;
                    let at = target as usize * channels..(target as usize + SEEK_FRAMES) * channels;
                    worst = worst.max(max_diff(&out[..at.len()], &reference[at]));
                }
                if let Some(frames) = session.ahead() {
                    let seconds = |frames| (frames, frames as f64 / rate);
                    ahead = Some([seconds(frames), seconds(usual)]);
                }
                Ok((
                    secs / SEEKS as f64,
                    tag(worst, known_difference(library, codec)),
                ))
            });
        seek.push((library, Outcome::new(result)));
    }
    Ok(Input {
        name,
        format,
        container,
        column,
        megabytes,
        seconds,
        planar,
        decode: decoded,
        seek,
        ahead,
    })
}

/// First line of the standard output of a successful command.
fn first_line(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(output.stdout)
        .ok()
        .filter(|_| output.status.success())?;
    text.lines().next().map(|line| line.trim().to_string())
}

/// `(OS, CPU)`: the Linux distribution and the CPU model where the system tells them.
fn machine() -> (String, String) {
    let read = |path| std::fs::read_to_string(path).unwrap_or_default();
    let (release, cpuinfo) = (read("/etc/os-release"), read("/proc/cpuinfo"));
    let distribution = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name")?.split(':').nth(1));
    let cpu = (model.map(|m| m.trim().to_string()))
        .or_else(|| first_line("sysctl", &["-n", "machdep.cpu.brand_string"]))
        .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
        .unwrap_or_else(|| "unknown".into());
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    (
        // No distribution name outside Linux: no trailing space either.
        format!("{os} {arch} {}", distribution.trim_matches('"'))
            .trim_end()
            .to_string(),
        format!("{cpu}, {threads} logical CPUs"),
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (mut dir, mut out_path, mut runs) = (None, None, 7);
    let mut program: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(args.next().ok_or(USAGE)?),
            "--out" => out_path = Some(args.next().ok_or(USAGE)?),
            "--ffmpeg" => program = Some(args.next().ok_or(USAGE)?.into()),
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|&n| n > 0)
                    .ok_or(USAGE)?
            }
            _ => return Err(format!("unknown argument `{arg}`\n{USAGE}").into()),
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("warning: debug build, the numbers are not representative (use --release)");
    }
    let given = program.is_some();
    let program = program.unwrap_or_else(|| FFMPEG.into());
    let ffmpeg = first_line(&program, &["-version"]).map(|v| {
        v.split(" Copyright")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let ff = ffmpeg.is_some().then_some(program.as_path());
    let (mut inputs, mut skipped) = (Vec::new(), Vec::new());
    let source = match &dir {
        None => {
            for bytes in synth::inputs() {
                let reference = akousma::decode_bytes(bytes.clone())?;
                inputs.push(bench(None, bytes, reference, runs, ff)?);
            }
            format!(
                "synthetic, 4 stereo {} Hz WAVE files of about 30 s generated in RAM (three tones per channel plus xorshift64* noise); the IMA ADPCM file has 2048-byte blocks of 2041 frames and an `IMAADPCMWAVEFORMAT` header (`cbSize` 2, holding `wSamplesPerBlock`)",
                synth::RATE
            )
        }
        Some(dir) => {
            let mut paths: Vec<_> = std::fs::read_dir(dir)?
                .map(|e| e.map(|e| e.path()))
                .collect::<Result<_, _>>()?;
            paths.sort();
            for path in paths.into_iter().filter(|path| path.is_file()) {
                let (name, bytes) = (
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    std::fs::read(&path)?,
                );
                match akousma::decode_bytes(bytes.clone()) {
                    Ok(reference) => {
                        inputs.push(bench(Some(name.into()), bytes, reference, runs, ff)?)
                    }
                    Err(e) => skipped.push(format!(
                        "- `{name}` is skipped: akousma does not decode it ({e})."
                    )),
                }
            }
            format!("every file in `{dir}` that akousma decodes")
        }
    };
    let ffmpeg = ffmpeg.ok_or(match given {
        true => format!("ffmpeg not found at `{}`", program.display()),
        false => "ffmpeg not found in PATH".into(),
    });
    let report = report(&inputs, skipped, dir.is_some(), ffmpeg, source, runs);
    match out_path {
        Some(path) => std::fs::write(path, report)?,
        None => print!("{report}"),
    }
    Ok(())
}

/// The report from the measured inputs: `skipped` lists the files of
/// `--dir` that akousma does not decode, `ffmpeg` is the version line of
/// the ffmpeg that ran or why none ran, and `source` describes the inputs.
fn report(
    inputs: &[Input],
    skipped: Vec<String>,
    dir: bool,
    ffmpeg: Result<String, String>,
    source: String,
    runs: usize,
) -> String {
    let libraries: Vec<_> = LIBRARIES
        .into_iter()
        .filter(|&l| ffmpeg.is_ok() || l != FFMPEG)
        .collect();
    let mut matrix = format!(
        "| Input | {} |\n|---|{}",
        libraries.join(" | "),
        ":-:|".repeat(libraries.len())
    );
    let mut table = match dir {
        true => "| Input | Format | Size | Duration |\n|---|---|---:|---:|".to_string(),
        false => "| Input | Size | Duration |\n|---|---:|---:|".to_string(),
    };
    let mut errors = Vec::new();
    for input in inputs {
        let name = &input.name;
        let row = cell(name);
        let cells: Vec<_> = libraries.iter().map(|l| input.capability(l)).collect();
        matrix += &format!("\n| {row} | {} |", cells.join(" | "));
        let format = match dir {
            true => format!(" {} |", cell(&input.format)),
            false => String::new(),
        };
        let size = match input.megabytes {
            mb if mb >= 0.1 => format!("{mb:.1}"),
            mb => shown(mb),
        };
        let seconds = input.seconds;
        table += &format!("\n| {row} |{format} {size} MB | {seconds:.1} s |");
        for (list, what) in [(&input.decode, "decoding"), (&input.seek, "seek and read")] {
            for (library, outcome) in list.iter() {
                if let Outcome::Failed(e) = outcome {
                    let e = e.split_whitespace().collect::<Vec<_>>().join(" ");
                    let e = e.trim_end_matches('.');
                    errors.push(format!("- `{library}` on {name}, {what}: {e}."));
                }
            }
        }
    }
    let mut containers: Vec<(&str, Vec<&Input>)> = Vec::new();
    for input in inputs {
        match containers
            .iter_mut()
            .find(|(name, _)| *name == input.container)
        {
            Some((_, list)) => list.push(input),
            None => containers.push((&input.container, vec![input])),
        }
    }
    let sections: String = (containers.iter())
        .map(|(name, list)| container(name, list, &libraries))
        .collect();
    errors.extend(skipped);
    let errors = match errors.is_empty() {
        true => String::new(),
        false => format!("\n**Errors.**\n\n{}\n", errors.join("\n")),
    };
    let (os, cpu) = machine();
    let ffmpeg = ffmpeg.unwrap_or_else(|missing| missing);
    let build = if cfg!(debug_assertions) {
        "debug, not representative"
    } else {
        "release"
    };
    let fields = [
        ("{os}", os),
        ("{cpu}", cpu),
        ("{rustc}", env!("BENCH_RUSTC").into()),
        ("{build}", build.into()),
        (
            "{libraries}",
            format!("{}, {ffmpeg}", env!("BENCH_LIBRARIES")),
        ),
        ("{source}", source),
        ("{runs}", runs.to_string()),
        ("{seeks}", SEEKS.to_string()),
        ("{seek_frames}", SEEK_FRAMES.to_string()),
        ("{hound_frames}", HOUND_FRAMES.to_string()),
        ("{matrix}", matrix),
        ("{inputs}", table),
        ("{sections}", sections),
        ("{conversion}", conversion(inputs)),
        ("{natives}", natives(inputs)),
        ("{ahead}", read_ahead(inputs)),
        ("{errors}", errors),
    ];
    substitute(include_str!("report.md"), &fields)
}

/// `template` with every `{key}` of `fields` replaced by its value, in one
/// pass over the template: a value is never searched for keys, so a file
/// named `{ahead}.wav` keeps its name.
fn substitute(template: &str, fields: &[(&str, String)]) -> String {
    let (mut out, mut rest) = (String::with_capacity(template.len()), template);
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match fields.iter().find(|(key, _)| rest.starts_with(key)) {
            Some((key, value)) => {
                out.push_str(value);
                rest = &rest[key.len()..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input of the WAV container with only the fields that the text
    /// functions read.
    fn input(name: &str, ahead: Option<[(usize, f64); 2]>) -> Input {
        Input {
            name: name.into(),
            format: format!("WAV · {name}"),
            container: "WAV".into(),
            column: name.into(),
            megabytes: 1.0,
            seconds: 30.0,
            planar: "i16 planar",
            decode: Vec::new(),
            seek: Vec::new(),
            ahead,
        }
    }

    /// The cells of the body rows of a Markdown table, row by row.
    fn body(table: &str) -> Vec<Vec<String>> {
        let rows = table.lines().skip(2);
        rows.map(|row| {
            let row = row.trim_start_matches("| ").trim_end_matches(" |");
            row.split(" | ").map(String::from).collect()
        })
        .collect()
    }

    #[test]
    fn values_are_shown_with_three_significant_digits() {
        let cases = [
            (0.0, "0.000"),
            (-0.0, "0.000"),
            (0.0004, "0.000"),
            (-0.0004, "0.000"),
            (-0.002, "-0.002"),
            (0.05, "0.050"),
            (0.0994, "0.099"),
            (0.0996, "0.10"),
            (0.09996, "0.10"),
            (1.234, "1.23"),
            (1.236, "1.24"),
            (9.996, "10.0"),
            (12.34, "12.3"),
            (99.96, "100"),
            (123.4, "123"),
            (1234.5, "1234"),
        ];
        for (value, text) in cases {
            assert_eq!(shown(value), text, "{value}");
        }
        assert_eq!(as_shown(1.236), 1.24);
        assert_eq!(as_shown(-0.0).to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn shares_never_show_a_negative_zero() {
        assert_eq!(percent(66.4), "66 %");
        assert_eq!(percent(-0.3), "0 %");
        assert_eq!(percent(-0.0), "0 %");
        assert_eq!(percent(-2.0), "-2 %");
    }

    #[test]
    fn ratios_and_their_span() {
        assert_eq!(times(1.94), "1.9");
        assert_eq!(times(9.94), "9.9");
        assert_eq!(times(9.96), "10");
        assert_eq!(times(25.4), "25");
        assert_eq!(span(&[3.1, 1.9, 2.5]), "1.9–3.1");
        assert_eq!(span(&[2.01, 2.04]), "2.0");
        // Only the columns where both sides have a value, as shown, and an
        // akousma value that shows as zero gives no ratio.
        let ratio = ratios(
            &[Some(2.0), None, Some(6.0), Some(5.0)],
            &[Some(1.0), Some(1.0), None, Some(0.0004)],
        );
        assert_eq!(ratio, [2.0]);
        assert_eq!(ratios(&[Some(4.004)], &[Some(2.0)]), [2.0]);
    }

    #[test]
    fn the_summary_names_in_process_libraries_only() {
        let rows: Vec<Row> = vec![
            ("akousma", vec![Some(1.0), Some(2.0)]),
            ("symphonia", vec![Some(2.0), Some(6.0)]),
            ("hound", vec![Some(25.0), None]),
            (FFMPEG, vec![Some(0.5), Some(0.5)]),
        ];
        assert_eq!(
            summary(&rows).as_deref(),
            Some(
                "Ratios to akousma (other / akousma; lower is faster): symphonia 2.0–3.0, hound 25."
            )
        );
        let rows: Vec<Row> = vec![
            ("akousma", vec![Some(2.0), Some(2.0)]),
            ("symphonia", vec![Some(1.0), Some(4.0)]),
            ("hound", vec![Some(1.0), Some(1.0)]),
        ];
        assert_eq!(
            summary(&rows).as_deref(),
            Some(
                "Ratios to akousma (other / akousma; lower is faster): symphonia 0.5–2.0, hound 0.5."
            )
        );
        // No other in-process library, or no akousma row: no summary.
        assert_eq!(
            summary(&[("akousma", vec![Some(1.0)]), (FFMPEG, vec![Some(9.0)])]),
            None
        );
        assert_eq!(summary(&[("symphonia", vec![Some(1.0)])]), None);
    }

    #[test]
    fn the_grid_orders_rows_and_bolds_the_best_values_of_every_column() {
        let rows: Vec<Row> = vec![
            (FFMPEG, vec![Some(0.5), Some(0.5), Some(0.5)]),
            ("hound", vec![Some(30.0), None, Some(8.0)]),
            ("akousma", vec![Some(1.0), Some(2.0), Some(8.0)]),
            ("symphonia", vec![Some(4.0), Some(5.0), Some(8.004)]),
            ("nothing", vec![None, None, None]),
        ];
        let table = grid(&["A", "B", "C"], rows);
        assert!(table.starts_with("| Implementation | A | B | C |\n|---|---:|---:|---:|\n"));
        // By mean ratio to akousma, ffmpeg last whatever its values; a row
        // without values is left out.
        assert_eq!(
            body(&table),
            [
                ["akousma", "1.00", "2.00", "8.00"],
                ["symphonia", "4.00", "5.00", "8.00"],
                ["hound", "30.0", "—", "8.00"],
                [
                    "ffmpeg (CLI, end to end)",
                    "**0.50**",
                    "**0.50**",
                    "**0.50**"
                ],
            ]
        );
        let rows: Vec<Row> = vec![
            ("akousma", vec![Some(1.0), Some(8.0)]),
            ("symphonia", vec![Some(4.0), Some(8.004)]),
            ("hound", vec![None, Some(7.996)]),
        ];
        // Column B ties at 8.00 as shown: every value of the tie is bold
        // (hound comes before symphonia by its mean ratio, 1.0 to 2.5).
        assert_eq!(
            body(&grid(&["A", "B"], rows)),
            [
                ["akousma", "**1.00**", "**8.00**"],
                ["hound", "—", "**8.00**"],
                ["symphonia", "4.00", "**8.00**"],
            ]
        );
    }

    #[test]
    fn lists_join_with_commas_and_a_final_and() {
        assert_eq!(and(&[]), "");
        assert_eq!(and(&["a"]), "a");
        assert_eq!(and(&["a", "b"]), "a and b");
        assert_eq!(and(&["a", "b", "c"]), "a, b and c");
        let inputs = [input("A", None), input("B", None), input("C", None)];
        let parity = |input: &Input| match input.name.as_str() {
            "B" => None,
            "A" => Some("x for".to_string()),
            _ => Some("y for".to_string()),
        };
        assert_eq!(grouped(&inputs, parity), "x for A, y for C");
        assert_eq!(natives(&inputs), "`i16` for A, B and C");
    }

    #[test]
    fn the_read_ahead_names_the_packet_it_rises_to() {
        let pcm = Some([(4096, 0.0929), (4096, 0.0929)]);
        let ima = Some([(2041, 0.0463), (16328, 0.3703)]);
        let inputs = [
            input("PCM 16-bit", pcm),
            input("Float 32-bit", pcm),
            input("IMA ADPCM", ima),
            input("Not timed", None),
        ];
        assert_eq!(
            read_ahead(&inputs),
            "4096 frames (0.09 s of audio) on PCM 16-bit and Float 32-bit, \
             2041 frames (0.05 s of audio), rising to 16328 (0.37 s) in the packets after it, on IMA ADPCM"
        );
        assert_eq!(read_ahead(&inputs[3..]), "nothing, as no input was timed");
    }

    #[test]
    fn the_template_is_filled_in_one_pass() {
        let fields = [("{a}", "{b} and {c".to_string()), ("{b}", "B".to_string())];
        assert_eq!(
            substitute("{a}, {b}, {x}, {", &fields),
            "{b} and {c, B, {x}, {"
        );
    }

    #[test]
    fn file_names_reach_the_report_as_they_are() {
        let inputs = [input("a|b.wav", None), input("{ahead}.wav", None)];
        let ffmpeg = Err("ffmpeg not found in PATH".to_string());
        let report = report(&inputs, Vec::new(), true, ffmpeg, "files".into(), 7);
        assert!(
            report.contains("\n| a\\|b.wav | yes | yes | yes |"),
            "{report}"
        );
        assert!(report.contains("\n| {ahead}.wav | WAV · {ahead}.wav | 1.0 MB | 30.0 s |"));
        // Prose may name the file as it is; no table row may.
        let rows = report.lines().filter(|line| line.starts_with('|'));
        assert!(rows.clone().all(|row| !row.contains("a|b")), "{report}");
        assert_eq!(rows.filter(|row| row.contains("a\\|b.wav")).count(), 2);
        assert!(report.contains("| Libraries | "));
        assert!(report.contains("ffmpeg not found in PATH |"));
    }
}
