//! The audio tap and the DSP stage that runs the equalizer and limiter
//! before the sink, shared by the desktop binary and the Android library.
//!
//! The tap stores half a second of post-EQ, pre-volume audio. The analyser
//! (desktop) and MilkDrop (desktop) read it; the equalizer, limiter, and
//! volume in `Tapped` shape what is heard on every platform.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

use crate::sink::AudioControl;

/// Half a second of audio.
pub const KEPT: usize = SAMPLE_RATE as usize / 2;

pub struct AudioTap {
    samples: Mutex<VecDeque<f32>>,
    /// The ring the MilkDrop child reads, attached while its window is open.
    #[cfg(not(target_os = "android"))]
    shm: Mutex<Option<Arc<crate::shm::Ring>>>,
}

impl std::fmt::Debug for AudioTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioTap")
    }
}

impl Default for AudioTap {
    fn default() -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(KEPT)),
            #[cfg(not(target_os = "android"))]
            shm: Mutex::new(None),
        }
    }
}

impl AudioTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Connects the tap to MilkDrop's shared-memory ring. `None` detaches it.
    #[cfg(not(target_os = "android"))]
    pub fn set_shm(&self, ring: Option<Arc<crate::shm::Ring>>) {
        *self.shm.lock().unwrap_or_else(|p| p.into_inner()) = ring;
    }

    /// Adds scaled stereo samples to the mono analyser buffer and, when
    /// attached, MilkDrop's stereo shared-memory ring.
    pub fn push(&self, interleaved: &[f64], gain: f32) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let (frames, _) = interleaved.as_chunks::<{ NUM_CHANNELS as usize }>();
        #[cfg(not(target_os = "android"))]
        let shm = self.shm.lock().unwrap_or_else(|p| p.into_inner()).clone();
        #[cfg(not(target_os = "android"))]
        let mut stereo: Vec<f32> = if shm.is_some() {
            Vec::with_capacity(frames.len() * 2)
        } else {
            Vec::new()
        };
        for frame in frames {
            let mono = frame.iter().sum::<f64>() as f32 / frame.len() as f32 * gain;
            if samples.len() == KEPT {
                samples.pop_front();
            }
            samples.push_back(mono);
            #[cfg(not(target_os = "android"))]
            if shm.is_some() {
                stereo.push(frame[0] as f32 * gain);
                stereo.push(frame[1] as f32 * gain);
            }
        }
        #[cfg(not(target_os = "android"))]
        if let Some(ring) = &shm {
            ring.push(&stereo);
        }
    }

    /// The `count` samples ending `lag` samples before the newest, with
    /// silence where there is less than that.
    pub fn window(&self, count: usize, lag: usize) -> Vec<f32> {
        let samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let end = samples.len().saturating_sub(lag);
        let start = end.saturating_sub(count);
        let mut out = vec![0.0; count];
        let taken = end - start;
        for (slot, sample) in out[count - taken..]
            .iter_mut()
            .zip(samples.range(start..end))
        {
            *slot = *sample;
        }
        out
    }

    pub fn clear(&self) {
        self.samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// How many mono samples are held, for the tests that bound the buffer.
    pub fn len(&self) -> usize {
        self.samples.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// Runs the equalizer and taps the signal before passing it to the real sink.
pub struct Tapped {
    inner: Box<dyn Sink>,
    control: Arc<AudioControl>,
    tap: Arc<AudioTap>,
    eq: crate::eq::Processor,
    /// Player volume, used to calculate the limiter ceiling.
    volume: Box<dyn VolumeGetter + Send>,
    /// Whether this wrapper applies volume after the tap. Otherwise the inner
    /// sink applies it, still after the tap and to already queued audio.
    applies_volume: bool,
    /// Final limiter, placed here because this stage knows the output volume.
    limiter: crate::limiter::Limiter,
    /// Track normalization factor. The tap removes it so visualizers show the
    /// source dynamics, as Winamp's analyser did.
    normalisation: Arc<std::sync::atomic::AtomicU64>,
}

impl Tapped {
    pub fn new(
        inner: Box<dyn Sink>,
        control: Arc<AudioControl>,
        tap: Arc<AudioTap>,
        volume: Box<dyn VolumeGetter + Send>,
        applies_volume: bool,
        eq: crate::eq::SharedEq,
        normalisation: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            inner,
            control,
            tap,
            eq: crate::eq::Processor::new(eq),
            volume,
            applies_volume,
            limiter: crate::limiter::Limiter::new(f64::from(SAMPLE_RATE)),
            normalisation,
        }
    }
}

/// Full-scale level for samples leaving `Tapped`.
///
/// This is 1.0 after volume is applied. Before volume, it is the level that
/// becomes 1.0 after the inner sink applies volume.
pub fn full_scale(volume: f64, applied: bool) -> Option<f64> {
    if applied {
        Some(1.0)
    } else if volume > f64::EPSILON {
        Some(1.0 / volume)
    } else {
        // At zero volume, no finite pre-volume ceiling is needed.
        None
    }
}

impl Sink for Tapped {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if self.control.take_processing_reset() {
            self.eq.reset();
            self.limiter = crate::limiter::Limiter::new(f64::from(SAMPLE_RATE));
            self.tap.clear();
        }
        let packet = match packet {
            AudioPacket::Samples(mut samples) => {
                self.eq.process(&mut samples);
                // Post-EQ, pre-volume, pre-normalisation: the equalizer
                // shapes what the bars show; the volume knob and the
                // loudness housekeeping never move them.
                let factor = f64::from_bits(
                    self.normalisation
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                let restore = if factor > 0.05 && factor < 20.0 {
                    (1.0 / factor).clamp(0.125, 8.0) as f32
                } else {
                    1.0
                };
                self.tap.push(&samples, restore);
                let attenuation = self.volume.attenuation_factor();
                if self.applies_volume {
                    for sample in &mut samples {
                        *sample *= attenuation;
                    }
                }
                if let Some(full_scale) = full_scale(attenuation, self.applies_volume) {
                    self.limiter.process(&mut samples, full_scale);
                }
                AudioPacket::Samples(samples)
            }
            raw => raw,
        };
        self.inner.write(packet, converter)
    }
}
