//! End-to-end tests of the `trim` command, running the real binary.

use std::path::Path;
use std::process::{Command, Output};

const SR: u32 = 48_000;

/// Writes a 32-bit float stereo WAV: `lead` s silence, 1 s tone, `tail` s silence.
fn write_wav(path: &Path, lead: f32, tone: bool, tail: f32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    let frames = |secs: f32| (secs * SR as f32) as usize;
    for _ in 0..frames(lead) * 2 {
        writer.write_sample(0.0f32).unwrap();
    }
    if tone {
        for i in 0..SR as usize {
            let s = 0.5 * (i as f32 * 0.0576).sin();
            writer.write_sample(s).unwrap();
            writer.write_sample(s).unwrap();
        }
    }
    for _ in 0..frames(tail) * 2 {
        writer.write_sample(0.0f32).unwrap();
    }
    writer.finalize().unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_map-recorder"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn trim_command_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("take.wav");
    write_wav(&input, 4.0, true, 3.0);

    let out = run(&["trim", input.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = dir.path().join("take.trimmed.wav");
    assert!(expected.is_file());
    assert!(stdout.contains("take.trimmed.wav"), "{stdout}");
    let duration = hound::WavReader::open(&expected).unwrap().duration();
    assert_eq!(duration, SR * 2 + SR + SR / 2); // 2 s lead + 1 s tone + 0.5 s tail
}

#[test]
fn trim_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("take.wav");
    write_wav(&input, 0.0, true, 1.0);
    let output = dir.path().join("exists.wav");
    std::fs::write(&output, b"keep me").unwrap();

    let out = run(&[
        "trim",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
    assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
}

#[test]
fn trim_silent_file_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("quiet.wav");
    write_wav(&input, 2.0, false, 0.0);

    let out = run(&["trim", input.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("No sound detected"));
    assert!(!dir.path().join("quiet.trimmed.wav").exists());
}
