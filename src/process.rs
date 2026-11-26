//! Post-processing of a raw capture: finds the sound and writes the trimmed
//! final file.
//!
//! Two streaming passes keep memory use flat for recordings of any length:
//! pass 1 measures loudness window by window; pass 2 copies only the kept
//! range into the final file, fading in/out at the cut points.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::audio::io::RawReader;
use crate::encode::FinalWriter;
use crate::mode::{Mode, Settings};
use crate::trim::{FADE_SECS, LoudnessAnalyzer, fade_gain, find_trim_range};

/// Result of trimming a recording.
#[derive(Debug, PartialEq)]
pub enum TrimOutcome {
    /// The final file was written.
    Written {
        path: PathBuf,
        kept_secs: f64,
        removed_lead_secs: f64,
        removed_tail_secs: f64,
    },
    /// No sound above the threshold: nothing was written.
    NoSound,
}

/// Frames processed per read in both passes.
pub const CHUNK_FRAMES: usize = 4096;

/// Trims `input` according to `settings` and writes the result to `output`
/// in the format of `mode`. Writes nothing when no sound is found.
pub fn trim_file(
    input: &Path,
    output: &Path,
    mode: Mode,
    settings: &Settings,
) -> Result<TrimOutcome> {
    let mut buf = Vec::with_capacity(CHUNK_FRAMES * 2);

    // Pass 1: measure the loudness of the whole recording.
    let mut reader = RawReader::open(input)?;
    let format = reader.format();
    let total_frames = reader.total_frames();
    let mut analyzer = LoudnessAnalyzer::new(format.sample_rate, format.channels);
    while reader.read_frames(&mut buf, CHUNK_FRAMES)? > 0 {
        analyzer.push(&buf);
    }
    let levels = analyzer.finish();
    let Some(range) = find_trim_range(&levels, total_frames, format.sample_rate, settings) else {
        return Ok(TrimOutcome::NoSound);
    };

    // Pass 2: skip the leading part, then copy the kept range with fades.
    let channels = format.channels.max(1) as usize;
    let fade_frames = (format.sample_rate as f64 * FADE_SECS).round() as u64;
    let kept_frames = range.end_frame - range.start_frame;
    let mut reader = RawReader::open(input)?;
    let mut writer = FinalWriter::create(mode, output, format)?;
    let mut position = 0u64; // index of the first frame in `buf`
    loop {
        let read = reader.read_frames(&mut buf, CHUNK_FRAMES)? as u64;
        if read == 0 || position >= range.end_frame {
            break;
        }
        let first = range.start_frame.max(position);
        let last = range.end_frame.min(position + read);
        if first < last {
            let slice = &mut buf
                [((first - position) as usize * channels)..((last - position) as usize * channels)];
            for (offset, frame) in slice.chunks_mut(channels).enumerate() {
                let gain = fade_gain(
                    first - range.start_frame + offset as u64,
                    kept_frames,
                    fade_frames,
                );
                for sample in frame {
                    *sample *= gain;
                }
            }
            writer.write(slice)?;
        }
        position += read;
    }
    let path = writer.finish()?;

    let rate = format.sample_rate as f64;
    Ok(TrimOutcome::Written {
        path,
        kept_secs: kept_frames as f64 / rate,
        removed_lead_secs: range.start_frame as f64 / rate,
        removed_tail_secs: (total_frames - range.end_frame) as f64 / rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::io::RawWriter;
    use crate::capture::StreamFormat;

    const SR: u32 = 48_000;

    fn tone(secs: f64) -> Vec<f32> {
        let frames = (secs * SR as f64).round() as usize;
        (0..frames)
            .flat_map(|i| {
                let s = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SR as f32).sin();
                [s, s]
            })
            .collect()
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * SR as f64).round() as usize * 2]
    }

    fn write_raw(path: &Path, samples: &[f32]) {
        let format = StreamFormat {
            sample_rate: SR,
            channels: 2,
        };
        let mut writer = RawWriter::create(path, format).unwrap();
        writer.write(samples).unwrap();
        writer.finalize().unwrap();
    }

    fn standard_signal() -> Vec<f32> {
        [silence(5.0), tone(3.0), silence(4.0)].concat()
    }

    #[test]
    fn music_trims_lead_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("r.raw.wav");
        let out = dir.path().join("r.wav");
        write_raw(&raw, &standard_signal());

        let outcome = trim_file(&raw, &out, Mode::Music, &Mode::Music.defaults()).unwrap();
        let TrimOutcome::Written {
            path,
            kept_secs,
            removed_lead_secs,
            removed_tail_secs,
        } = outcome
        else {
            panic!("expected Written, got {outcome:?}");
        };
        assert_eq!(path, out);
        assert!((kept_secs - 5.5).abs() < 1e-6, "{kept_secs}");
        assert!(
            (removed_lead_secs - 3.0).abs() < 1e-6,
            "{removed_lead_secs}"
        );
        assert!(
            (removed_tail_secs - 3.5).abs() < 1e-6,
            "{removed_tail_secs}"
        );
        let reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.duration(), 5 * SR + SR / 2);
    }

    #[test]
    fn cut_points_are_faded() {
        // Sound right from the start: the first kept sample is a cut point.
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("f.raw.wav");
        let out = dir.path().join("f.wav");
        let signal: Vec<f32> = [vec![0.5f32; SR as usize * 2], silence(2.0)].concat();
        write_raw(&raw, &signal);
        trim_file(&raw, &out, Mode::Music, &Mode::Music.defaults()).unwrap();

        let mut reader = hound::WavReader::open(&out).unwrap();
        let samples: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(samples[0], 0, "fade-in starts at zero");
        let mid = (SR as usize / 2) * 2;
        assert_eq!(samples[mid], 4_194_304, "middle is not attenuated");
        assert_eq!(*samples.last().unwrap(), 0, "fade-out ends at zero");
    }

    #[test]
    fn meeting_from_caf_produces_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("m.raw.caf");
        let out = dir.path().join("m.m4a");
        write_raw(&raw, &standard_signal());
        let outcome = trim_file(&raw, &out, Mode::Meeting, &Mode::Meeting.defaults()).unwrap();
        assert!(
            matches!(outcome, TrimOutcome::Written { .. }),
            "{outcome:?}"
        );
        assert!(out.is_file());
    }

    #[test]
    fn silent_input_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("s.raw.wav");
        let out = dir.path().join("s.wav");
        write_raw(&raw, &silence(3.0));
        let outcome = trim_file(&raw, &out, Mode::Music, &Mode::Music.defaults()).unwrap();
        assert_eq!(outcome, TrimOutcome::NoSound);
        assert!(!out.exists());
    }

    #[test]
    fn empty_input_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("e.raw.caf");
        let out = dir.path().join("e.m4a");
        write_raw(&raw, &[]);
        let outcome = trim_file(&raw, &out, Mode::Meeting, &Mode::Meeting.defaults()).unwrap();
        assert_eq!(outcome, TrimOutcome::NoSound);
        assert!(!out.exists());
    }
}
