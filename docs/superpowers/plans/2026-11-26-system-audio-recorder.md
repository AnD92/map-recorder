# map-recorder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust CLI for macOS that records all system audio output via a Core Audio process tap and, on stop, trims leading silence to ≤ 2 s and removes trailing silence, with `music` and `meeting` modes.

**Architecture:** An `unsafe`-confined `capture` module wraps the Core Audio process tap and hands interleaved `f32` samples from the real-time thread to a lock-free ring buffer; a writer thread streams them into a raw WAV (music) or CAF (meeting). On stop, a streaming two-pass trimmer measures 10 ms window loudness, picks the range to keep, and writes the final file (24-bit WAV, or mono AAC `.m4a` via `afconvert`).

**Tech Stack:** Rust stable, `objc2` / `objc2-core-audio` / `objc2-core-foundation` / `objc2-foundation` / `block2` (Core Audio FFI), `rtrb` (ring buffer), `hound` (WAV), `clap` (CLI, derive), `ctrlc`, `anyhow` + `thiserror`, `chrono` (default file names), `tempfile` (dev).

**Spec:** `docs/superpowers/specs/2026-11-26-system-audio-recorder-design.md`. Read it before starting any task.

## Global Constraints

- macOS 14.2+ only; developed on macOS 26 / Apple Silicon. No Linux code paths.
- `unsafe` only inside `src/capture/`. Every `unsafe` block carries a `// SAFETY:` comment.
- Comments explain the **purpose** of each logical block, professionally; never narrate trivial code (loops, assignments).
- Nothing may allocate, lock, log or do I/O inside the Core Audio IOProc callback.
- No runtime dependencies beyond macOS itself (`afconvert` ships with macOS).
- Silence analysis: 10 ms windows, RMS over all channels in dBFS; sound event = ≥ 50 ms of consecutive windows at or above threshold; 5 ms linear fade at each cut point.
- Defaults: `music` threshold −60 dBFS, `meeting` −40 dBFS; `--max-lead` 2.0 s; `--tail-pad` 0.5 s; default mode `music`.
- Raw capture: 32-bit float, native rate/channels; `music` → `<stem>.raw.wav`, `meeting` → `<stem>.raw.caf`.
- Final output: `music` → WAV 24-bit PCM, native channels and rate; `meeting` → AAC `.m4a`, mono, 48 kHz, 64 kbps.
- Music raw WAV safe cap: stop gracefully at 4 000 000 000 bytes written.
- Default output path: `./recording-YYYYMMDD-HHMMSS.<wav|m4a>`.
- If no sound is detected: no final file, raw file kept, message with permission hint.
- If trimming/encoding fails: raw file kept and its path printed. Raw is deleted after success unless `--keep-raw`.
- All user-facing text and docs in English. License MIT.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean at the end of every task.

## Review Focus

1. **Output file already exists** → refuse with a clear error, never overwrite silently (test in Task 7).
2. **User-supplied WAV for `trim` that is 16/24-bit int or mono** → processed correctly, not rejected (test in Task 4).
3. **Recording stopped almost immediately / zero frames captured** → "no sound detected" path, no panic, no empty final file (tests in Tasks 2 and 6).
4. **Default output device changes mid-recording (e.g. AirPods connect)** → no crash; recording stops or continues, and the raw file is always finalised and trimmed (manual check in Task 8; documented as limitation in README).
5. **Ctrl+C pressed a second time while trimming** → process exits but raw file stays on disk and its path is printed (manual check in Task 8).

---

## File Structure

```
Cargo.toml
build.rs                     embeds Info.plist into the binary (__TEXT,__info_plist)
Info.plist                   NSAudioCaptureUsageDescription etc.
LICENSE                      MIT
README.md                    user documentation (English)
CLAUDE.md                    notes for future AI-assisted work
docs/core-audio-tap.md       low-level module explained
examples/capture_probe.rs    5 s capture → WAV, prints peak dBFS (manual check)
src/main.rs                  entry point: parse CLI, dispatch, map errors to exit codes
src/lib.rs                   module declarations
src/cli.rs                   clap definitions + command handlers
src/mode.rs                  Mode, Settings, extensions, raw path naming
src/trim.rs                  pure: loudness windows, event detection, trim range, fade gain
src/audio/mod.rs
src/audio/caf.rs             minimal CAF LPCM f32 writer/reader
src/audio/io.rs              RawWriter / RawReader over WAV and CAF (streaming)
src/encode.rs                FinalWriter: 24-bit WAV or mono CAF → afconvert → m4a
src/process.rs               two-pass streaming trim_file
src/recorder.rs              tap → ring buffer → writer thread, stop handling, meter, size cap
src/capture/mod.rs           safe API: SystemAudioTap, StreamFormat, CaptureError
src/capture/tap.rs           unsafe Core Audio FFI (tap, aggregate device, IOProc)
tests/trim_cli.rs            end-to-end test of the `trim` command
```

---

### Task 1: Toolchain, scaffold and capture prototype (go/no-go gate)

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `build.rs`, `Info.plist`, `src/lib.rs`, `src/main.rs` (stub), `src/capture/mod.rs`, `src/capture/tap.rs`, `examples/capture_probe.rs`, `docs/core-audio-tap.md`

**Interfaces:**
- Produces (in `src/capture/mod.rs`):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct StreamFormat { pub sample_rate: u32, pub channels: u16 }

  #[derive(Debug, thiserror::Error)]
  pub enum CaptureError {
      #[error("macOS 14.2 or later is required")] UnsupportedOs,
      #[error("Core Audio call `{call}` failed with status {status}")] CoreAudio { call: &'static str, status: i32 },
  }

  pub struct SystemAudioTap { /* tap id, aggregate device id, ioproc id, format */ }
  impl SystemAudioTap {
      /// `on_samples` runs on the real-time audio thread and receives interleaved f32 frames.
      pub fn start<F>(on_samples: F) -> Result<Self, CaptureError> where F: FnMut(&[f32]) + Send + 'static;
      pub fn format(&self) -> StreamFormat;
  }
  impl Drop for SystemAudioTap { /* stop + destroy in reverse order */ }
  ```

- [ ] **Step 1: Install the toolchain**

Run: `xcode-select -p || xcode-select --install`, then `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`, then `source "$HOME/.cargo/env" && rustc --version && cargo --version`.
Expected: Rust stable version printed (ask the user before running the installer if permissions block it).

- [ ] **Step 2: Scaffold the crate**

`cargo init --name map-recorder` (lib + bin: `src/lib.rs` declares `pub mod capture;`, `src/main.rs` prints "not implemented yet"). Add dependencies with `cargo add` (latest versions): `objc2`, `objc2-foundation`, `objc2-core-foundation`, `objc2-core-audio` (features covering `CATapDescription`, `AudioHardwareTapping`, `AudioHardware`, `AudioHardwareBase`), `block2`, `thiserror`, `anyhow`, `hound`. `.gitignore`: `/target`, `*.raw.wav`, `*.raw.caf`, `recording-*`.

- [ ] **Step 3: Embed Info.plist**

`Info.plist` with `CFBundleIdentifier` = `io.github.map-recorder`, `CFBundleName` = `map-recorder`, `NSAudioCaptureUsageDescription` = `map-recorder records the audio your Mac plays so it can save it to a file.`. `build.rs` passes `-sectcreate __TEXT __info_plist <abs path>/Info.plist` via `cargo:rustc-link-arg-bins=` and `cargo:rustc-link-arg-examples=`, plus `cargo:rerun-if-changed=Info.plist`.
Verify: after `cargo build --examples`, `otool -s __TEXT __info_plist target/debug/examples/capture_probe | head` shows bytes.

- [ ] **Step 4: Implement `SystemAudioTap::start` in `src/capture/tap.rs`**

Follow spec §7.2 steps 1–7 exactly: global stereo `CATapDescription` (empty exclusion list, private, `muteBehavior` = unmuted) → `AudioHardwareCreateProcessTap` → read tap UID (`kAudioTapPropertyUID`) and format (`kAudioTapPropertyFormat`) → private aggregate device (`kAudioAggregateDeviceIsPrivateKey` = 1, main sub-device = current default output device UID, `kAudioAggregateDeviceTapListKey` = [{`kAudioSubTapUIDKey`: tap UID, `kAudioSubTapDriftCompensationKey`: 1}], `kAudioAggregateDeviceTapAutoStartKey` = 1) → `AudioDeviceCreateIOProcIDWithBlock` → `AudioDeviceStart`. On any failure, undo what was created so far. The IOProc buffer list may be non-interleaved (one buffer per channel): interleave into a scratch `Vec<f32>` pre-allocated before start (capacity 16384 frames × channels); if a callback exceeds it, process in slices. Return `UnsupportedOs` if `CATapDescription` class is unavailable.

- [ ] **Step 5: Write `examples/capture_probe.rs`**

Starts a tap, collects samples for 5 s via an `rtrb` ring buffer (add `rtrb`), writes `probe.wav` (32-bit float) and prints `format`, frames captured and peak dBFS.
Run: `cargo run --example capture_probe` while music is playing.
Expected: no error; prints a format such as `48000 Hz, 2 ch`.

- [ ] **Step 6: MANUAL CHECKPOINT — hand off to the user**

Ask the user to: play music, run `cargo run --example capture_probe`, accept the "System Audio Recording" prompt (or enable the terminal in System Settings → Privacy & Security → Screen & System Audio Recording → System Audio Recording Only), re-run, then listen to `probe.wav`.
Pass: peak dBFS > −40 and `probe.wav` plays the music. **If it fails after troubleshooting, STOP the plan and report to the user (spec §7.5). Do not switch to ScreenCaptureKit.**

- [ ] **Step 7: Write `docs/core-audio-tap.md`**

English, for readers who don't know Core Audio: each step of spec §7.2 and why it exists, the permission model and Info.plist embedding (§7.3), real-time thread rules, teardown order, and "my recording is silent" debugging. Record anything learned in Step 6.

- [ ] **Step 8: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat: Core Audio process tap capture with probe example"
```

---

### Task 2: Modes and silence detection (pure logic)

**Files:**
- Create: `src/mode.rs`, `src/trim.rs`
- Modify: `src/lib.rs` (add modules)

**Interfaces:**
- Produces (`src/mode.rs`):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
  pub enum Mode { Music, Meeting }
  #[derive(Debug, Clone, Copy, PartialEq)]
  pub struct Settings { pub threshold_db: f32, pub max_lead_secs: f32, pub tail_pad_secs: f32 }
  impl Mode {
      pub fn defaults(self) -> Settings;              // Music: -60/2.0/0.5, Meeting: -40/2.0/0.5
      pub fn raw_extension(self) -> &'static str;     // "wav" / "caf"
      pub fn output_extension(self) -> &'static str;  // "wav" / "m4a"
  }
  /// `out/recording-1.wav` + Music → `out/recording-1.raw.wav`
  pub fn raw_path_for(output: &Path, mode: Mode) -> PathBuf;
  ```
- Produces (`src/trim.rs`):
  ```rust
  pub const WINDOW_SECS: f64 = 0.010;
  pub const MIN_EVENT_SECS: f64 = 0.050;
  pub const FADE_SECS: f64 = 0.005;
  pub struct LoudnessAnalyzer { /* sample_rate, channels, window accumulator */ }
  impl LoudnessAnalyzer {
      pub fn new(sample_rate: u32, channels: u16) -> Self;
      pub fn push(&mut self, interleaved: &[f32]);   // accepts any chunk size
      pub fn finish(self) -> Vec<f32>;                // dBFS per window; trailing partial window included; zero energy = f32::NEG_INFINITY
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct TrimRange { pub start_frame: u64, pub end_frame: u64 }  // end exclusive
  pub fn find_trim_range(window_db: &[f32], total_frames: u64, sample_rate: u32, settings: &Settings) -> Option<TrimRange>;
  /// Gain for frame `i` (0-based within the kept range): linear ramp up over the first `fade_frames`, down over the last `fade_frames`, 1.0 elsewhere.
  pub fn fade_gain(i: u64, range_len: u64, fade_frames: u64) -> f32;
  ```

Algorithm for `find_trim_range` (not determined by signature): `wf = round(sample_rate × WINDOW_SECS)`; `min_windows = ceil(MIN_EVENT_SECS / WINDOW_SECS)` (= 5); find runs of windows with `db >= threshold_db` of length ≥ `min_windows`; `None` if none. `first = first_run_start × wf`, `last = min(total_frames, last_run_end_exclusive × wf)`; `start = first.saturating_sub(round(max_lead × sr))`; `end = min(total_frames, last + round(tail_pad × sr))`.

- [ ] **Step 1: Write failing tests** (in `src/trim.rs` and `src/mode.rs` `#[cfg(test)]`), using a helper `tone(secs, amp)` (440 Hz sine, 48 kHz stereo interleaved) and `silence(secs)`; analyse with `LoudnessAnalyzer`:

```rust
#[test] fn defaults_per_mode() {
    assert_eq!(Mode::Music.defaults(), Settings { threshold_db: -60.0, max_lead_secs: 2.0, tail_pad_secs: 0.5 });
    assert_eq!(Mode::Meeting.defaults().threshold_db, -40.0);
}
#[test] fn raw_path_naming() {
    assert_eq!(raw_path_for(Path::new("out/rec.wav"), Mode::Music), PathBuf::from("out/rec.raw.wav"));
    assert_eq!(raw_path_for(Path::new("rec.m4a"), Mode::Meeting), PathBuf::from("rec.raw.caf"));
}
#[test] fn long_lead_is_cut_to_two_seconds_and_tail_to_half_second() {
    // 5 s silence + 3 s tone(0.5) + 4 s silence, music
    assert_eq!(range, Some(TrimRange { start_frame: 3 * 48_000, end_frame: 8 * 48_000 + 24_000 }));
}
#[test] fn short_lead_is_kept_entirely() { /* 1 s silence + tone → start_frame == 0 */ }
#[test] fn isolated_click_in_tail_is_ignored() { /* tone 2 s, silence 3 s, 20 ms tone(0.9), silence 2 s → end_frame == 2 s + 0.5 s */ }
#[test] fn quiet_fade_counts_as_sound_in_music_but_not_meeting() { /* tone 2 s, 2 s tone at amp 0.0018 (~ -55 dBFS RMS), silence 2 s → music end = 4.5 s, meeting end = 2.5 s */ }
#[test] fn all_silence_returns_none() {}
#[test] fn zero_frames_returns_none() { /* analyzer with no push → empty vec → None, no panic */ }
#[test] fn sound_until_the_end_keeps_the_end() { /* silence 1 s + tone 2 s → end_frame == total_frames */ }
#[test] fn sound_in_one_channel_only_is_detected() {}
#[test] fn chunking_does_not_change_result() { /* same signal pushed whole vs in chunks of 333 samples → equal vectors */ }
#[test] fn fade_gain_ramps() {
    assert_eq!(fade_gain(0, 1000, 240), 0.0);
    assert_eq!(fade_gain(500, 1000, 240), 1.0);
    assert!(fade_gain(999, 1000, 240) < 0.01);
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib` → compile errors (items missing).
- [ ] **Step 3: Implement `mode.rs` and `trim.rs`** per the interfaces (add `clap` with `derive` feature now).
- [ ] **Step 4: Run, expect PASS** — `cargo test --lib`.
- [ ] **Step 5: Commit** — `git commit -m "feat: modes and silence detection"`.

---

### Task 3: Minimal CAF writer/reader

**Files:**
- Create: `src/audio/mod.rs`, `src/audio/caf.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct CafWriter { /* BufWriter<File>, data start offset, bytes written */ }
  impl CafWriter {
      pub fn create(path: &Path, sample_rate: u32, channels: u16) -> io::Result<Self>;
      pub fn write_samples(&mut self, interleaved: &[f32]) -> io::Result<()>;
      pub fn bytes_written(&self) -> u64;          // audio bytes only
      pub fn finalize(self) -> io::Result<()>;     // patches data chunk size
  }
  pub struct CafReader { /* BufReader<File>, sample_rate, channels, total_frames */ }
  impl CafReader {
      pub fn open(path: &Path) -> io::Result<Self>;
      pub fn sample_rate(&self) -> u32;
      pub fn channels(&self) -> u16;
      pub fn total_frames(&self) -> u64;
      /// Appends up to `max_frames` frames to `buf` (cleared first); returns frames read, 0 at EOF.
      pub fn read_frames(&mut self, buf: &mut Vec<f32>, max_frames: usize) -> io::Result<usize>;
  }
  ```

Format (all header fields big-endian, Apple CAF spec): file header `caff`, version 1, flags 0; `desc` chunk size 32: f64 sample rate, `lpcm`, format flags 3 (float | little-endian), bytesPerPacket = 4×channels, framesPerPacket 1, channelsPerFrame, bitsPerChannel 32; `data` chunk: size written as −1 (unknown) at create, followed by u32 edit count 0, then LE f32 samples; `finalize` seeks back and writes `4 + audio bytes`. Reader: walks chunks, rejects anything that isn't LPCM float32 LE (`io::ErrorKind::InvalidData`), and when data size is −1 derives it from file length.

- [ ] **Step 1: Write failing tests** (with `tempfile`, add as dev-dependency):
```rust
#[test] fn round_trip_stereo() { /* write 10_000 frames 48 kHz stereo, read in chunks of 4096 → identical samples, total_frames == 10_000 */ }
#[test] fn round_trip_mono() {}
#[test] fn unfinalized_file_is_readable() { /* write, drop writer without finalize (simulated crash; flush the BufWriter) → reader total_frames == frames written */ }
#[test] fn rejects_non_caf() { /* open a WAV → InvalidData */ }
#[test] fn afconvert_accepts_our_caf() { /* `afinfo <file>` exits 0 and stdout contains "Float32" or "32-bit float"; mark #[cfg(target_os = "macos")] */ }
```
- [ ] **Step 2: Run, expect FAIL** — `cargo test --lib caf`.
- [ ] **Step 3: Implement** per interface and format above.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: minimal CAF float writer and reader`.

---

### Task 4: Streaming raw I/O over WAV and CAF

**Files:**
- Create: `src/audio/io.rs`

**Interfaces:**
- Consumes: `CafWriter`, `CafReader` (Task 3); `StreamFormat` (Task 1).
- Produces:
  ```rust
  pub enum RawWriter { Wav(hound::WavWriter<BufWriter<File>>), Caf(CafWriter) }
  impl RawWriter {
      /// Picks the container from the extension (`wav` | `caf`); 32-bit float.
      pub fn create(path: &Path, format: StreamFormat) -> anyhow::Result<Self>;
      pub fn write(&mut self, interleaved: &[f32]) -> anyhow::Result<()>;
      pub fn bytes_written(&self) -> u64;
      pub fn finalize(self) -> anyhow::Result<()>;
  }
  pub struct RawReader { /* enum inner: hound reader or CafReader */ }
  impl RawReader {
      /// WAV: accepts f32 and 16/24/32-bit int, converted to f32 in [-1, 1]. CAF: as CafReader.
      pub fn open(path: &Path) -> anyhow::Result<Self>;
      pub fn format(&self) -> StreamFormat;
      pub fn total_frames(&self) -> u64;
      pub fn read_frames(&mut self, buf: &mut Vec<f32>, max_frames: usize) -> anyhow::Result<usize>;
  }
  ```

- [ ] **Step 1: Write failing tests:**
```rust
#[test] fn wav_float_round_trip() {}
#[test] fn caf_round_trip_via_raw_types() {}
#[test] fn reads_int16_mono_wav() { /* hound-written i16 mono, sample 16384 → ~0.5 */ }
#[test] fn reads_int24_stereo_wav() { /* sample 4_194_304 → ~0.5 */ }
#[test] fn unknown_extension_is_an_error() { /* "x.mp3" → Err, message contains "wav" and "caf" */ }
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: streaming raw audio reader/writer`.

---

### Task 5: Final output encoding

**Files:**
- Create: `src/encode.rs`

**Interfaces:**
- Consumes: `Mode` (Task 2), `CafWriter` (Task 3), `StreamFormat` (Task 1).
- Produces:
  ```rust
  pub struct FinalWriter { /* Music: hound 24-bit writer; Meeting: mono CafWriter to temp + final path */ }
  impl FinalWriter {
      /// Music: 24-bit PCM WAV, same rate/channels as `format`.
      /// Meeting: mono f32 CAF temp file `<final>.tmp.caf` (channel average).
      pub fn create(mode: Mode, final_path: &Path, format: StreamFormat) -> anyhow::Result<Self>;
      pub fn write(&mut self, interleaved: &[f32]) -> anyhow::Result<()>;
      /// Music: finalize WAV. Meeting: finalize temp, run
      /// `afconvert -f m4af -d aac@48000 -b 64000 -c 1 <tmp> <final>`, delete temp (also on failure).
      pub fn finish(self) -> anyhow::Result<PathBuf>;
  }
  ```
24-bit conversion: clamp to [−1, 1], `(x * 8_388_607.0).round() as i32`.

- [ ] **Step 1: Write failing tests:**
```rust
#[test] fn music_writes_24bit_stereo_wav() { /* spec: bits_per_sample 24, Int, channels 2, rate 48000, len == frames*2 */ }
#[test] fn music_clamps_out_of_range_samples() { /* 1.5 → 8_388_607 */ }
#[test] fn meeting_writes_mono_m4a() { /* 1 s stereo tone 44.1 kHz → .m4a exists; `afinfo` output contains "aac" (case-insensitive), "1 ch", "48000"; no .tmp.caf left */ }
#[test] fn meeting_cleans_temp_on_afconvert_failure() { /* final path in a non-existent directory → Err, no temp left */ }
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: final output encoding per mode`.

---

### Task 6: Two-pass streaming trim

**Files:**
- Create: `src/process.rs`

**Interfaces:**
- Consumes: `RawReader` (Task 4), `LoudnessAnalyzer`, `find_trim_range`, `fade_gain`, `FADE_SECS` (Task 2), `FinalWriter` (Task 5), `Mode`, `Settings`.
- Produces:
  ```rust
  #[derive(Debug, PartialEq)]
  pub enum TrimOutcome {
      Written { path: PathBuf, kept_secs: f64, removed_lead_secs: f64, removed_tail_secs: f64 },
      NoSound,
  }
  pub const CHUNK_FRAMES: usize = 4096;
  pub fn trim_file(input: &Path, output: &Path, mode: Mode, settings: &Settings) -> anyhow::Result<TrimOutcome>;
  ```
Pass 1: open, feed all chunks to `LoudnessAnalyzer`, compute range. `None` → `NoSound` (nothing written). Pass 2: reopen, read and discard up to `start_frame`, write frames until `end_frame` multiplied by `fade_gain` (fade frames = round(sr × FADE_SECS)), `finish`.

- [ ] **Step 1: Write failing tests** (generate inputs with `RawWriter`):
```rust
#[test] fn music_trims_lead_and_tail() { /* 5 s silence + 3 s tone + 4 s silence, raw .wav → Written, output frames == 5.5 s × 48000, removed_lead_secs ≈ 3.0, removed_tail_secs ≈ 3.5 */ }
#[test] fn cut_points_are_faded() { /* first output sample == 0, sample at 1 s from start not attenuated */ }
#[test] fn meeting_from_caf_produces_m4a() {}
#[test] fn silent_input_writes_nothing() { /* NoSound, output path does not exist */ }
#[test] fn empty_input_writes_nothing() { /* raw with 0 frames → NoSound */ }
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat: two-pass streaming trim`.

---

### Task 7: CLI skeleton and `trim` command

**Files:**
- Create: `src/cli.rs`, `tests/trim_cli.rs`
- Modify: `src/main.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: `trim_file`, `TrimOutcome` (Task 6), `Mode`, `Settings` (Task 2).
- Produces:
  ```rust
  #[derive(clap::Parser)] #[command(name = "map-recorder", version, about)]
  pub struct Cli { #[command(subcommand)] pub command: Command }
  #[derive(clap::Subcommand)]
  pub enum Command { Record(RecordArgs), Trim(TrimArgs) }
  #[derive(clap::Args)]
  pub struct TuningArgs {           // flattened into both commands
      #[arg(long, value_enum, default_value = "music")] pub mode: Mode,
      #[arg(long)] pub threshold_db: Option<f32>,
      #[arg(long)] pub max_lead: Option<f32>,
      #[arg(long)] pub tail_pad: Option<f32>,
  }
  impl TuningArgs { pub fn settings(&self) -> Settings; } // mode defaults overridden by provided values
  #[derive(clap::Args)] pub struct TrimArgs { pub input: PathBuf, #[arg(short, long)] pub output: Option<PathBuf>, #[command(flatten)] pub tuning: TuningArgs }
  #[derive(clap::Args)] pub struct RecordArgs { #[arg(short, long)] pub output: Option<PathBuf>, #[arg(long)] pub keep_raw: bool, #[command(flatten)] pub tuning: TuningArgs }
  /// `recording-YYYYMMDD-HHMMSS.<ext>` in the current directory (add `chrono`).
  pub fn default_output_path(mode: Mode, now: chrono::DateTime<chrono::Local>) -> PathBuf;
  pub fn run(cli: Cli) -> anyhow::Result<()>;
  ```
`trim` default output: `<input stem>.trimmed.<ext>` beside the input. Existing output → error `Output file <path> already exists`. `NoSound` → print `No sound detected (threshold <x> dBFS). Nothing written.` and exit code 2. `Written` → print path and kept/removed seconds. `Record` arm: `todo!()` placeholder replaced in Task 8 — print "not implemented yet" and exit 1 instead of panicking. `main.rs` prints `error: {:#}` and exits 1 on `Err`.

- [ ] **Step 1: Write failing tests** — unit tests in `cli.rs` and integration in `tests/trim_cli.rs` (run the binary via `env!("CARGO_BIN_EXE_map-recorder")`):
```rust
#[test] fn settings_override_mode_defaults() { /* --mode meeting --threshold-db -50 → Settings{-50, 2.0, 0.5} */ }
#[test] fn default_output_path_format() { /* 2026-11-26 16:09:05, Music → "recording-20261126-160905.wav" */ }
#[test] fn trim_command_end_to_end() { /* generated WAV → exit 0, <stem>.trimmed.wav exists, stdout mentions it */ }
#[test] fn trim_refuses_to_overwrite() { /* output exists → non-zero exit, stderr contains "already exists", file untouched */ }
#[test] fn trim_silent_file_exits_2() {}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS** — `cargo test`.
- [ ] **Step 5: Commit** — `feat: CLI with trim command`.

---

### Task 8: `record` command

**Files:**
- Create: `src/recorder.rs`
- Modify: `src/cli.rs` (Record arm), `src/lib.rs`, `Cargo.toml` (`rtrb`, `ctrlc`)

**Interfaces:**
- Consumes: `SystemAudioTap`, `StreamFormat` (Task 1), `RawWriter` (Task 4), `trim_file`, `TrimOutcome` (Task 6), `raw_path_for`, `Mode`, `Settings` (Task 2), `RecordArgs`, `default_output_path` (Task 7).
- Produces:
  ```rust
  pub const WAV_SAFE_LIMIT_BYTES: u64 = 4_000_000_000;
  pub const RING_SECONDS: u32 = 10;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum StopReason { User, SizeLimit, Error }
  pub struct RecordOptions { pub mode: Mode, pub settings: Settings, pub output: PathBuf, pub keep_raw: bool }
  pub struct RecordReport { pub raw_path: PathBuf, pub duration_secs: f64, pub overruns: u64, pub stop_reason: StopReason, pub outcome: Option<TrimOutcome> }
  pub fn record(opts: &RecordOptions) -> anyhow::Result<RecordReport>;
  /// e.g. "● REC 00:03:27  [#########-----------]  -18.4 dBFS   (press Enter to stop)"
  pub fn format_status(elapsed: Duration, level_db: f32) -> String;
  pub fn size_limit_reached(mode: Mode, bytes_written: u64) -> bool; // true only for Music at >= WAV_SAFE_LIMIT_BYTES
  ```
Flow: refuse if output or raw path exists → install `ctrlc` handler setting a shared `AtomicBool` stop flag (a second Ctrl+C after the flag is already set prints the raw path and calls `std::process::exit(130)`) → stdin thread sets the flag on any line → start tap with a callback that pushes into an `rtrb` producer sized `RING_SECONDS × rate × channels` (failed pushes increment an `AtomicU64` overrun counter; store the chunk peak as `f32` bits in an `AtomicU32` for the meter) → writer thread drains into `RawWriter`, checks `size_limit_reached` → main thread redraws `format_status` with `\r` every 100 ms until stop → drop tap, join writer, finalize raw → `trim_file` → delete raw only on `Written` and `!keep_raw`; on `Err` from trimming keep raw and print its path. CLI prints a summary; `NoSound` prints the permission hint pointing to the README troubleshooting section; `SizeLimit` prints a warning; overruns > 0 print a warning.

- [ ] **Step 1: Write failing tests:**
```rust
#[test] fn status_line_shows_time_and_level() { /* 207 s, -18.4 → contains "00:03:27" and "-18.4 dBFS" */ }
#[test] fn status_line_handles_silence() { /* NEG_INFINITY → contains "-inf" or "silence", no panic */ }
#[test] fn size_limit_only_for_music() {
    assert!(size_limit_reached(Mode::Music, WAV_SAFE_LIMIT_BYTES));
    assert!(!size_limit_reached(Mode::Music, WAV_SAFE_LIMIT_BYTES - 1));
    assert!(!size_limit_reached(Mode::Meeting, 10 * WAV_SAFE_LIMIT_BYTES));
}
```
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement `recorder.rs` and wire the Record arm.**
- [ ] **Step 4: Run, expect PASS** — `cargo test`.
- [ ] **Step 5: MANUAL CHECK with the user** — (a) `cargo run -- record`, wait ~5 s, play a song for 20 s, stop music, wait 10 s, press Enter → final WAV starts ≤ 2 s before the music and ends ≈ 0.5 s after it. (b) Same with `--mode meeting` and a video call/YouTube speech → `.m4a` produced. (c) Ctrl+C stops and saves. (d) Ctrl+C twice during trimming of a long recording → raw path printed, raw file exists. (e) Connect/disconnect headphones mid-recording → no crash, raw file finalised. Record results (especially e) in `CLAUDE.md` and the README limitations.
- [ ] **Step 6: Commit** — `feat: record command`.

---

### Task 9: Documentation and release polish

**Files:**
- Create: `README.md`, `LICENSE` (MIT, year 2026, holder: ask the user what name to use), `CLAUDE.md`
- Modify: `docs/core-audio-tap.md` (final review)

- [ ] **Step 1: README.md** (English) with sections: pitch; features; requirements (macOS 14.2+, Apple Silicon tested); install (rustup, `cargo install --git <repo-url>`, build from source); first run & permission walkthrough; usage examples (`record`, `record --mode meeting -o call.m4a`, `trim raw.wav --threshold-db -50`); how trimming works with an ASCII timeline; options table (exact defaults from Global Constraints); output formats table; troubleshooting (silent recording → permission; overruns); limitations (≈3 h music cap, output device change, no mic); roadmap (transcription, Linux); development (`cargo test`, probe example, link to `docs/core-audio-tap.md`); license.
- [ ] **Step 2: CLAUDE.md** — build/test/lint commands, module map (one line each), real-time rules, `unsafe` confinement, comment style rule, manual checks that can't be automated, gotchas discovered in Tasks 1 and 8.
- [ ] **Step 3: Verify** — `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` all pass; `cargo run -- --help` and `cargo run -- record --help` text matches the README options table.
- [ ] **Step 4: Commit** — `docs: README, CLAUDE.md, license`.
