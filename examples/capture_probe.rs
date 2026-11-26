//! Manual check of the capture layer.
//!
//! Records five seconds of system audio into `probe.wav` (32-bit float) and
//! prints the stream format, the number of frames captured and the peak level.
//! Play some music before running it: `cargo run --example capture_probe`.

use std::thread;
use std::time::{Duration, Instant};

use map_recorder::capture::SystemAudioTap;

const SECONDS: u64 = 5;

fn main() -> anyhow::Result<()> {
    // Real-time callback → ring buffer: the callback only pushes samples.
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 8 * SECONDS as usize);
    let tap = SystemAudioTap::start(move |samples| {
        for &sample in samples {
            if producer.push(sample).is_err() {
                break;
            }
        }
    })?;
    let format = tap.format();
    println!("format: {} Hz, {} ch", format.sample_rate, format.channels);

    // Drain the ring buffer into memory while the tap is running.
    let mut captured = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(SECONDS) {
        while let Ok(sample) = consumer.pop() {
            captured.push(sample);
        }
        thread::sleep(Duration::from_millis(20));
    }
    drop(tap);
    while let Ok(sample) = consumer.pop() {
        captured.push(sample);
    }

    // Persist the capture and report how loud it was.
    let spec = hound::WavSpec {
        channels: format.channels,
        sample_rate: format.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create("probe.wav", spec)?;
    for &sample in &captured {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    let peak = captured.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
    let peak_db = 20.0 * peak.max(1e-10).log10();
    println!(
        "frames: {}, peak: {:.1} dBFS, written: probe.wav",
        captured.len() / format.channels.max(1) as usize,
        peak_db
    );
    Ok(())
}
