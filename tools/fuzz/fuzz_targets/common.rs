//! Opening and decoding shared by the targets. Every error is a valid outcome
//! for a malformed input and ends the step quietly; only a panic, an abort, a
//! hang or running out of memory is a finding.

use std::io::Cursor;

use akousma::{
    AudioBuf, AudioPlanes, DecodeOptions, Decoder, DemuxOptions, Demuxer, Hint, Packet, Track,
};

/// A demuxer and a decoder of its default track, with the default limits
/// (packets up to 16 MiB).
pub struct Session {
    pub demuxer: Box<dyn Demuxer>,
    pub decoder: Box<dyn Decoder>,
    pub track: Track,
    buf: AudioBuf,
    packet: Packet,
}

impl Session {
    pub fn open(data: &[u8]) -> Option<Self> {
        let source = Cursor::new(data.to_vec());
        let probed = akousma::default_probe()
            .open(&Hint::new(), source, &DemuxOptions::new())
            .ok()?;
        let demuxer = probed.demuxer;
        let track = demuxer.default_track()?.clone();
        let params = &track.params;
        let decoder = akousma::default_codecs()
            .make(params, &DecodeOptions::new())
            .ok()?;
        let planes = AudioPlanes::F32((0..params.channels.max(1)).map(|_| Vec::new()).collect());
        let buf = AudioBuf::new(planes, params.sample_rate, params.channel_mask, 32).ok()?;
        Some(Self {
            demuxer,
            decoder,
            track,
            buf,
            packet: Packet::default(),
        })
    }

    /// Reads and decodes up to `limit` packets of the track. Returns `false`
    /// when a packet failed to decode, `true` when the limit, the end or an
    /// error of the demuxer stopped the reading.
    pub fn read(&mut self, limit: usize) -> bool {
        for _ in 0..limit {
            match self.demuxer.next_packet(&mut self.packet) {
                Ok(true) => {}
                Ok(false) | Err(_) => return true,
            }
            if self.packet.track_id == self.track.id
                && self.decoder.decode(&self.packet, &mut self.buf).is_err()
            {
                return false;
            }
        }
        true
    }
}
