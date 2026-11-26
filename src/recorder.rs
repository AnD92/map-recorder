//! A recording session: system audio → ring buffer → raw file → trimmed result.
//!
//! Threads involved:
//! - Core Audio's real-time thread runs the capture callback, which only
//!   copies samples into a lock-free ring buffer and updates the level meter;
//! - a writer thread drains the ring buffer into the raw file;
//! - a stdin thread waits for Enter;
//! - the main thread draws the status line and coordinates the stop.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::audio::io::RawWriter;
use crate::capture::{StreamFormat, SystemAudioTap};
use crate::cli::ensure_absent;
use crate::encode::partial_paths_for;
use crate::mode::{Format, Mode, Settings, raw_path_for};
use crate::process::{TrimOutcome, trim_file};

/// The music raw file (32-bit float WAV) is stopped before WAV's 4 GiB limit.
pub const WAV_SAFE_LIMIT_BYTES: u64 = 4_000_000_000;
/// Seconds of audio the ring buffer can hold if the writer falls behind.
pub const RING_SECONDS: u32 = 10;
/// Ring capacity: `RING_SECONDS` of 96 kHz stereo (≈ 7.7 MB). Typical
/// streams (48 kHz stereo) get twice that margin.
const RING_CAPACITY_SAMPLES: usize = RING_SECONDS as usize * 96_000 * 2;
/// How often the raw file's header is brought up to date while recording.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// How often the status line is redrawn.
const REFRESH: Duration = Duration::from_millis(100);
/// Width of the level meter bar, in characters.
const METER_WIDTH: usize = 20;
/// Level shown as an empty meter.
const METER_FLOOR_DB: f32 = -60.0;

/// Why the recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Enter or Ctrl+C.
    User,
    /// The music raw WAV approached its size limit.
    SizeLimit,
    /// Writing the raw file failed.
    Error,
}

pub struct RecordOptions {
    pub mode: Mode,
    /// Format of the final, trimmed file.
    pub format: Format,
    pub settings: Settings,
    pub output: PathBuf,
    pub keep_raw: bool,
}

pub struct RecordReport {
    pub raw_path: PathBuf,
    /// Whether the raw file is still on disk.
    pub raw_kept: bool,
    pub duration_secs: f64,
    /// Number of audio callbacks dropped because the writer fell behind.
    pub overruns: u64,
    /// Audio cycles skipped because their buffer layout was not understood.
    pub unreadable_cycles: u64,
    pub stop_reason: StopReason,
    pub outcome: TrimOutcome,
}

/// One-line status shown while recording, e.g.
/// `● REC 00:03:27  [#########-----------]  -18.4 dBFS   (press Enter to stop)`.
pub fn format_status(elapsed: Duration, level_db: f32) -> String {
    let total = elapsed.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total / 60) % 60, total % 60);
    let fill = ((level_db - METER_FLOOR_DB) / -METER_FLOOR_DB * METER_WIDTH as f32)
        .clamp(0.0, METER_WIDTH as f32) as usize;
    let bar = format!("{}{}", "#".repeat(fill), "-".repeat(METER_WIDTH - fill));
    let level = if level_db.is_finite() && level_db > -100.0 {
        format!("{level_db:5.1} dBFS")
    } else {
        "  silence ".to_string()
    };
    format!("● REC {hours:02}:{minutes:02}:{seconds:02}  [{bar}]  {level}   (press Enter to stop)")
}

/// Whether the raw file must be closed to stay within the container's limit.
/// Only WAV (music) has one; CAF (meeting) grows without limit.
pub fn size_limit_reached(mode: Mode, bytes_written: u64) -> bool {
    mode == Mode::Music && bytes_written >= WAV_SAFE_LIMIT_BYTES
}

/// What a Ctrl+C received after the stop was already requested should do.
#[derive(Debug, PartialEq, Eq)]
enum SignalAction {
    /// The raw file is still being closed: exiting now could corrupt it.
    WaitForRawFile,
    /// The raw file is complete: exit immediately and point at it.
    ExitKeepingRaw,
}

fn repeated_signal_action(raw_finalized: bool) -> SignalAction {
    if raw_finalized {
        SignalAction::ExitKeepingRaw
    } else {
        SignalAction::WaitForRawFile
    }
}

/// State shared between the main thread, the writer thread and the signal
/// and stdin handlers.
struct Shared {
    stop: AtomicBool,
    stop_reason: AtomicU8,
    capture_done: AtomicBool,
    /// Set once the raw file has been finalised and is safe to keep as is.
    raw_finalized: AtomicBool,
}

impl Shared {
    fn request_stop(&self, reason: StopReason) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            self.stop_reason.store(reason as u8, Ordering::SeqCst);
        }
    }

    fn reason(&self) -> StopReason {
        match self.stop_reason.load(Ordering::SeqCst) {
            r if r == StopReason::SizeLimit as u8 => StopReason::SizeLimit,
            r if r == StopReason::Error as u8 => StopReason::Error,
            _ => StopReason::User,
        }
    }
}

/// Records until the user stops, then trims the capture into `opts.output`.
pub fn record(opts: &RecordOptions) -> Result<RecordReport> {
    let raw_path = raw_path_for(&opts.output, opts.mode);
    ensure_absent(&opts.output)?;
    ensure_absent(&raw_path)?;

    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        stop_reason: AtomicU8::new(StopReason::User as u8),
        capture_done: AtomicBool::new(false),
        raw_finalized: AtomicBool::new(false),
    });
    install_stop_handlers(&shared, &raw_path, &opts.output)?;

    // The ring buffer must exist before capture starts, when the stream
    // format is not yet known, so it is sized for a demanding format.
    let (mut producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);

    // Start capturing. The callback runs on the real-time thread: it only
    // copies samples into the ring buffer and records the peak level.
    let peak = Arc::new(AtomicU32::new(0));
    let overruns = Arc::new(AtomicU64::new(0));
    let tap = {
        let peak = Arc::clone(&peak);
        let overruns = Arc::clone(&overruns);
        SystemAudioTap::start(move |samples| {
            // Non-negative floats order like their bit patterns, so an
            // integer max gives the float max without locks.
            let chunk_peak = samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
            peak.fetch_max(chunk_peak.to_bits(), Ordering::Relaxed);
            match producer.write_chunk_uninit(samples.len()) {
                Ok(chunk) => {
                    chunk.fill_from_iter(samples.iter().copied());
                }
                Err(_) => {
                    overruns.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
        .context("cannot start system audio capture")?
    };
    let format = tap.format();

    // Writer thread: streams the ring buffer into the raw file.
    let writer = {
        let shared = Arc::clone(&shared);
        let raw_path = raw_path.clone();
        let mode = opts.mode;
        thread::spawn(move || write_raw(&raw_path, format, mode, consumer, &shared))
    };

    // Main loop: redraw the status line until a stop is requested.
    println!(
        "Recording system audio ({} Hz, {} ch, {} mode) → {}",
        format.sample_rate,
        format.channels,
        mode_name(opts.mode),
        opts.output.display()
    );
    let started = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        let level = f32::from_bits(peak.swap(0, Ordering::Relaxed));
        let level_db = 20.0 * level.max(1e-10).log10();
        print!("\r{}", format_status(started.elapsed(), level_db));
        let _ = io::stdout().flush();
        thread::sleep(REFRESH);
    }
    println!();

    // Stop capturing, let the writer drain what is left, close the raw file.
    let unreadable_cycles = tap.unreadable_cycles();
    drop(tap);
    shared.capture_done.store(true, Ordering::SeqCst);
    let frames = writer
        .join()
        .map_err(|_| anyhow::anyhow!("raw writer thread panicked"))?
        .with_context(|| format!("failed while writing {}", raw_path.display()))?;
    shared.raw_finalized.store(true, Ordering::SeqCst);
    let duration_secs = frames as f64 / format.sample_rate as f64;

    // Trim; on failure the raw file stays on disk and its path is reported.
    println!("Trimming silence…");
    let outcome = trim_file(
        &raw_path,
        &opts.output,
        opts.mode,
        opts.format,
        &opts.settings,
    )
    .with_context(|| {
        format!(
            "trimming failed; the untrimmed recording is kept at {}",
            raw_path.display()
        )
    })?;
    let raw_kept = !matches!(outcome, TrimOutcome::Written { .. }) || opts.keep_raw;
    if !raw_kept {
        std::fs::remove_file(&raw_path)
            .with_context(|| format!("cannot delete {}", raw_path.display()))?;
    }

    Ok(RecordReport {
        raw_path,
        raw_kept,
        duration_secs,
        overruns: overruns.load(Ordering::Relaxed),
        unreadable_cycles,
        stop_reason: shared.reason(),
        outcome,
    })
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Music => "music",
        Mode::Meeting => "meeting",
    }
}

/// Enter (any line on stdin), Ctrl+C, closing the terminal or SIGTERM stop
/// the recording gracefully. A repeated signal, e.g. while trimming, exits
/// immediately once the raw file is complete, removing any half-written
/// final file and pointing at the raw one.
fn install_stop_handlers(shared: &Arc<Shared>, raw_path: &Path, output: &Path) -> Result<()> {
    let on_signal = {
        let shared = Arc::clone(shared);
        let raw_path = raw_path.to_path_buf();
        let output = output.to_path_buf();
        move || {
            if !shared.stop.load(Ordering::SeqCst) {
                shared.request_stop(StopReason::User);
                return;
            }
            match repeated_signal_action(shared.raw_finalized.load(Ordering::SeqCst)) {
                SignalAction::WaitForRawFile => {
                    eprintln!("\nFinishing the raw recording file, please wait…");
                }
                SignalAction::ExitKeepingRaw => {
                    for partial in partial_paths_for(&output) {
                        let _ = std::fs::remove_file(partial);
                    }
                    eprintln!(
                        "\nInterrupted. The untrimmed recording is at {}",
                        raw_path.display()
                    );
                    std::process::exit(130);
                }
            }
        }
    };
    ctrlc::set_handler(on_signal).context("cannot install the Ctrl+C handler")?;

    let shared = Arc::clone(shared);
    thread::spawn(move || {
        let mut line = String::new();
        // EOF (stdin not interactive) is ignored: only an actual line stops.
        if io::stdin().lock().read_line(&mut line).unwrap_or(0) > 0 {
            shared.request_stop(StopReason::User);
        }
    });
    Ok(())
}

/// Drains the ring buffer into the raw file until capture has ended and the
/// buffer is empty. Returns the number of frames written.
fn write_raw(
    path: &Path,
    format: StreamFormat,
    mode: Mode,
    mut consumer: rtrb::Consumer<f32>,
    shared: &Shared,
) -> Result<u64> {
    let result = (|| {
        let mut writer = RawWriter::create(path, format)?;
        let mut samples = 0u64;
        let mut last_flush = Instant::now();
        loop {
            // Read whatever is available; an empty buffer after capture has
            // ended means everything has been written.
            let done = shared.capture_done.load(Ordering::SeqCst);
            let available = consumer.slots();
            if available == 0 {
                if done {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            let chunk = consumer
                .read_chunk(available)
                .expect("slots() reported these samples");
            let (first, second) = chunk.as_slices();
            writer.write(first)?;
            writer.write(second)?;
            chunk.commit_all();
            samples += available as u64;

            // Keep the file valid on disk in case the process is killed.
            if last_flush.elapsed() >= FLUSH_INTERVAL {
                writer.flush()?;
                last_flush = Instant::now();
            }

            if size_limit_reached(mode, writer.bytes_written()) {
                shared.request_stop(StopReason::SizeLimit);
                break;
            }
        }
        writer.finalize()?;
        Ok(samples / format.channels.max(1) as u64)
    })();
    if result.is_err() {
        shared.request_stop(StopReason::Error);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_shows_time_and_level() {
        let line = format_status(Duration::from_secs(207), -18.4);
        assert!(line.contains("00:03:27"), "{line}");
        assert!(line.contains("-18.4 dBFS"), "{line}");
        assert!(line.contains("Enter"), "{line}");
    }

    #[test]
    fn status_line_handles_silence() {
        let line = format_status(Duration::ZERO, f32::NEG_INFINITY);
        assert!(line.contains("00:00:00"), "{line}");
        assert!(line.contains("silence"), "{line}");
    }

    #[test]
    fn repeated_ctrl_c_waits_until_raw_file_is_complete() {
        assert_eq!(repeated_signal_action(false), SignalAction::WaitForRawFile);
        assert_eq!(repeated_signal_action(true), SignalAction::ExitKeepingRaw);
    }

    #[test]
    fn size_limit_only_for_music() {
        assert!(size_limit_reached(Mode::Music, WAV_SAFE_LIMIT_BYTES));
        assert!(!size_limit_reached(Mode::Music, WAV_SAFE_LIMIT_BYTES - 1));
        assert!(!size_limit_reached(
            Mode::Meeting,
            10 * WAV_SAFE_LIMIT_BYTES
        ));
    }
}
