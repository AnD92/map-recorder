//! Converts existing audio files to compact AAC `.m4a`, without trimming.
//!
//! The input goes straight to `afconvert`, so any format macOS can read works
//! (WAV, AIFF, CAF, FLAC, MP3, …). The original file is never modified.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cli::ensure_absent;
use crate::encode::{aac_settings, convert_to_aac, part_path_for, publish};
use crate::mode::Mode;

/// Where `convert` writes when no `-o` is given: next to the input, with the
/// `.m4a` extension (`song.wav` → `song.m4a`).
pub fn default_converted_path(input: &Path) -> PathBuf {
    input.with_extension("m4a")
}

/// Encodes `input` into `output` as AAC with the quality of `mode`.
///
/// Refuses to overwrite `output` and requires an `.m4a` name. The result is
/// written under a `.part` name and renamed when complete, so nothing is left
/// behind on failure.
pub fn convert_file(input: &Path, output: &Path, mode: Mode) -> Result<()> {
    if !input.is_file() {
        bail!("cannot read {}: no such file", input.display());
    }
    let is_m4a = output
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m4a"));
    if !is_m4a {
        bail!("output file {} must end in .m4a", output.display());
    }
    ensure_absent(output)?;
    let part = part_path_for(output);
    convert_to_aac(input, &part, aac_settings(mode))?;
    publish(&part, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Writes 1 s of a 16-bit tone.
    fn write_wav(path: &Path, sample_rate: u32, channels: u16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..sample_rate {
            let s = (8_000.0 * (i as f32 * 0.05).sin()) as i16;
            for _ in 0..channels {
                writer.write_sample(s).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    fn afinfo(path: &Path) -> String {
        let out = Command::new("afinfo").arg(path).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_lowercase()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn default_converted_path_swaps_extension() {
        assert_eq!(
            default_converted_path(Path::new("a/song.wav")),
            PathBuf::from("a/song.m4a")
        );
    }

    #[test]
    fn convert_file_music_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.wav");
        let output = dir.path().join("song.m4a");
        write_wav(&input, 44_100, 2);
        let before = std::fs::read(&input).unwrap();
        convert_file(&input, &output, Mode::Music).unwrap();
        let info = afinfo(&output);
        assert!(info.contains("aac"), "{info}");
        assert!(info.contains("2 ch"), "{info}");
        assert!(info.contains("48000"), "{info}");
        assert_eq!(std::fs::read(&input).unwrap(), before, "input modified");
        assert_eq!(names(dir.path()), ["song.m4a", "song.wav"]);
    }

    #[test]
    fn convert_file_meeting_is_mono() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("call.wav");
        let output = dir.path().join("call.m4a");
        write_wav(&input, 48_000, 2);
        convert_file(&input, &output, Mode::Meeting).unwrap();
        let info = afinfo(&output);
        assert!(info.contains("1 ch"), "{info}");
    }

    #[test]
    fn convert_file_refuses_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.wav");
        let output = dir.path().join("song.m4a");
        write_wav(&input, 48_000, 2);
        std::fs::write(&output, b"keep me").unwrap();
        let err = convert_file(&input, &output, Mode::Music)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
    }

    #[test]
    fn convert_file_rejects_non_m4a_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.wav");
        write_wav(&input, 48_000, 2);
        let output = dir.path().join("song.mp3");
        let err = convert_file(&input, &output, Mode::Music)
            .unwrap_err()
            .to_string();
        assert!(err.contains(".m4a"), "{err}");
        assert!(!output.exists());
    }

    #[test]
    fn convert_file_missing_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("missing.wav");
        let output = dir.path().join("missing.m4a");
        let err = convert_file(&input, &output, Mode::Music)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing.wav"), "{err}");
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn convert_file_unreadable_input_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("notes.wav");
        std::fs::write(&input, b"not audio").unwrap();
        let output = dir.path().join("notes.m4a");
        assert!(convert_file(&input, &output, Mode::Music).is_err());
        assert_eq!(names(dir.path()), ["notes.wav"]);
    }
}
