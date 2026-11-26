//! Command-line interface: argument definitions and command handlers.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::mode::{Mode, Settings};
use crate::process::{TrimOutcome, trim_file};
use crate::recorder::{RecordOptions, RecordReport, StopReason, record};

/// Record everything your Mac plays, then trim the silence around it.
#[derive(Parser)]
#[command(name = "map-recorder", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Record system audio until you press Enter (or Ctrl+C), then trim it.
    Record(RecordArgs),
    /// Trim the silence of an existing .wav or .caf file.
    Trim(TrimArgs),
}

/// Options shared by every command that trims audio.
#[derive(Args)]
pub struct TuningArgs {
    /// What you are recording: picks silence sensitivity and output format.
    #[arg(long, value_enum, default_value = "music")]
    pub mode: Mode,
    /// Loudness (dBFS) below which audio counts as silence
    /// [default: -60 for music, -40 for meeting].
    #[arg(long, allow_negative_numbers = true)]
    pub threshold_db: Option<f32>,
    /// Maximum seconds of silence kept before the first sound [default: 2].
    #[arg(long)]
    pub max_lead: Option<f32>,
    /// Seconds of silence kept after the last sound [default: 0.5].
    #[arg(long)]
    pub tail_pad: Option<f32>,
}

impl TuningArgs {
    /// The mode's presets, overridden by any value given on the command line.
    pub fn settings(&self) -> Settings {
        let defaults = self.mode.defaults();
        Settings {
            threshold_db: self.threshold_db.unwrap_or(defaults.threshold_db),
            max_lead_secs: self.max_lead.unwrap_or(defaults.max_lead_secs),
            tail_pad_secs: self.tail_pad.unwrap_or(defaults.tail_pad_secs),
        }
    }
}

#[derive(Args)]
pub struct TrimArgs {
    /// The recording to trim (.wav or .caf).
    pub input: PathBuf,
    /// Where to write the result [default: <input>.trimmed.<wav|m4a>].
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub tuning: TuningArgs,
}

#[derive(Args)]
pub struct RecordArgs {
    /// Where to write the result [default: ./recording-YYYYMMDD-HHMMSS.<wav|m4a>].
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Keep the untrimmed capture next to the result.
    #[arg(long)]
    pub keep_raw: bool,
    #[command(flatten)]
    pub tuning: TuningArgs,
}

/// Exit code used when no sound was found and nothing was written.
pub const EXIT_NO_SOUND: u8 = 2;

/// `recording-YYYYMMDD-HHMMSS.<ext>` in the current directory.
pub fn default_output_path(mode: Mode, now: chrono::DateTime<chrono::Local>) -> PathBuf {
    PathBuf::from(format!(
        "recording-{}.{}",
        now.format("%Y%m%d-%H%M%S"),
        mode.output_extension()
    ))
}

/// Default result path for `trim`: `<dir>/<stem>.trimmed.<ext>`.
fn trimmed_path_for(input: &Path, mode: Mode) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}.trimmed.{}", mode.output_extension()))
}

/// Refuses to overwrite existing files.
pub fn ensure_absent(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("Output file {} already exists", path.display());
    }
    Ok(())
}

/// Prints the outcome of trimming and returns the matching exit code.
pub fn report_outcome(outcome: &TrimOutcome, settings: &Settings) -> ExitCode {
    match outcome {
        TrimOutcome::Written {
            path,
            kept_secs,
            removed_lead_secs,
            removed_tail_secs,
        } => {
            println!("Saved {}", path.display());
            println!(
                "  kept {kept_secs:.1} s, removed {removed_lead_secs:.1} s at the start and {removed_tail_secs:.1} s at the end"
            );
            ExitCode::SUCCESS
        }
        TrimOutcome::NoSound => {
            println!(
                "No sound detected (threshold {} dBFS). Nothing written.",
                settings.threshold_db
            );
            ExitCode::from(EXIT_NO_SOUND)
        }
    }
}

/// Prints the summary of a recording session and returns the exit code.
fn report_recording(report: &RecordReport, settings: &Settings) -> ExitCode {
    println!("Recorded {:.1} s of audio.", report.duration_secs);
    match report.stop_reason {
        StopReason::SizeLimit => println!(
            "Warning: stopped automatically because the WAV file reached its size limit (about 3 hours). Use --mode meeting for longer sessions."
        ),
        StopReason::Error => println!("Warning: recording stopped because of a write error."),
        StopReason::User => {}
    }
    if report.overruns > 0 {
        println!(
            "Warning: {} audio buffers were dropped because the disk could not keep up.",
            report.overruns
        );
    }
    let code = report_outcome(&report.outcome, settings);
    if report.outcome == TrimOutcome::NoSound {
        println!(
            "If audio was playing, check that your terminal has the System Audio Recording permission (see README → Troubleshooting)."
        );
    }
    if report.raw_kept {
        println!("Untrimmed recording kept at {}", report.raw_path.display());
    }
    code
}

/// Runs the parsed command and returns the process exit code.
pub fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Trim(args) => {
            let mode = args.tuning.mode;
            let settings = args.tuning.settings();
            let output = args
                .output
                .unwrap_or_else(|| trimmed_path_for(&args.input, mode));
            ensure_absent(&output)?;
            let outcome = trim_file(&args.input, &output, mode, &settings)?;
            Ok(report_outcome(&outcome, &settings))
        }
        Command::Record(args) => {
            let mode = args.tuning.mode;
            let settings = args.tuning.settings();
            let output = args
                .output
                .unwrap_or_else(|| default_output_path(mode, chrono::Local::now()));
            let report = record(&RecordOptions {
                mode,
                settings,
                output,
                keep_raw: args.keep_raw,
            })?;
            Ok(report_recording(&report, &settings))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn settings_override_mode_defaults() {
        let cli = Cli::parse_from([
            "map-recorder",
            "trim",
            "in.wav",
            "--mode",
            "meeting",
            "--threshold-db",
            "-50",
        ]);
        let Command::Trim(args) = cli.command else {
            panic!("expected trim");
        };
        assert_eq!(
            args.tuning.settings(),
            Settings {
                threshold_db: -50.0,
                max_lead_secs: 2.0,
                tail_pad_secs: 0.5
            }
        );
    }

    #[test]
    fn settings_default_to_mode_presets() {
        let cli = Cli::parse_from(["map-recorder", "record"]);
        let Command::Record(args) = cli.command else {
            panic!("expected record");
        };
        assert_eq!(args.tuning.mode, Mode::Music);
        assert_eq!(args.tuning.settings(), Mode::Music.defaults());
    }

    #[test]
    fn default_output_path_format() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 26, 16, 9, 5)
            .unwrap();
        assert_eq!(
            default_output_path(Mode::Music, now),
            PathBuf::from("recording-20261126-160905.wav")
        );
        assert_eq!(
            default_output_path(Mode::Meeting, now),
            PathBuf::from("recording-20261126-160905.m4a")
        );
    }
}
