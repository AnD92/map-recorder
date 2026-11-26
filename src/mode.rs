//! Recording modes and their tuning presets.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// What is being recorded. The mode picks silence sensitivity and file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Music: strict silence detection, lossless WAV output.
    Music,
    /// Meetings / speech: lenient silence detection, compact AAC output.
    Meeting,
}

/// Container and codec of the final, trimmed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Compact AAC in an .m4a file; plays on phones, Macs and music apps.
    M4a,
    /// Lossless 24-bit PCM WAV.
    Wav,
}

impl Format {
    /// File extension used for this format, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::M4a => "m4a",
            Format::Wav => "wav",
        }
    }

    /// The format whose extension matches `path` (case-insensitive), if any.
    fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        [Format::M4a, Format::Wav]
            .into_iter()
            .find(|format| format.extension() == ext)
    }
}

/// Picks the final format from the `--format` flag and the `-o` path.
///
/// Without `-o` the flag decides (default m4a). With `-o` its extension must
/// be `.wav` or `.m4a`: it decides when no flag is given and must agree with
/// the flag otherwise, so the file name never lies about its content.
pub fn resolve_format(flag: Option<Format>, output: Option<&Path>) -> Result<Format> {
    let Some(output) = output else {
        return Ok(flag.unwrap_or(Format::M4a));
    };
    let Some(from_name) = Format::from_path(output) else {
        bail!("output file {} must end in .wav or .m4a", output.display());
    };
    match flag {
        Some(flag) if flag != from_name => {
            let ext = flag.extension();
            bail!(
                "--format {ext} does not match the output file {}; use a .{ext} name or drop --format",
                output.display()
            )
        }
        _ => Ok(from_name),
    }
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
    fn format_extensions() {
        assert_eq!(Format::M4a.extension(), "m4a");
        assert_eq!(Format::Wav.extension(), "wav");
    }

    #[test]
    fn resolve_format_defaults_to_m4a() {
        assert_eq!(resolve_format(None, None).unwrap(), Format::M4a);
    }

    #[test]
    fn resolve_format_flag_without_output() {
        assert_eq!(
            resolve_format(Some(Format::Wav), None).unwrap(),
            Format::Wav
        );
    }

    #[test]
    fn resolve_format_from_output_extension_any_case() {
        assert_eq!(
            resolve_format(None, Some(Path::new("a/Song.WAV"))).unwrap(),
            Format::Wav
        );
        assert_eq!(
            resolve_format(None, Some(Path::new("call.m4a"))).unwrap(),
            Format::M4a
        );
    }

    #[test]
    fn resolve_format_rejects_unknown_extension() {
        let err = resolve_format(None, Some(Path::new("take.mp3")))
            .unwrap_err()
            .to_string();
        assert!(err.contains(".wav") && err.contains(".m4a"), "{err}");
        assert!(resolve_format(None, Some(Path::new("take"))).is_err());
    }

    #[test]
    fn resolve_format_rejects_flag_extension_mismatch() {
        let err = resolve_format(Some(Format::Wav), Some(Path::new("take.m4a")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--format wav"), "{err}");
        assert_eq!(
            resolve_format(Some(Format::M4a), Some(Path::new("t.M4A"))).unwrap(),
            Format::M4a
        );
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
