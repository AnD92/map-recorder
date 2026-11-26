//! Silence detection: decides which part of a recording to keep.
//!
//! The signal is measured in short windows; runs of loud-enough windows form
//! "sound events". The kept range starts at most `max_lead_secs` before the
//! first event and ends `tail_pad_secs` after the last one. This module is
//! pure computation (no I/O, no platform code) so it can be tested with
//! synthetic signals.

use crate::mode::Settings;

/// Length of one analysis window.
pub const WINDOW_SECS: f64 = 0.010;
/// Minimum duration of consecutive loud windows to count as sound
/// (filters out isolated clicks and pops).
pub const MIN_EVENT_SECS: f64 = 0.050;
/// Length of the linear fade applied at each cut point to avoid clicks.
pub const FADE_SECS: f64 = 0.005;

/// Streaming loudness meter: accepts interleaved samples in chunks of any
/// size and produces one RMS level (dBFS) per window.
pub struct LoudnessAnalyzer {
    /// Samples (all channels) that make up one window.
    window_samples: usize,
    /// Running sum of squares and sample count of the current window.
    sum_squares: f64,
    count: usize,
    levels: Vec<f32>,
}

/// Number of frames in one analysis window at `sample_rate`.
pub fn window_frames(sample_rate: u32) -> u64 {
    ((sample_rate as f64 * WINDOW_SECS).round() as u64).max(1)
}

impl LoudnessAnalyzer {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            window_samples: window_frames(sample_rate) as usize * channels.max(1) as usize,
            sum_squares: 0.0,
            count: 0,
            levels: Vec::new(),
        }
    }

    /// Accumulates samples; window boundaries are tracked across calls, so
    /// the result does not depend on how the signal is chunked.
    pub fn push(&mut self, interleaved: &[f32]) {
        for &sample in interleaved {
            self.sum_squares += sample as f64 * sample as f64;
            self.count += 1;
            if self.count == self.window_samples {
                self.close_window();
            }
        }
    }

    /// Returns one dBFS level per window, including a trailing partial
    /// window. Windows with zero energy are `f32::NEG_INFINITY`.
    pub fn finish(mut self) -> Vec<f32> {
        if self.count > 0 {
            self.close_window();
        }
        self.levels
    }

    fn close_window(&mut self) {
        let rms = (self.sum_squares / self.count as f64).sqrt();
        let db = if rms > 0.0 {
            (20.0 * rms.log10()) as f32
        } else {
            f32::NEG_INFINITY
        };
        self.levels.push(db);
        self.sum_squares = 0.0;
        self.count = 0;
    }
}

/// Frame range to keep; `end_frame` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimRange {
    pub start_frame: u64,
    pub end_frame: u64,
}

pub fn find_trim_range(
    window_db: &[f32],
    total_frames: u64,
    sample_rate: u32,
    settings: &Settings,
) -> Option<TrimRange> {
    let window = window_frames(sample_rate);
    let min_windows = (MIN_EVENT_SECS / WINDOW_SECS).round() as usize;

    // Locate the first and last runs of loud windows that are long enough
    // to be real sound rather than an isolated click.
    let mut first_start: Option<usize> = None;
    let mut last_end = 0usize;
    let mut run_start = 0usize;
    let mut run_len = 0usize;
    for (index, &db) in window_db.iter().enumerate() {
        if db >= settings.threshold_db {
            if run_len == 0 {
                run_start = index;
            }
            run_len += 1;
            if run_len >= min_windows {
                first_start.get_or_insert(run_start);
                last_end = index + 1;
            }
        } else {
            run_len = 0;
        }
    }
    let first_start = first_start?;

    // Convert window positions to frames and apply lead/tail allowances.
    let to_frames = |secs: f32| (secs as f64 * sample_rate as f64).round() as u64;
    let first_sound = first_start as u64 * window;
    let last_sound = (last_end as u64 * window).min(total_frames);
    Some(TrimRange {
        start_frame: first_sound.saturating_sub(to_frames(settings.max_lead_secs)),
        end_frame: (last_sound + to_frames(settings.tail_pad_secs)).min(total_frames),
    })
}

/// Gain for frame `i` (0-based within a kept range of `range_len` frames):
/// a linear ramp up over the first `fade_frames`, down over the last
/// `fade_frames`, and 1.0 in between.
pub fn fade_gain(i: u64, range_len: u64, fade_frames: u64) -> f32 {
    if fade_frames == 0 {
        return 1.0;
    }
    let from_start = i;
    let from_end = range_len.saturating_sub(i + 1);
    let distance = from_start.min(from_end);
    if distance >= fade_frames {
        1.0
    } else {
        distance as f32 / fade_frames as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    const SR: u32 = 48_000;

    /// Stereo 440 Hz sine with peak amplitude `amp`.
    fn tone(secs: f64, amp: f32) -> Vec<f32> {
        let frames = (secs * SR as f64).round() as usize;
        (0..frames)
            .flat_map(|i| {
                let s = amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SR as f32).sin();
                [s, s]
            })
            .collect()
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * SR as f64).round() as usize * 2]
    }

    fn concat(parts: &[Vec<f32>]) -> Vec<f32> {
        parts.concat()
    }

    fn range_for(signal: &[f32], mode: Mode) -> Option<TrimRange> {
        let mut analyzer = LoudnessAnalyzer::new(SR, 2);
        analyzer.push(signal);
        let windows = analyzer.finish();
        find_trim_range(&windows, (signal.len() / 2) as u64, SR, &mode.defaults())
    }

    fn secs(s: f64) -> u64 {
        (s * SR as f64).round() as u64
    }

    #[test]
    fn long_lead_is_cut_to_two_seconds_and_tail_to_half_second() {
        let signal = concat(&[silence(5.0), tone(3.0, 0.5), silence(4.0)]);
        assert_eq!(
            range_for(&signal, Mode::Music),
            Some(TrimRange {
                start_frame: secs(3.0),
                end_frame: secs(8.5)
            })
        );
    }

    #[test]
    fn short_lead_is_kept_entirely() {
        let signal = concat(&[silence(1.0), tone(2.0, 0.5), silence(2.0)]);
        assert_eq!(range_for(&signal, Mode::Music).unwrap().start_frame, 0);
    }

    #[test]
    fn isolated_click_in_tail_is_ignored() {
        let signal = concat(&[tone(2.0, 0.5), silence(3.0), tone(0.02, 0.9), silence(2.0)]);
        assert_eq!(
            range_for(&signal, Mode::Music).unwrap().end_frame,
            secs(2.5)
        );
    }

    #[test]
    fn quiet_fade_counts_as_sound_in_music_but_not_meeting() {
        // amplitude 0.0018 → RMS ≈ -57 dBFS: above -60, below -40.
        let signal = concat(&[tone(2.0, 0.5), tone(2.0, 0.0018), silence(2.0)]);
        assert_eq!(
            range_for(&signal, Mode::Music).unwrap().end_frame,
            secs(4.5)
        );
        assert_eq!(
            range_for(&signal, Mode::Meeting).unwrap().end_frame,
            secs(2.5)
        );
    }

    #[test]
    fn all_silence_returns_none() {
        assert_eq!(range_for(&silence(3.0), Mode::Music), None);
    }

    #[test]
    fn zero_frames_returns_none() {
        let analyzer = LoudnessAnalyzer::new(SR, 2);
        let windows = analyzer.finish();
        assert!(windows.is_empty());
        assert_eq!(
            find_trim_range(&windows, 0, SR, &Mode::Music.defaults()),
            None
        );
    }

    #[test]
    fn sound_until_the_end_keeps_the_end() {
        let signal = concat(&[silence(1.0), tone(2.0, 0.5)]);
        assert_eq!(
            range_for(&signal, Mode::Music).unwrap().end_frame,
            secs(3.0)
        );
    }

    #[test]
    fn sound_in_one_channel_only_is_detected() {
        let mut signal = concat(&[silence(3.0), tone(1.0, 0.5), silence(1.0)]);
        for frame in signal.chunks_mut(2) {
            frame[1] = 0.0;
        }
        assert_eq!(
            range_for(&signal, Mode::Music),
            Some(TrimRange {
                start_frame: secs(1.0),
                end_frame: secs(4.5)
            })
        );
    }

    #[test]
    fn chunking_does_not_change_result() {
        let signal = concat(&[silence(0.5), tone(0.7, 0.3), silence(0.4)]);
        let mut whole = LoudnessAnalyzer::new(SR, 2);
        whole.push(&signal);
        let mut chunked = LoudnessAnalyzer::new(SR, 2);
        for chunk in signal.chunks(333) {
            chunked.push(chunk);
        }
        assert_eq!(whole.finish(), chunked.finish());
    }

    #[test]
    fn fade_gain_ramps() {
        assert_eq!(fade_gain(0, 1000, 240), 0.0);
        assert_eq!(fade_gain(500, 1000, 240), 1.0);
        assert!(fade_gain(999, 1000, 240) < 0.01);
        assert!(fade_gain(120, 1000, 240) > 0.4 && fade_gain(120, 1000, 240) < 0.6);
    }
}
