//! Writes the final, trimmed recording in the selected format.
//!
//! - `Format::Wav`: 24-bit PCM WAV at the original sample rate (lossless).
//! - `Format::M4a`: AAC in an `.m4a` file, 48 kHz. Samples go to a temporary
//!   float CAF, then macOS's built-in `afconvert` tool encodes it.
//!
//! The mode picks channels and AAC quality: `music` keeps the original
//! channels (256 kbps AAC); `meeting` downmixes to mono (64 kbps AAC).
//!
//! The file is written under a `.part` name and renamed when complete, so a
//! file with the final name is always a finished one.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::audio::caf::CafWriter;
use crate::capture::StreamFormat;
use crate::mode::{Format, Mode};

/// Full-scale value of a signed 24-bit sample.
const I24_MAX: f32 = 8_388_607.0;

/// Sample rate of every AAC file. Apple's encoder rejects high bitrates at
/// low rates (e.g. 256 kbps at 22.05 kHz), and 48 kHz suits any source.
const AAC_SAMPLE_RATE: u32 = 48_000;

/// AAC encoding parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacSettings {
    /// Target bitrate in bits per second.
    pub bitrate: u32,
    /// Downmix to a single channel.
    pub mono: bool,
}

/// AAC quality for a mode: transparent stereo for music, compact mono for
/// speech.
pub fn aac_settings(mode: Mode) -> AacSettings {
    match mode {
        Mode::Music => AacSettings {
            bitrate: 256_000,
            mono: false,
        },
        Mode::Meeting => AacSettings {
            bitrate: 64_000,
            mono: true,
        },
    }
}

/// Writer for the final file of a recording.
pub struct FinalWriter {
    /// Channels of the incoming interleaved samples.
    in_channels: usize,
    /// Average each frame into one sample before writing.
    downmix: bool,
    /// Reused buffer for downmixed samples.
    mono: Vec<f32>,
    sink: Sink,
}

/// Destination of the samples.
enum Sink {
    /// 24-bit PCM WAV written directly to the `.part` file.
    Wav {
        writer: hound::WavWriter<BufWriter<File>>,
        final_path: PathBuf,
    },
    /// Float CAF written to a temporary file, encoded to AAC on `finish`.
    Aac {
        temp: CafWriter,
        temp_path: PathBuf,
        final_path: PathBuf,
        settings: AacSettings,
    },
}

/// Work-in-progress files that may exist next to `final_path` while it is
/// being written: the `.part` file renamed into place on success and the
/// temporary CAF used for AAC conversion.
pub fn partial_paths_for(final_path: &Path) -> [PathBuf; 2] {
    [part_path_for(final_path), temp_path_for(final_path)]
}

/// File the final output is written to before being renamed into place,
/// e.g. `song.wav.part`.
pub(crate) fn part_path_for(final_path: &Path) -> PathBuf {
    let name = final_path.file_name().unwrap_or_default().to_string_lossy();
    final_path.with_file_name(format!("{name}.part"))
}

/// Moves a completed `.part` file to its final name; removes it on failure.
pub(crate) fn publish(part: &Path, final_path: &Path) -> Result<()> {
    std::fs::rename(part, final_path).map_err(|error| {
        let _ = std::fs::remove_file(part);
        anyhow::Error::from(error).context(format!("cannot create {}", final_path.display()))
    })
}

/// Temporary file used before AAC conversion, e.g. `call.m4a.tmp.caf`.
fn temp_path_for(final_path: &Path) -> PathBuf {
    let name = final_path.file_name().unwrap_or_default().to_string_lossy();
    final_path.with_file_name(format!("{name}.tmp.caf"))
}

impl FinalWriter {
    pub fn create(
        mode: Mode,
        format: Format,
        final_path: &Path,
        stream: StreamFormat,
    ) -> Result<Self> {
        let in_channels = stream.channels.max(1);
        let downmix = mode == Mode::Meeting && in_channels > 1;
        let out_channels = if downmix { 1 } else { in_channels };
        let sink = match format {
            Format::Wav => {
                let spec = hound::WavSpec {
                    channels: out_channels,
                    sample_rate: stream.sample_rate,
                    bits_per_sample: 24,
                    sample_format: hound::SampleFormat::Int,
                };
                let part = part_path_for(final_path);
                Sink::Wav {
                    writer: hound::WavWriter::create(&part, spec)
                        .with_context(|| format!("cannot create {}", part.display()))?,
                    final_path: final_path.to_path_buf(),
                }
            }
            Format::M4a => {
                let temp_path = temp_path_for(final_path);
                let temp = CafWriter::create(&temp_path, stream.sample_rate, out_channels)
                    .with_context(|| format!("cannot create {}", temp_path.display()))?;
                Sink::Aac {
                    temp,
                    temp_path,
                    final_path: final_path.to_path_buf(),
                    settings: aac_settings(mode),
                }
            }
        };
        Ok(Self {
            in_channels: in_channels as usize,
            downmix,
            mono: Vec::new(),
            sink,
        })
    }

    /// Appends interleaved samples in the recording's original layout.
    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        let samples = if self.downmix {
            // Each output sample is the average of its frame.
            let channels = self.in_channels;
            self.mono.clear();
            self.mono.extend(
                interleaved
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32),
            );
            &self.mono[..]
        } else {
            interleaved
        };
        match &mut self.sink {
            Sink::Wav { writer, .. } => {
                for &sample in samples {
                    writer.write_sample((sample.clamp(-1.0, 1.0) * I24_MAX).round() as i32)?;
                }
            }
            Sink::Aac { temp, .. } => temp.write_samples(samples)?,
        }
        Ok(())
    }

    /// Completes the file and returns its path. For AAC this runs the
    /// conversion; the temporary file is removed whether it succeeds or not.
    pub fn finish(self) -> Result<PathBuf> {
        match self.sink {
            Sink::Wav { writer, final_path } => {
                writer.finalize()?;
                publish(&part_path_for(&final_path), &final_path)?;
                Ok(final_path)
            }
            Sink::Aac {
                temp,
                temp_path,
                final_path,
                settings,
            } => {
                let part = part_path_for(&final_path);
                let result = temp
                    .finalize()
                    .map_err(anyhow::Error::from)
                    .and_then(|()| convert_to_aac(&temp_path, &part, settings))
                    .and_then(|()| publish(&part, &final_path));
                let _ = std::fs::remove_file(&temp_path);
                result.map(|()| final_path)
            }
        }
    }
}

/// Encodes any audio file macOS can read into a 48 kHz AAC `.m4a` using the
/// `afconvert` tool that ships with macOS. Removes `output` on failure.
pub fn convert_to_aac(input: &Path, output: &Path, settings: AacSettings) -> Result<()> {
    let mut command = Command::new("afconvert");
    command
        .args(["-f", "m4af", "-d"])
        .arg(format!("aac@{AAC_SAMPLE_RATE}"))
        .arg("-b")
        .arg(settings.bitrate.to_string());
    if settings.mono {
        command.args(["-c", "1"]);
    }
    let result = command
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
        let mut writer = FinalWriter::create(Mode::Music, Format::Wav, &path, format).unwrap();
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
    fn final_name_appears_only_when_finished() {
        let dir = tempfile::tempdir().unwrap();
        for (mode, format_, name) in [
            (Mode::Music, Format::Wav, "a.wav"),
            (Mode::Music, Format::M4a, "b.m4a"),
            (Mode::Meeting, Format::M4a, "c.m4a"),
            (Mode::Meeting, Format::Wav, "d.wav"),
        ] {
            let path = dir.path().join(name);
            let format = StreamFormat {
                sample_rate: 48_000,
                channels: 2,
            };
            let mut writer = FinalWriter::create(mode, format_, &path, format).unwrap();
            writer.write(&tone(4_800, 2, 0.5)).unwrap();
            assert!(!path.exists(), "{name} must not exist before finish");
            writer.finish().unwrap();
            assert!(path.is_file());
            for partial in partial_paths_for(&path) {
                assert!(!partial.exists(), "{} left behind", partial.display());
            }
        }
    }

    #[test]
    fn music_clamps_out_of_range_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loud.wav");
        let format = StreamFormat {
            sample_rate: 48_000,
            channels: 1,
        };
        let mut writer = FinalWriter::create(Mode::Music, Format::Wav, &path, format).unwrap();
        writer.write(&[1.5, -1.5, 0.5]).unwrap();
        writer.finish().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let samples: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(samples, vec![8_388_607, -8_388_607, 4_194_304]);
    }

    /// `afinfo` output for `path`, lowercased.
    fn afinfo(path: &Path) -> String {
        let out = Command::new("afinfo").arg(path).output().unwrap();
        String::from_utf8_lossy(&out.stdout).to_lowercase()
    }

    fn dir_names(dir: &Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect()
    }

    #[test]
    fn music_m4a_is_stereo_aac_48k() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.m4a");
        let format = StreamFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let mut writer = FinalWriter::create(Mode::Music, Format::M4a, &path, format).unwrap();
        writer.write(&tone(44_100, 2, 0.5)).unwrap();
        assert_eq!(writer.finish().unwrap(), path);
        let info = afinfo(&path);
        assert!(info.contains("aac"), "{info}");
        assert!(info.contains("2 ch"), "{info}");
        assert!(info.contains("48000"), "{info}");
        assert_eq!(
            dir_names(dir.path()),
            vec![std::ffi::OsString::from("song.m4a")]
        );
    }

    #[test]
    fn music_m4a_keeps_mono_source_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.m4a");
        let format = StreamFormat {
            sample_rate: 44_100,
            channels: 1,
        };
        let mut writer = FinalWriter::create(Mode::Music, Format::M4a, &path, format).unwrap();
        writer.write(&tone(44_100, 1, 0.5)).unwrap();
        writer.finish().unwrap();
        let info = afinfo(&path);
        assert!(info.contains("1 ch"), "{info}");
        assert!(info.contains("48000"), "{info}");
    }

    #[test]
    fn meeting_wav_is_mono_24bit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("call.wav");
        let format = StreamFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut writer = FinalWriter::create(Mode::Meeting, Format::Wav, &path, format).unwrap();
        writer.write(&tone(1_000, 2, 0.5)).unwrap();
        writer.finish().unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 24);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(reader.len(), 1_000);
    }

    #[test]
    fn aac_settings_per_mode() {
        assert_eq!(
            aac_settings(Mode::Music),
            AacSettings {
                bitrate: 256_000,
                mono: false
            }
        );
        assert_eq!(
            aac_settings(Mode::Meeting),
            AacSettings {
                bitrate: 64_000,
                mono: true
            }
        );
    }

    #[test]
    fn meeting_writes_mono_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("call.m4a");
        let format = StreamFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let mut writer = FinalWriter::create(Mode::Meeting, Format::M4a, &path, format).unwrap();
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
        let mut writer = FinalWriter::create(Mode::Meeting, Format::M4a, &path, format).unwrap();
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
