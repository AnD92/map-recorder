//! Minimal Core Audio Format (CAF) support for 32-bit float linear PCM.
//!
//! CAF is Apple's container. Unlike WAV it has no 4 GB limit, and its data
//! chunk may declare an "unknown" size (-1), which lets us stream audio to
//! disk and still read the file if the program is interrupted before the
//! size is patched. Only the exact layout this module writes is supported:
//! little-endian 32-bit float samples, interleaved.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// `kCAFLinearPCMFormatFlagIsFloat | kCAFLinearPCMFormatFlagIsLittleEndian`.
const FLOAT_LITTLE_ENDIAN: u32 = 0b11;
/// Byte offset of the data chunk's size field in files written by `CafWriter`
/// (file header 8 + desc chunk header 12 + desc body 32).
const DATA_SIZE_OFFSET: u64 = 8 + 12 + 32 + 4;
/// Size of the data chunk's "edit count" prefix that precedes the samples.
const EDIT_COUNT_BYTES: u64 = 4;

/// Streams interleaved `f32` samples into a CAF file.
pub struct CafWriter {
    out: BufWriter<File>,
    bytes_written: u64,
}

impl CafWriter {
    /// Creates the file and writes the header with an "unknown" data size,
    /// so the file is valid even if `finalize` is never called.
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);

        // File header: magic, version 1, flags 0.
        out.write_all(b"caff")?;
        out.write_all(&1u16.to_be_bytes())?;
        out.write_all(&0u16.to_be_bytes())?;

        // Audio description chunk: sample rate and sample layout.
        out.write_all(b"desc")?;
        out.write_all(&32i64.to_be_bytes())?;
        out.write_all(&(sample_rate as f64).to_be_bytes())?;
        out.write_all(b"lpcm")?;
        out.write_all(&FLOAT_LITTLE_ENDIAN.to_be_bytes())?;
        out.write_all(&(4 * channels as u32).to_be_bytes())?; // bytes per packet
        out.write_all(&1u32.to_be_bytes())?; // frames per packet
        out.write_all(&(channels as u32).to_be_bytes())?;
        out.write_all(&32u32.to_be_bytes())?; // bits per channel

        // Data chunk header: size -1 means "until end of file".
        out.write_all(b"data")?;
        out.write_all(&(-1i64).to_be_bytes())?;
        out.write_all(&0u32.to_be_bytes())?; // edit count

        Ok(Self {
            out,
            bytes_written: 0,
        })
    }

    pub fn write_samples(&mut self, interleaved: &[f32]) -> io::Result<()> {
        for sample in interleaved {
            self.out.write_all(&sample.to_le_bytes())?;
        }
        self.bytes_written += interleaved.len() as u64 * 4;
        Ok(())
    }

    /// Audio bytes written so far (headers excluded).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Pushes buffered samples to disk. The data size stays "unknown", which
    /// readers resolve from the file length.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Flushes the samples and records the real data chunk size.
    pub fn finalize(mut self) -> io::Result<()> {
        self.out.flush()?;
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(DATA_SIZE_OFFSET))?;
        file.write_all(&((EDIT_COUNT_BYTES + self.bytes_written) as i64).to_be_bytes())?;
        file.sync_all()
    }
}

/// Reads interleaved `f32` samples from a CAF file written by `CafWriter`
/// (or any CAF with the same sample layout).
pub struct CafReader {
    input: BufReader<File>,
    sample_rate: u32,
    channels: u16,
    total_frames: u64,
    frames_read: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn read_array<const N: usize>(input: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

impl CafReader {
    /// Parses the header, validates the sample layout and positions the
    /// reader at the first sample.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut input = BufReader::new(file);

        let header: [u8; 8] = read_array(&mut input).map_err(|_| invalid("not a CAF file"))?;
        if &header[..4] != b"caff" {
            return Err(invalid("not a CAF file"));
        }

        // Walk the chunks until the audio data; remember the description.
        let mut position = 8u64;
        let mut format: Option<(u32, u16)> = None;
        loop {
            let kind: [u8; 4] =
                read_array(&mut input).map_err(|_| invalid("CAF file has no audio data"))?;
            let size = i64::from_be_bytes(read_array(&mut input)?);
            position += 12;
            match &kind {
                b"desc" => {
                    let rate = f64::from_be_bytes(read_array(&mut input)?);
                    let format_id: [u8; 4] = read_array(&mut input)?;
                    let flags = u32::from_be_bytes(read_array(&mut input)?);
                    let _bytes_per_packet = u32::from_be_bytes(read_array(&mut input)?);
                    let frames_per_packet = u32::from_be_bytes(read_array(&mut input)?);
                    let channels = u32::from_be_bytes(read_array(&mut input)?);
                    let bits = u32::from_be_bytes(read_array(&mut input)?);
                    if &format_id != b"lpcm"
                        || flags != FLOAT_LITTLE_ENDIAN
                        || bits != 32
                        || frames_per_packet != 1
                        || channels == 0
                        || channels > u16::MAX as u32
                    {
                        return Err(invalid(
                            "unsupported CAF sample format (expected 32-bit float little-endian PCM)",
                        ));
                    }
                    format = Some((rate.round() as u32, channels as u16));
                    input.seek_relative(size - 32)?;
                    position += size as u64;
                }
                b"data" => {
                    let (sample_rate, channels) =
                        format.ok_or_else(|| invalid("CAF data chunk before description"))?;
                    input.seek_relative(EDIT_COUNT_BYTES as i64)?;
                    position += EDIT_COUNT_BYTES;
                    let data_bytes = if size < 0 {
                        file_len.saturating_sub(position)
                    } else {
                        (size as u64).saturating_sub(EDIT_COUNT_BYTES)
                    };
                    return Ok(Self {
                        input,
                        sample_rate,
                        channels,
                        total_frames: data_bytes / (4 * channels as u64),
                        frames_read: 0,
                    });
                }
                _ => {
                    if size < 0 {
                        return Err(invalid("CAF chunk with unknown size before audio data"));
                    }
                    input.seek_relative(size)?;
                    position += size as u64;
                }
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Replaces the contents of `buf` with up to `max_frames` frames and
    /// returns how many were read; 0 means end of audio.
    pub fn read_frames(&mut self, buf: &mut Vec<f32>, max_frames: usize) -> io::Result<usize> {
        buf.clear();
        let frames = (self.total_frames - self.frames_read).min(max_frames as u64) as usize;
        let mut bytes = vec![0u8; frames * self.channels as usize * 4];
        self.input.read_exact(&mut bytes)?;
        buf.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        );
        self.frames_read += frames as u64;
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| (i as f32 / 1000.0).sin())
            .collect()
    }

    fn read_all(reader: &mut CafReader) -> Vec<f32> {
        let mut all = Vec::new();
        let mut buf = Vec::new();
        while reader.read_frames(&mut buf, 4096).unwrap() > 0 {
            all.extend_from_slice(&buf);
        }
        all
    }

    #[test]
    fn round_trip_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.caf");
        let samples = ramp(10_000, 2);
        let mut writer = CafWriter::create(&path, 48_000, 2).unwrap();
        writer.write_samples(&samples[..7_000]).unwrap();
        writer.write_samples(&samples[7_000..]).unwrap();
        assert_eq!(writer.bytes_written(), 10_000 * 2 * 4);
        writer.finalize().unwrap();

        let mut reader = CafReader::open(&path).unwrap();
        assert_eq!(reader.sample_rate(), 48_000);
        assert_eq!(reader.channels(), 2);
        assert_eq!(reader.total_frames(), 10_000);
        assert_eq!(read_all(&mut reader), samples);
    }

    #[test]
    fn round_trip_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.caf");
        let samples = ramp(5_000, 1);
        let mut writer = CafWriter::create(&path, 44_100, 1).unwrap();
        writer.write_samples(&samples).unwrap();
        writer.finalize().unwrap();
        let mut reader = CafReader::open(&path).unwrap();
        assert_eq!((reader.sample_rate(), reader.channels()), (44_100, 1));
        assert_eq!(read_all(&mut reader), samples);
    }

    #[test]
    fn unfinalized_file_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.caf");
        let samples = ramp(3_000, 2);
        let mut writer = CafWriter::create(&path, 48_000, 2).unwrap();
        writer.write_samples(&samples).unwrap();
        drop(writer); // simulated crash: data size never patched
        let mut reader = CafReader::open(&path).unwrap();
        assert_eq!(reader.total_frames(), 3_000);
        assert_eq!(read_all(&mut reader), samples);
    }

    #[test]
    fn rejects_non_caf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.caf");
        std::fs::write(&path, b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        let err = CafReader::open(&path).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn afinfo_accepts_our_caf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.caf");
        let mut writer = CafWriter::create(&path, 48_000, 2).unwrap();
        writer.write_samples(&ramp(4_800, 2)).unwrap();
        writer.finalize().unwrap();
        let out = std::process::Command::new("afinfo")
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(out.status.success(), "{text}");
        assert!(
            text.contains("float32") || text.contains("32-bit float"),
            "{text}"
        );
        assert!(text.contains("4800"), "{text}");
    }
}
