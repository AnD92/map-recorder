# map-recorder — System Audio Recorder CLI (Design)

Date: 2026-11-26
Status: Draft, awaiting approval

## 1. Intent

A command-line tool for macOS (Apple Silicon, developed on an M4) that records
**everything the Mac plays through its audio output** and, when the recording
stops, automatically cleans the file up:

- leading silence is shortened so the first sound starts **at most 2 seconds**
  into the file;
- trailing silence (the part after the last sound) is **removed**.

Why: the primary use is recording music; the secondary use is recording
meetings that will later be transcribed. In both cases the user does not want
to babysit start/stop timing — they start the tool, let things play, stop it,
and get a tidy file.

The project will be published as a **public GitHub repository**, so the
README (English) and code quality matter: other people must be able to
understand what it does, install it, and use it.

## 2. Goals

- Record system audio output on macOS with no extra drivers (no BlackHole,
  no Multi-Output device setup).
- Start and stop from the terminal.
- Two modes with different silence sensitivity and output format:
  `music` and `meeting`.
- Automatic trimming of leading/trailing silence after stop.
- A clear English README: what it does, requirements, install, usage,
  permissions, how trimming works, limitations.
- Professional code comments that explain the **purpose** of each block
  (not line-by-line narration of trivial constructs).
- A dedicated document explaining how the low-level Core Audio module works
  (see §7).

## 3. Non-goals (for now)

- Linux or Windows support.
- Transcription (planned future feature for `meeting` mode).
- Recording the microphone, or mixing mic + system audio.
- Per-application capture (the API allows it; not in scope for v1).
- A GUI or menu-bar app.
- Editing beyond lead/tail silence trimming (no silence removal in the middle,
  no normalisation, no splitting into tracks).

## 4. Anti-goals (would be a failure even if it "works")

- Cutting real audio: quiet fade-outs at the end of a song, or the last word
  of a meeting, must not be chopped off.
- Losing a recording: if the trimming/encoding step fails, or the process is
  interrupted, the raw capture must still be on disk.
- Glitches/dropouts in the recording caused by doing heavy work on the
  real-time audio thread.
- Requiring the user to grant "Screen Recording" permission for an audio tool.

## 5. Constraints

- Language: **Rust** (stable toolchain). The user is not a Rust expert:
  prefer readable code over clever code; keep `unsafe` confined to one module.
- Platform: **macOS 14.2+** (Core Audio process taps). Developed on macOS 26
  on Apple Silicon. Intel Macs are not a target but nothing should
  deliberately exclude them.
- No external runtime dependencies beyond what ships with macOS.
  (Rust, Cargo and Xcode Command Line Tools are needed to build.)
- Nothing is installed on the machine yet (no Rust toolchain). Installing
  `rustup` is part of setup.

## 6. Approaches considered

| Approach | Trade-offs | Decision |
|---|---|---|
| **A. Core Audio Process Taps** (`AudioHardwareCreateProcessTap` + private aggregate device) | Purpose-built for capturing system audio; dedicated "System Audio Recording Only" permission; no screen-recording indicator; full quality. Rust bindings exist (`objc2-core-audio` 0.3.x exposes `CATapDescription`, `AudioHardwareCreateProcessTap`, `AudioHardwareCreateAggregateDevice`) but require some `unsafe` glue code. | **Chosen** by the user. |
| B. ScreenCaptureKit (via `screencapturekit` crate) | Higher-level, well-maintained crate; simpler code. But requires the **Screen Recording** permission and shows the screen-recording indicator — off-putting for an audio tool. | Fallback if A proves unworkable. Everything above the capture module stays the same. |
| C. Virtual driver (BlackHole) + `cpal` | Simplest Rust code; but users must install a driver and configure a Multi-Output device by hand. | Rejected: bad experience for a public tool. |

## 7. Architecture

```
┌────────────┐   commands    ┌──────────────┐
│  cli.rs    │──────────────▶│ recorder.rs  │  orchestrates a session
└────────────┘               └──────┬───────┘
                                    │ start/stop
                    ┌───────────────▼──────────────┐
                    │ capture/ (macOS, unsafe FFI) │  Core Audio tap
                    │   real-time IOProc callback  │
                    └───────────────┬──────────────┘
                                    │ f32 frames via lock-free ring buffer
                    ┌───────────────▼──────────────┐
                    │ writer thread → raw WAV      │  streaming to disk
                    └───────────────┬──────────────┘
                                    │ on stop
                    ┌───────────────▼──────────────┐
                    │ trim.rs (pure, unit-tested)  │  find sound bounds
                    └───────────────┬──────────────┘
                    ┌───────────────▼──────────────┐
                    │ encode.rs                    │  final file per mode
                    └──────────────────────────────┘
```

### 7.1 Modules

- **`cli`** — argument parsing (`clap`), user-facing messages.
- **`capture`** — the only module with `unsafe`. Wraps the Core Audio process
  tap behind a small safe API, e.g. `SystemAudioTap::start(callback) -> Result<Tap>`
  and `Drop` that tears everything down. Exposes the stream format
  (sample rate, channel count).
- **`recorder`** — wires capture → ring buffer → writer thread; handles
  stop signals; returns the path of the raw file.
- **`trim`** — pure logic over a stream of per-window loudness values:
  locate first/last sound, compute trim range. No I/O, no macOS code →
  fully unit-testable with synthetic signals, portable to Linux later.
  Trimming is **streaming, two-pass**: pass 1 reads the raw file chunk by
  chunk and computes window loudness; pass 2 copies only the chosen range
  to the output. Memory stays flat regardless of recording length.
- **`encode`** — writes the final file according to the mode.
- **`mode`** — the `music`/`meeting` presets (thresholds, format).

### 7.2 Low-level capture flow (to be documented in `docs/core-audio-tap.md`)

The implementer must write `docs/core-audio-tap.md` explaining, for a reader
who doesn't know Core Audio, each of these steps and why it's needed:

1. **Describe the tap** — create a `CATapDescription` for a *global stereo tap*
   that captures all processes (initialised with an empty exclusion list;
   our own process excluded is fine since it plays nothing). Mark it
   private and give it a UUID.
2. **Create the tap** — `AudioHardwareCreateProcessTap` returns an
   `AudioObjectID` for the tap. This is the call that triggers the
   macOS permission prompt.
3. **Create a private aggregate device** — `AudioHardwareCreateAggregateDevice`
   with a dictionary that: is private (not visible in System Settings),
   uses the current default output device as main sub-device / clock source,
   and lists the tap's UUID in its tap list. Taps can only be read through
   a device, which is why the aggregate exists.
4. **Read the stream format** — query the tap's format
   (`kAudioTapPropertyFormat`) to learn sample rate and channel layout.
5. **Install the IOProc** — `AudioDeviceCreateIOProcIDWithBlock` on the
   aggregate device. The block runs on a **real-time thread**: it must only
   copy samples into the lock-free ring buffer (no allocation, no locks,
   no I/O, no logging).
6. **Start** — `AudioDeviceStart`.
7. **Teardown (reverse order, always, also on error/Ctrl+C)** —
   `AudioDeviceStop`, `AudioDeviceDestroyIOProcID`,
   `AudioHardwareDestroyAggregateDevice`, `AudioHardwareDestroyProcessTap`.

The doc should also cover: the permission model (below), the real-time
thread rules, and how to debug when the capture returns only silence.

### 7.3 Permission

macOS 14.2+ gates taps behind the "System Audio Recording" privacy
permission (TCC). For a CLI, macOS attributes the request to the
*responsible* app (e.g. Terminal, iTerm, VS Code). Known risk: a
command-line binary without an `Info.plist` containing
`NSAudioCaptureUsageDescription` may receive silent buffers instead of a
prompt. Mitigation: embed an `Info.plist` in the binary (linker section
`__TEXT,__info_plist` via `build.rs`). **First implementation task is to
verify capture produces non-silent audio** on the dev machine before
building anything on top; if this cannot be made to work, fall back to
approach B and tell the user.

The tool should detect the "all silence for the whole session while
something was expected" case and print a hint about permissions.

### 7.4 Real-time safety and data safety

- IOProc → `rtrb` (lock-free SPSC ring buffer) → writer thread.
  Ring buffer sized for several seconds of audio; overruns are counted and
  reported at the end, never block the audio thread.
- Writer thread streams the raw capture (32-bit float, native rate and
  channels) to disk next to the output. Streaming keeps memory flat and
  means a crash still leaves the audio on disk. The raw container depends
  on the mode (decided by the user):
  - `music` → **WAV** (`<name>.raw.wav`). WAV is capped at 4 GB, i.e.
    roughly **3 hours** of 48 kHz stereo float. Accepted limit for music:
    the recorder watches the size and, shortly before the cap, stops
    gracefully, warns the user and proceeds to trimming. Documented in
    the README.
  - `meeting` → **CAF** (`<name>.raw.caf`), Apple's Core Audio Format,
    which has no practical size limit, so meetings can run for many hours.
    CAF is written with a data chunk of "unknown" size while recording and
    the size is patched on close; a small hand-written CAF reader/writer
    (LPCM float only) is acceptable since `hound` only handles WAV.
- After the final file is written successfully, the raw file is deleted
  unless `--keep-raw` is passed. If trimming/encoding fails, the raw file is
  kept and its path printed.

### 7.5 If process taps don't work

If the first task shows the tap cannot produce real (non-silent) audio on
the dev machine, **the builder stops and reports to the user** instead of
switching to ScreenCaptureKit on their own. The user then decides whether
the Screen Recording permission is acceptable. *(My call, consistent with
the anti-goal in §4.)*

### 7.6 Manual verification checkpoint

The first implementation task is a capture prototype followed by a
**hand-off to the user**: grant the "System Audio Recording" permission to
the terminal app, play some music, record, confirm the file isn't silent.
Nothing is built on top of capture before that check passes.

## 8. Behaviour

### 8.1 Commands

```
map-recorder record [--mode music|meeting] [-o <path>] [options]
map-recorder trim <input.wav|input.caf> [--mode music|meeting] [-o <path>] [options]
```

- `record` — starts capturing immediately. Shows elapsed time and a simple
  live level meter on one line. **Stops when the user presses Enter**
  (or `q` + Enter). **Ctrl+C also stops gracefully** and still saves.
- `trim` — applies the same post-processing to an existing WAV. Useful for
  re-processing a kept raw file with different thresholds, and for testing
  the trimming without capturing. *(Builder-facing addition; my call.)*
- Default mode: `music`.
- Default output path: `~/Music/map-recorder/recording-YYYYMMDD-HHMMSS.<ext>`
  (folder created automatically). The folder can be changed with the
  `MAP_RECORDER_DIR` environment variable; `-o` overrides it per recording.
  *(Changed 2026-11-26 at the user's request; was the current directory.)*

### 8.2 Options (both commands)

| Option | Default (music / meeting) | Meaning |
|---|---|---|
| `--threshold-db <dB>` | `-60` / `-40` dBFS | Loudness below this counts as silence |
| `--max-lead <sec>` | `2.0` | Max silence kept before the first sound |
| `--tail-pad <sec>` | `0.5` | Silence kept after the last sound |
| `--keep-raw` | off | Keep the untrimmed raw WAV (`record` only) |

### 8.3 Silence detection (both modes)

- Signal is analysed in **10 ms windows**; loudness = RMS across all
  channels, in dBFS.
- A window is "sound" if its loudness ≥ threshold.
- To avoid a single click or pop counting as sound, a **sound event requires
  at least 50 ms of consecutive sound windows** *(my call)*.
- **First sound** = start of the first sound event. **Last sound** = end of
  the last sound event.
- Output range = `[max(0, first − max_lead), min(len, last + tail_pad)]`.
  If the leading silence is already ≤ `max_lead`, nothing is cut at the start.
- If no sound is detected at all: no final file is written, the raw file is
  kept, and the tool explains why (and hints at the permission issue).
- Cuts happen at sample boundaries; apply a very short fade (≈5 ms) at each
  cut point to avoid clicks *(my call)*.

### 8.4 Modes

| | `music` | `meeting` |
|---|---|---|
| Purpose | Music, faithful archive | Spoken audio, later transcription |
| Silence threshold | **strict**: −60 dBFS (only near-digital silence is silence, so quiet passages and fade-outs survive) | **lenient**: −40 dBFS (room tone / hum counts as silence) |
| Raw capture | WAV, 32-bit float (≈3 h cap) | CAF, 32-bit float (no practical cap) |
| Output | **WAV, 24-bit PCM, stereo, native sample rate** (lossless; same ≈4 h cap, unreachable given the raw cap) | **AAC in `.m4a`, mono, 48 kHz, ~64 kbps** |

Meeting encoding uses macOS's built-in `afconvert` (no extra dependencies):
trim → write temporary mono CAF → `afconvert -f m4af -d aac -b 64000`.
*(My call: avoids bundling an AAC encoder in Rust; `afconvert` ships with
every macOS.)* Downmix to mono = average of channels.

Thresholds are starting points; they're exposed as options and documented
so users can tune them.

## 9. Repository, docs and code style

- Public GitHub repo; license: **MIT** *(my call; easy to change)*.
- `README.md` (English): one-paragraph pitch; features; requirements
  (macOS 14.2+); install (`rustup`, `cargo install --git …` / build from
  source); first-run permission walkthrough; usage examples for both modes;
  how trimming works (with a small timeline diagram); options table;
  troubleshooting (silent recordings → permissions); limitations and roadmap
  (transcription, Linux).
- `docs/core-audio-tap.md`: the low-level module explained (see §7.2).
- `CLAUDE.md`: notes for future AI-assisted work (build/test commands,
  module map, real-time rules, gotchas found during development).
- Comments: explain the **purpose** of each logical block and every `unsafe`
  block's safety reasoning (`// SAFETY:`). Don't narrate trivial code.
- `cargo fmt` and `cargo clippy` clean.

## 10. Testing

- CAF reader/writer: round-trip tests, including a file whose data size was
  left "unknown" (simulated crash).
- `trim`: unit tests with synthetic signals (silence + tone + silence, fade
  tails, isolated clicks, all-silence, sound from sample 0, sound until the
  end, multichannel).
- `encode`: test that WAV output has the expected format and length;
  meeting path tested by running `afconvert` in an integration test (macOS only).
- `trim` CLI command: integration test end-to-end on a generated WAV.
- `capture`: cannot be unit-tested meaningfully; verified manually
  (play audio, record, confirm non-silent output). Steps go in the README's
  development section.

## 11. Left to the builder

- Exact crate choices beyond those named (`objc2-core-audio`, `rtrb`,
  `hound` for WAV, `clap`, `ctrlc` are suggested).
- Ring buffer size, live meter appearance and refresh rate.
- Internal error types and message wording.
- How the default output device change mid-recording is handled (acceptable
  for v1: document as a limitation).
