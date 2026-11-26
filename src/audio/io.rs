//! Streaming access to the raw (untrimmed) capture, in WAV or CAF.
//!
//! The container is chosen from the file extension. Samples are always
//! exchanged as interleaved `f32`, read and written in chunks so that memory
//! use does not grow with the length of the recording.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::caf::{CafReader, CafWriter};
use crate::capture::StreamFormat;

/// Supported containers, derived from a file extension.
enum Container {
    Wav,
    Caf,
}

fn container_of(path: &Path) -> Result<Container> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("wav") => Ok(Container::Wav),
        Some("caf") => Ok(Container::Caf),
        _ => bail!(
            "unsupported file type for {}: expected a .wav or .caf file",
            path.display()
        ),
    }
}

/// Writer for the raw capture (32-bit float).
pub enum RawWriter {
    Wav(hound::WavWriter<BufWriter<File>>),
    Caf(CafWriter),
}

impl RawWriter {
    /// Creates a 32-bit float file; the container follows the extension.
    pub fn create(path: &Path, format: StreamFormat) -> Result<Self> {
        let context = || format!("cannot create {}", path.display());
        Ok(match container_of(path)? {
            Container::Wav => {
                let spec = hound::WavSpec {
                    channels: format.channels,
                    sample_rate: format.sample_rate,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                };
                RawWriter::Wav(hound::WavWriter::create(path, spec).with_context(context)?)
            }
            Container::Caf => RawWriter::Caf(
                CafWriter::create(path, format.sample_rate, format.channels)
                    .with_context(context)?,
            ),
        })
    }

    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        match self {
            RawWriter::Wav(writer) => {
                for &sample in interleaved {
                    writer.write_sample(sample)?;
                }
            }
            RawWriter::Caf(writer) => writer.write_samples(interleaved)?,
        }
        Ok(())
    }

    /// Audio bytes written so far; used to stay below the WAV size limit.
    pub fn bytes_written(&self) -> u64 {
        match self {
            RawWriter::Wav(writer) => writer.len() as u64 * 4,
            RawWriter::Caf(writer) => writer.bytes_written(),
        }
    }

    /// Pushes buffered samples to disk and updates the container's size
    /// fields, so the file is complete up to this point even if the process
    /// is killed before `finalize`.
    pub fn flush(&mut self) -> Result<()> {
        match self {
            RawWriter::Wav(writer) => writer.flush()?,
            RawWriter::Caf(writer) => writer.flush()?,
        }
        Ok(())
    }

    /// Flushes everything and fixes up the container's size fields.
    pub fn finalize(self) -> Result<()> {
        match self {
            RawWriter::Wav(writer) => writer.finalize()?,
            RawWriter::Caf(writer) => writer.finalize()?,
        }
        Ok(())
    }
}

/// How samples stored in a WAV file are converted to `f32`.
enum WavSamples {
    Float,
    /// Integer PCM; the value is the divisor that maps it to `[-1, 1]`.
    Int(f32),
}

enum ReaderInner {
    Wav(hound::WavReader<BufReader<File>>, WavSamples),
    Caf(CafReader),
}

/// Reader for raw captures and user-supplied files.
///
/// WAV files may hold 32-bit float or 16/24/32-bit integer samples; CAF
/// files must be 32-bit float (the format this program writes).
pub struct RawReader {
    inner: ReaderInner,
    format: StreamFormat,
    total_frames: u64,
}

impl RawReader {
    pub fn open(path: &Path) -> Result<Self> {
        let context = || format!("cannot read {}", path.display());
        match container_of(path)? {
            Container::Wav => {
                let reader = hound::WavReader::open(path).with_context(context)?;
                let spec = reader.spec();
                let samples = match (spec.sample_format, spec.bits_per_sample) {
                    (hound::SampleFormat::Float, 32) => WavSamples::Float,
                    (hound::SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
                        WavSamples::Int((1u64 << (bits - 1)) as f32)
                    }
                    (_, bits) => bail!("unsupported WAV sample format ({bits}-bit)"),
                };
                Ok(Self {
                    format: StreamFormat {
                        sample_rate: spec.sample_rate,
                        channels: spec.channels,
                    },
                    total_frames: reader.duration() as u64,
                    inner: ReaderInner::Wav(reader, samples),
                })
            }
            Container::Caf => {
                let reader = CafReader::open(path).with_context(context)?;
                Ok(Self {
                    format: StreamFormat {
                        sample_rate: reader.sample_rate(),
                        channels: reader.channels(),
                    },
                    total_frames: reader.total_frames(),
                    inner: ReaderInner::Caf(reader),
                })
            }
        }
    }

    pub fn format(&self) -> StreamFormat {
        self.format
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Replaces the contents of `buf` with up to `max_frames` frames and
    /// returns how many were read; 0 means end of audio.
    pub fn read_frames(&mut self, buf: &mut Vec<f32>, max_frames: usize) -> Result<usize> {
        match &mut self.inner {
            ReaderInner::Caf(reader) => Ok(reader.read_frames(buf, max_frames)?),
            ReaderInner::Wav(reader, kind) => {
                buf.clear();
                let wanted = max_frames * self.format.channels as usize;
                match kind {
                    WavSamples::Float => {
                        for sample in reader.samples::<f32>().take(wanted) {
                            buf.push(sample?);
                        }
                    }
                    WavSamples::Int(scale) => {
                        for sample in reader.samples::<i32>().take(wanted) {
                            buf.push(sample? as f32 / *scale);
                        }
                    }
                }
                Ok(buf.len() / self.format.channels.max(1) as usize)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: StreamFormat = StreamFormat {
        sample_rate: 48_000,
        channels: 2,
    };

    fn signal(frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| (i as f32 / 77.0).sin() * 0.8)
            .collect()
    }

    fn read_all(reader: &mut RawReader) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = Vec::new();
        while reader.read_frames(&mut buf, 1000).unwrap() > 0 {
            all.extend_from_slice(&buf);
        }
        all
    }

    fn round_trip(name: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let samples = signal(5_000, 2);
        let mut writer = RawWriter::create(&path, STEREO).unwrap();
        writer.write(&samples).unwrap();
        assert_eq!(writer.bytes_written(), 5_000 * 2 * 4);
        writer.finalize().unwrap();

        let mut reader = RawReader::open(&path).unwrap();
        assert_eq!(reader.format(), STEREO);
        assert_eq!(reader.total_frames(), 5_000);
        assert_eq!(read_all(&mut reader), samples);
    }

    #[test]
    fn flushed_wav_is_readable_while_still_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.raw.wav");
        let samples = signal(2_000, 2);
        let mut writer = RawWriter::create(&path, STEREO).unwrap();
        writer.write(&samples).unwrap();
        writer.flush().unwrap();
        // The writer is still open (e.g. the process is about to be killed).
        let mut reader = RawReader::open(&path).unwrap();
        assert_eq!(reader.total_frames(), 2_000);
        assert_eq!(read_all(&mut reader), samples);
        drop(writer);
    }

    #[test]
    fn wav_float_round_trip() {
        round_trip("a.raw.wav");
    }

    #[test]
    fn caf_round_trip_via_raw_types() {
        round_trip("a.raw.caf");
    }

    fn write_int_wav(path: &Path, channels: u16, bits: u16, values: &[i32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 44_100,
            bits_per_sample: bits,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for &v in values {
            if bits == 16 {
                writer.write_sample(v as i16).unwrap();
            } else {
                writer.write_sample(v).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn reads_int16_mono_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i16.wav");
        write_int_wav(&path, 1, 16, &[16_384, -32_768, 0]);
        let mut reader = RawReader::open(&path).unwrap();
        assert_eq!(
            reader.format(),
            StreamFormat {
                sample_rate: 44_100,
                channels: 1
            }
        );
        assert_eq!(read_all(&mut reader), vec![0.5, -1.0, 0.0]);
    }

    #[test]
    fn reads_int24_stereo_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i24.wav");
        write_int_wav(&path, 2, 24, &[4_194_304, -4_194_304]);
        let mut reader = RawReader::open(&path).unwrap();
        assert_eq!(reader.total_frames(), 1);
        assert_eq!(read_all(&mut reader), vec![0.5, -0.5]);
    }

    #[test]
    fn unknown_extension_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mp3");
        let message = RawWriter::create(&path, STEREO).err().unwrap().to_string();
        assert!(
            message.contains("wav") && message.contains("caf"),
            "{message}"
        );
        std::fs::write(&path, b"junk").unwrap();
        let message = format!("{:#}", RawReader::open(&path).err().unwrap());
        assert!(
            message.contains("wav") && message.contains("caf"),
            "{message}"
        );
    }
}
