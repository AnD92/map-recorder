//! System audio capture.
//!
//! This module exposes a small, safe API on top of Core Audio *process taps*
//! (macOS 14.2+). A tap mirrors everything the Mac plays; we read it through a
//! private aggregate device and hand the samples to a caller-supplied callback.
//!
//! All `unsafe` code of the project lives in [`tap`]. See
//! `docs/core-audio-tap.md` for a step-by-step explanation.

mod tap;

/// Sample rate and channel layout of the captured stream.
///
/// Samples delivered by [`SystemAudioTap`] are always interleaved `f32`
/// in the range `[-1.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Errors that can occur while setting up or running the capture.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("macOS 14.2 or later is required")]
    UnsupportedOs,
    #[error("Core Audio call `{call}` failed with status {status}")]
    CoreAudio { call: &'static str, status: i32 },
    #[error("unsupported tap stream format: {0}")]
    UnsupportedFormat(String),
}

/// A running capture of all system audio output.
///
/// Capture starts in [`SystemAudioTap::start`] and stops when the value is
/// dropped; every Core Audio object created along the way is released.
pub struct SystemAudioTap {
    inner: tap::TapSession,
}

impl SystemAudioTap {
    /// Starts capturing system audio.
    ///
    /// `on_samples` runs on Core Audio's **real-time thread** and receives
    /// interleaved `f32` frames. It must not allocate, lock, block or do I/O:
    /// copy the samples somewhere (e.g. a lock-free ring buffer) and return.
    pub fn start<F>(on_samples: F) -> Result<Self, CaptureError>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        Ok(Self {
            inner: tap::TapSession::start(Box::new(on_samples))?,
        })
    }

    /// Format of the samples passed to the callback.
    pub fn format(&self) -> StreamFormat {
        self.inner.format()
    }
}
