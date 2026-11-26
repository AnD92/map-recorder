//! Writes the final, trimmed recording in the format of the selected mode.
//!
//! - `music`: 24-bit PCM WAV, original sample rate and channels (lossless).
//! - `meeting`: AAC in an `.m4a` file, mono, 48 kHz, 64 kbps. Samples are
//!   downmixed to mono into a temporary CAF, then converted with macOS's
//!   built-in `afconvert` tool.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::audio::caf::CafWriter;
use crate::capture::StreamFormat;
use crate::mode::Mode;

/// Full-scale value of a signed 24-bit sample.
const I24_MAX: f32 = 8_388_607.0;

/// Writer for the final file of a recording.
pub struct FinalWriter {
    inner: Inner,
}

enum Inner {
    /// 24-bit PCM WAV written directly.
    Music {
        writer: hound::WavWriter<BufWriter<File>>,
        path: PathBuf,
    },
    /// Mono float CAF written to a temporary file, converted on `finish`.
    Meeting {
        temp: CafWriter,
        temp_path: PathBuf,
        final_path: PathBuf,
        channels: usize,
        mono: Vec<f32>,
    },
}

/// Temporary file used before AAC conversion, e.g. `call.m4a.tmp.caf`.
fn temp_path_for(final_path: &Path) -> PathBuf {
    let name = final_path.file_name().unwrap_or_default().to_string_lossy();
    final_path.with_file_name(format!("{name}.tmp.caf"))
}

impl FinalWriter {
    pub fn create(mode: Mode, final_path: &Path, format: StreamFormat) -> Result<Self> {
        let inner = match mode {
            Mode::Music => {
                let spec = hound::WavSpec {
                    channels: format.channels,
                    sample_rate: format.sample_rate,
                    bits_per_sample: 24,
                    sample_format: hound::SampleFormat::Int,
                };
                Inner::Music {
                    writer: hound::WavWriter::create(final_path, spec)
                        .with_context(|| format!("cannot create {}", final_path.display()))?,
                    path: final_path.to_path_buf(),
                }
            }
            Mode::Meeting => {
                let temp_path = temp_path_for(final_path);
                let temp = CafWriter::create(&temp_path, format.sample_rate, 1)
                    .with_context(|| format!("cannot create {}", temp_path.display()))?;
                Inner::Meeting {
                    temp,
                    temp_path,
                    final_path: final_path.to_path_buf(),
                    channels: format.channels.max(1) as usize,
                    mono: Vec::new(),
                }
            }
        };
        Ok(Self { inner })
    }

    /// Appends interleaved samples in the recording's original layout.
    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        match &mut self.inner {
            Inner::Music { writer, .. } => {
                for &sample in interleaved {
                    writer.write_sample((sample.clamp(-1.0, 1.0) * I24_MAX).round() as i32)?;
                }
            }
            Inner::Meeting {
                temp,
                channels,
                mono,
                ..
            } => {
                // Downmix: each output sample is the average of its frame.
                mono.clear();
                mono.extend(
                    interleaved
                        .chunks(*channels)
                        .map(|frame| frame.iter().sum::<f32>() / *channels as f32),
                );
                temp.write_samples(mono)?;
            }
        }
        Ok(())
    }

    /// Completes the file and returns its path. For meetings this runs the
    /// AAC conversion; the temporary file is removed whether it succeeds or not.
    pub fn finish(self) -> Result<PathBuf> {
        match self.inner {
            Inner::Music { writer, path } => {
                writer.finalize()?;
                Ok(path)
            }
            Inner::Meeting {
                temp,
                temp_path,
                final_path,
                ..
            } => {
                let result = temp
                    .finalize()
                    .map_err(anyhow::Error::from)
                    .and_then(|()| convert_to_aac(&temp_path, &final_path));
                let _ = std::fs::remove_file(&temp_path);
                result.map(|()| final_path)
            }
        }
    }
}

/// Converts a CAF file into a mono, 48 kHz, 64 kbps AAC `.m4a` using the
/// `afconvert` tool that ships with macOS.
fn convert_to_aac(input: &Path, output: &Path) -> Result<()> {
    let result = Command::new("afconvert")
        .args(["-f", "m4af", "-d", "aac@48000", "-b", "64000", "-c", "1"])
        .arg(input)
        .arg(output)
        .output()
        .context("cannot run afconvert (it ships with macOS)")?;
    if !result.status.success() {
        if output.is_file() {
            let _ = std::fs::remove_file(output);
        }
        bail!(
            "afconvert failed to encode {}: {}",
            output.display(),
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, channels: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = amp * (i as f32 * 0.05).sin();
                vec![s; channels]
            })
            .collect()
    }

    #[test]
    fn music_writes_24bit_stereo_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.wav");
        let format = StreamFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut writer = FinalWriter::create(Mode::Music, &path, format).unwrap();
        writer.write(&tone(1_000, 2, 0.5)).unwrap();
        assert_eq!(writer.finish().unwrap(), path);

        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.bits_per_sample, 24);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(reader.len(), 2_000);
    }

    #[test]
    fn music_clamps_out_of_range_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loud.wav");
        let format = StreamFormat {
            sample_rate: 48_000,
            channels: 1,
        };
        let mut writer = FinalWriter::create(Mode::Music, &path, format).unwrap();
        writer.write(&[1.5, -1.5, 0.5]).unwrap();
        writer.finish().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let samples: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(samples, vec![8_388_607, -8_388_607, 4_194_304]);
    }

    #[test]
    fn meeting_writes_mono_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("call.m4a");
        let format = StreamFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let mut writer = FinalWriter::create(Mode::Meeting, &path, format).unwrap();
        writer.write(&tone(44_100, 2, 0.5)).unwrap();
        assert_eq!(writer.finish().unwrap(), path);

        let out = Command::new("afinfo").arg(&path).output().unwrap();
        let info = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(info.contains("aac"), "{info}");
        assert!(info.contains("1 ch"), "{info}");
        assert!(info.contains("48000"), "{info}");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("call.m4a")]);
    }

    #[test]
    fn meeting_cleans_temp_on_afconvert_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("call.m4a");
        let format = StreamFormat {
            sample_rate: 48_000,
            channels: 1,
        };
        let mut writer = FinalWriter::create(Mode::Meeting, &path, format).unwrap();
        writer.write(&tone(4_800, 1, 0.5)).unwrap();
        // Make the destination unwritable: a directory with the output's name.
        std::fs::create_dir(&path).unwrap();
        assert!(writer.finish().is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("call.m4a")]);
    }
}
