//! End-to-end tests of the `convert` command, running the real binary.

use std::path::Path;
use std::process::{Command, Output};

/// Writes 1 s of a 16-bit stereo tone at 48 kHz.
fn write_wav(path: &Path) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..48_000 {
        let s = (8_000.0 * (i as f32 * 0.05).sin()) as i16;
        writer.write_sample(s).unwrap();
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_map-recorder"))
        .args(args)
        .output()
        .unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn convert_multiple_files_continues_after_failure() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.wav");
    let b = dir.path().join("b.wav");
    let missing = dir.path().join("missing.wav");
    write_wav(&a);
    write_wav(&b);

    let out = run(&[
        "convert",
        a.to_str().unwrap(),
        missing.to_str().unwrap(),
        b.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing.wav"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("a.m4a") && stdout.contains("b.m4a"),
        "{stdout}"
    );
    assert_eq!(names(dir.path()), ["a.m4a", "a.wav", "b.m4a", "b.wav"]);
}

#[test]
fn convert_m4a_input_refuses_to_overwrite_itself() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("take.wav");
    let song = dir.path().join("song.m4a");
    write_wav(&wav);
    let out = run(&[
        "convert",
        wav.to_str().unwrap(),
        "-o",
        song.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let before = std::fs::read(&song).unwrap();

    let out = run(&["convert", song.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read(&song).unwrap(), before);
}

#[test]
fn convert_output_flag_with_many_files_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.wav");
    let b = dir.path().join("b.wav");
    write_wav(&a);
    write_wav(&b);
    let x = dir.path().join("x.m4a");
    let out = run(&[
        "convert",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "-o",
        x.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert_eq!(names(dir.path()), ["a.wav", "b.wav"]);
}
