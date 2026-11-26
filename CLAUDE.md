# CLAUDE.md — notes for AI-assisted work on map-recorder

Rust CLI for macOS 14.2+ that records system audio via a Core Audio process tap
and trims leading/trailing silence. User-facing docs: `README.md`. Design and
plan: `docs/superpowers/specs/`, `docs/superpowers/plans/`.

## Commands

- Rust may not be on PATH in non-interactive shells: `source ~/.cargo/env` first.
- `cargo test` — all unit + integration tests (must stay green).
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` — must be clean.
- `cargo run --example capture_probe` — manual 5 s capture check.

## Module map

- `src/capture/tap.rs` — ALL `unsafe` code (tap, aggregate device, IO block). Explained in `docs/core-audio-tap.md`; keep that doc in sync.
- `src/capture/mod.rs` — safe API: `SystemAudioTap::start(callback)`, `StreamFormat`, `CaptureError`.
- `src/recorder.rs` — record session: rtrb ring buffer, writer thread, Enter/Ctrl+C, status line, WAV size cap.
- `src/trim.rs` — pure silence detection (10 ms RMS windows, ≥50 ms events, fades).
- `src/process.rs` — `trim_file`: two streaming passes (measure, then copy range).
- `src/encode.rs` — `FinalWriter`: 24-bit WAV or float CAF → `afconvert` → AAC m4a, in any mode (meeting downmixes to mono); `convert_to_aac`/`aac_settings` shared with `convert`.
- `src/audio/caf.rs` — hand-written CAF f32 reader/writer (data size -1 while recording).
- `src/audio/io.rs` — `RawWriter`/`RawReader` over WAV (hound) and CAF, picked by extension.
- `src/mode.rs` — `Mode` presets (music −60 dBFS / meeting −40 dBFS), raw extensions; `Format` (m4a default / wav) and `resolve_format` (`--format` vs `-o` extension).
- `src/convert.rs` — `convert` command: any file `afconvert` reads → `.m4a`, no trimming.
- `src/cli.rs` — clap definitions (record, trim, convert, completions), `EXAMPLES` help block, default paths (`~/Music/map-recorder`, `MAP_RECORDER_DIR`), reporting.

## Rules

- `unsafe` only in `src/capture/`; every block has a `// SAFETY:` comment.
- Real-time callback (IO block and the closure passed to `SystemAudioTap::start`): no allocation, no locks, no I/O, no logging.
- Comments explain the purpose of a block, professionally; don't narrate trivial code.
- User-facing text and docs in English (the user talks to Claude in Italian).
- TDD: write the failing test first. Pure logic lives outside `capture`/`recorder` so it can be unit-tested.

## Gotchas learned

- **Running the capture from Claude's shell gives zero frames**: every Core Audio call succeeds but the IO block never fires (no TCC permission in that context). Capture must be verified by the user in their own terminal. Plumbing can still be smoke-tested from here with `(sleep 1; echo) | ./target/debug/map-recorder record -o <scratch>/x.wav` → expect the "No sound detected" path and exit code 2.
- On macOS 26 the tap delivers **interleaved** f32 stereo (1 buffer, 2 channels, 48 kHz); the non-interleaved path exists but hasn't been observed.
- `Info.plist` is embedded via `build.rs` (`-sectcreate __TEXT __info_plist`) for every linked artifact; needed for the permission prompt.
- Changing output device mid-recording: brief gap in captured audio, then it resumes (verified by the user).
- No `timeout` command on macOS by default.
- macOS has no MP3 encoder (`afconvert -f MPG3` fails with `'fmt?'`; it only decodes MP3). AAC at 256 kbps is rejected at low sample rates (e.g. 22.05 kHz), so m4a output is always encoded at 48 kHz.
- `cargo::unused_dependencies` warnings appear for dev-only crates; harmless.
