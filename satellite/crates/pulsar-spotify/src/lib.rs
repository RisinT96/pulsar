//! Spotify Connect integration building blocks.
//!
//! Spotify's volume requests must go through the controller, which owns the shared master
//! volume and mute state. They must never change the ALSA mixer independently.
#![deny(unsafe_code)]

mod service;
mod sink;
pub use service::{Config, Event, Handle, Playback, Transport, spawn};
use std::sync::atomic::{AtomicU16, Ordering};
use tokio::sync::mpsc::UnboundedSender;

/// Spotify Connect volume is a raw 0..65535 scale; Pulsar's master volume is 0..1.
pub(crate) fn volume_to_u16(volume: f32) -> u16 {
    (volume.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

pub(crate) fn volume_to_f32(volume: u16) -> f32 {
    f32::from(volume) / f32::from(u16::MAX)
}

/// A librespot mixer that forwards remote volume requests to the Pulsar controller.
pub struct Mixer {
    volume: AtomicU16,
    events: UnboundedSender<u16>,
}

impl Mixer {
    pub fn new(volume: u16, events: UnboundedSender<u16>) -> Self {
        Self {
            volume: AtomicU16::new(volume),
            events,
        }
    }

    /// Report a local volume change without feeding it back as a remote request.
    pub fn report_volume(&self, volume: u16) {
        self.volume.store(volume, Ordering::Relaxed);
    }
}

impl librespot_playback::mixer::Mixer for Mixer {
    fn open(_: librespot_playback::mixer::MixerConfig) -> Result<Self, librespot_core::Error> {
        Err(librespot_core::Error::unavailable(
            "Pulsar mixer requires a controller",
        ))
    }

    fn volume(&self) -> u16 {
        self.volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u16) {
        if self.volume.swap(volume, Ordering::Relaxed) != volume {
            let _ = self.events.send(volume);
        }
    }

    fn get_soft_volume(&self) -> Box<dyn librespot_playback::mixer::VolumeGetter + Send> {
        Box::new(librespot_playback::mixer::NoOpVolume)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_playback::mixer::Mixer as _;

    #[test]
    fn remote_volume_is_forwarded_and_local_reports_do_not_loop() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mixer = Mixer::new(123, tx);
        mixer.set_volume(456);
        assert_eq!(rx.try_recv(), Ok(456));
        assert_eq!(mixer.volume(), 456);
        mixer.report_volume(789);
        assert_eq!(mixer.volume(), 789);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn volume_scale_round_trips_at_the_ends_and_rounds_steps() {
        assert_eq!(volume_to_u16(0.0), 0);
        assert_eq!(volume_to_u16(1.0), u16::MAX);
        assert_eq!(volume_to_u16(0.5), 32_768);
        assert_eq!(volume_to_u16(-1.0), 0);
        assert_eq!(volume_to_u16(2.0), u16::MAX);
        assert_eq!(volume_to_f32(0), 0.0);
        assert_eq!(volume_to_f32(u16::MAX), 1.0);
    }
}
