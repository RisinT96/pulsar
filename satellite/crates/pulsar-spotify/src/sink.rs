//! Audio output for librespot on the station's shared playback PCM.
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use log::debug;
use pulsar_alsa::{Format, HwRequest, Pcm};
use pulsar_core::model::Model;

const RATE: u32 = 44_100;
const CHANNELS: u32 = 2;
const FRAME_BYTES: usize = 4;
const WAIT_MS: i32 = 100;

pub struct SpotifySink {
    model: &'static Model,
    pcm: Option<Pcm>,
    gain: Arc<AtomicU32>,
    /// Gain-scaled S16LE frames reused by every packet.
    bytes: Vec<u8>,
}

impl SpotifySink {
    pub fn new(model: &'static Model, gain: Arc<AtomicU32>) -> Self {
        Self {
            model,
            pcm: None,
            gain,
            bytes: Vec::new(),
        }
    }
}

fn err(e: i32) -> SinkError {
    SinkError::OnWrite(pulsar_alsa::strerror(e))
}

impl Sink for SpotifySink {
    fn start(&mut self) -> SinkResult<()> {
        if self.pcm.is_some() {
            self.stop()?;
        }
        let mut pcm = pulsar_alsa::open_playback(self.model).map_err(err)?;
        let hw = pcm
            .set_hw_params(&HwRequest {
                format: Format::S16Le,
                channels: CHANNELS,
                rate: RATE,
                rate_exact: true,
                period_frames: None,
                buffer_frames: None,
            })
            .map_err(err)?;
        let period = hw.period_frames.max(1);
        pcm.set_sw_params(period, period).map_err(err)?;
        pcm.prepare().map_err(err)?;
        debug!(
            "spotify: output '{}' open (period {period}, buffer {} frames)",
            self.model.playback_pcm, hw.buffer_frames
        );
        self.pcm = Some(pcm);
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(mut pcm) = self.pcm.take()
            && let Err(e) = pcm.drop_frames()
        {
            debug!("spotify: output close: {}", pulsar_alsa::strerror(e));
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Err(SinkError::InvalidParams("raw audio unsupported".into()));
        };
        let samples = converter.f64_to_s16(&samples);
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed)).clamp(0.0, 1.0);
        self.bytes.clear();
        self.bytes.reserve(samples.len() * 2);
        for sample in samples {
            // The cast saturates, so a gain of 1.0 cannot wrap a full-scale sample.
            let sample = (f32::from(sample) * gain) as i16;
            self.bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let bytes = &self.bytes;
        let frames = bytes.len() / FRAME_BYTES;
        let pcm = self
            .pcm
            .as_mut()
            .ok_or_else(|| SinkError::NotConnected("Spotify PCM closed".into()))?;
        let mut written = 0;
        while written < frames {
            match pcm.writei(&bytes[written * FRAME_BYTES..], frames - written) {
                Ok(0) => {
                    pcm.wait(WAIT_MS).map_err(err)?;
                }
                Ok(n) => written += n,
                Err(e) if e == -libc::EAGAIN || e == -libc::EINTR => {
                    pcm.wait(WAIT_MS).map_err(err)?;
                }
                Err(e) => pcm.recover(e, true).map_err(err)?,
            }
        }
        Ok(())
    }
}
