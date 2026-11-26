//! Recording modes and their tuning presets.

use std::path::{Path, PathBuf};

/// What is being recorded. The mode picks silence sensitivity and file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Music: strict silence detection, lossless WAV output.
    Music,
    /// Meetings / speech: lenient silence detection, compact AAC output.
    Meeting,
}

/// Parameters of the silence trimming.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Windows quieter than this (dBFS) count as silence.
    pub threshold_db: f32,
    /// Maximum silence kept before the first sound, in seconds.
    pub max_lead_secs: f32,
    /// Silence kept after the last sound, in seconds.
    pub tail_pad_secs: f32,
}

impl Mode {
    /// Default trimming parameters. Music uses a strict threshold so quiet
    /// passages and fade-outs survive; meetings use a lenient one so room
    /// noise counts as silence.
    pub fn defaults(self) -> Settings {
        let threshold_db = match self {
            Mode::Music => -60.0,
            Mode::Meeting => -40.0,
        };
        Settings {
            threshold_db,
            max_lead_secs: 2.0,
            tail_pad_secs: 0.5,
        }
    }

    /// Container of the untrimmed capture. Meetings use CAF because it has
    /// no 4 GB size limit, so long sessions are safe.
    pub fn raw_extension(self) -> &'static str {
        match self {
            Mode::Music => "wav",
            Mode::Meeting => "caf",
        }
    }

    /// Extension of the final, trimmed file.
    pub fn output_extension(self) -> &'static str {
        match self {
            Mode::Music => "wav",
            Mode::Meeting => "m4a",
        }
    }
}

/// Path of the untrimmed capture that sits next to the final output,
/// e.g. `out/recording-1.wav` + music → `out/recording-1.raw.wav`.
pub fn raw_path_for(output: &Path, mode: Mode) -> PathBuf {
    let stem = output.file_stem().unwrap_or_default().to_string_lossy();
    output.with_file_name(format!("{stem}.raw.{}", mode.raw_extension()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_per_mode() {
        assert_eq!(
            Mode::Music.defaults(),
            Settings {
                threshold_db: -60.0,
                max_lead_secs: 2.0,
                tail_pad_secs: 0.5
            }
        );
        assert_eq!(
            Mode::Meeting.defaults(),
            Settings {
                threshold_db: -40.0,
                max_lead_secs: 2.0,
                tail_pad_secs: 0.5
            }
        );
    }

    #[test]
    fn extensions_per_mode() {
        assert_eq!(Mode::Music.raw_extension(), "wav");
        assert_eq!(Mode::Meeting.raw_extension(), "caf");
        assert_eq!(Mode::Music.output_extension(), "wav");
        assert_eq!(Mode::Meeting.output_extension(), "m4a");
    }

    #[test]
    fn raw_path_naming() {
        assert_eq!(
            raw_path_for(Path::new("out/rec.wav"), Mode::Music),
            PathBuf::from("out/rec.raw.wav")
        );
        assert_eq!(
            raw_path_for(Path::new("rec.m4a"), Mode::Meeting),
            PathBuf::from("rec.raw.caf")
        );
    }
}
