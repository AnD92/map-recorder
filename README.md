# map-recorder

**Record everything your Mac plays — then trim the silence around it.**

`map-recorder` is a small command-line tool for macOS that captures the system
audio output (music apps, browsers, video calls… anything you can hear) and
saves it to a file. When you stop the recording it automatically cleans it up:

- the silence **before** the first sound is shortened to **at most 2 seconds**;
- the silence **after** the last sound is **removed** (a short half-second tail is kept so nothing is cut abruptly).

No virtual audio drivers, no Multi-Output devices, no extra software: it uses
the native Core Audio *process tap* API introduced in macOS 14.2.

```
$ map-recorder record
Recording system audio (48000 Hz, 2 ch, music mode) → ~/Music/map-recorder/recording-20261126-160905.wav
● REC 00:03:27  [##############------]  -18.4 dBFS   (press Enter to stop)
Trimming silence…
Recorded 237.6 s of audio.
Saved /Users/you/Music/map-recorder/recording-20261126-160905.wav
  kept 205.1 s, removed 2.0 s at the start and 30.5 s at the end
```

## Features

- 🎧 **System audio capture** without drivers, via Core Audio process taps.
- ✂️ **Automatic trimming** of leading and trailing silence.
- 🎼 **Two modes**:
  - `music` — strict silence detection (quiet passages and fade-outs are preserved), lossless **24-bit WAV**;
  - `meeting` — lenient silence detection (room noise counts as silence), compact **AAC `.m4a`**, mono, ready for transcription tools.
- 📈 Live elapsed time and level meter while recording.
- 🛟 **Safe by design**: audio is streamed to disk and the file is kept valid every second, so even a crash or a closed terminal leaves a usable file; the untrimmed capture is kept whenever something goes wrong, and a file with the final name is always complete.
- 🔁 `trim` command to re-process any `.wav` / `.caf` file with different settings.

## Requirements

- **macOS 14.2 (Sonoma) or later.** Developed and tested on macOS 26 with Apple Silicon (M4).
- To build from source: the [Rust toolchain](https://rustup.rs) and the Xcode Command Line Tools (`xcode-select --install`).

## Installation

Install Rust if you don't have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install `map-recorder` directly from GitHub:

```sh
cargo install --git https://github.com/AnD92/map-recorder
```

or build it from a clone:

```sh
git clone https://github.com/AnD92/map-recorder
cd map-recorder
cargo build --release
./target/release/map-recorder --help
```

## First run: grant the permission

macOS protects system audio behind the **System Audio Recording** privacy
permission. The permission is granted to the **terminal app** you run
`map-recorder` from (Terminal, iTerm2, VS Code, …).

1. Run `map-recorder record` once. macOS should ask whether your terminal may record system audio — click **Allow**.
2. If no prompt appears, open **System Settings → Privacy & Security → Screen & System Audio Recording**, and enable your terminal under **System Audio Recording Only**.
3. Restart the terminal if you just changed the setting.

## Usage

### Record music

```sh
map-recorder record
```

Start playback whenever you like, then press **Enter** (or **Ctrl+C**) to stop.
Pressing **Ctrl+C** again while trimming exits immediately and keeps the untrimmed capture.
The result is saved to `~/Music/map-recorder/recording-YYYYMMDD-HHMMSS.wav`.

### Record a meeting

```sh
map-recorder record --mode meeting
```

Produces `~/Music/map-recorder/recording-YYYYMMDD-HHMMSS.m4a` (AAC, mono, 48 kHz, 64 kbps — about 30 MB per hour).

### Choose where to save

```sh
map-recorder record -o ~/Desktop/live-session.wav
map-recorder record --mode meeting -o standup.m4a
```

To change the default folder permanently, set `MAP_RECORDER_DIR`, e.g. in `~/.zshrc`:

```sh
export MAP_RECORDER_DIR="$HOME/Recordings"
```

### Re-trim an existing file

```sh
map-recorder trim recording.raw.wav --threshold-db -50
map-recorder trim interview.wav --mode meeting -o interview.m4a
```

`trim` accepts WAV (16/24/32-bit integer or 32-bit float) and CAF (32-bit float)
files. By default it writes `<name>.trimmed.<wav|m4a>` next to the input.

## Options

| Option | Commands | Default | Description |
|---|---|---|---|
| `--mode <music\|meeting>` | record, trim | `music` | Silence sensitivity and output format |
| `--threshold-db <dB>` | record, trim | `-60` (music), `-40` (meeting) | Loudness below which audio counts as silence |
| `--max-lead <sec>` | record, trim | `2` | Maximum silence kept before the first sound |
| `--tail-pad <sec>` | record, trim | `0.5` | Silence kept after the last sound |
| `-o, --output <path>` | record, trim | see above | Output file |
| `--keep-raw` | record | off | Keep the untrimmed capture next to the result |

Existing files are never overwritten: choose another name or delete the old file.

### Output formats

| Mode | While recording (raw) | Final file |
|---|---|---|
| `music` | `<name>.raw.wav` — 32-bit float, original rate/channels | `<name>.wav` — 24-bit PCM, original rate/channels |
| `meeting` | `<name>.raw.caf` — 32-bit float, original rate/channels | `<name>.m4a` — AAC, mono, 48 kHz, 64 kbps |

The raw file is deleted after a successful trim (unless `--keep-raw`).

## How trimming works

The recording is analysed in **10 ms windows**. Each window's loudness is its
RMS level across all channels, in dBFS. A window is *sound* if it is at least
as loud as the threshold; a **sound event** is at least **50 ms** of consecutive
sound windows, so a lone click or pop doesn't count.

```
 raw:     |--- silence 5 s ---|=========== music 3 s ===========|---- silence 4 s ----|
                              ^ first sound                     ^ last sound
 result:          |-- 2 s --|=========== music 3 s ===========|0.5 s|
```

- If there are less than 2 s of silence at the start, nothing is cut there.
- A very short (5 ms) fade is applied at each cut point to avoid clicks.
- If no sound is found at all, no file is written and the raw capture is kept.

Why two thresholds? In **music**, only near-digital silence (below −60 dBFS)
counts as silence, so soft intros and long fade-outs survive. In **meetings**,
background hum and room tone (typically −50…−40 dBFS) should count as silence,
otherwise the recording would never "end".

Both trimming passes stream the file from disk, so memory use stays flat no
matter how long the recording is.

## Troubleshooting

**"No sound detected" although audio was playing / the file is silent.**
Your terminal probably lacks the System Audio Recording permission. See
[First run](#first-run-grant-the-permission). Toggle the setting off and on and
restart the terminal if it already looks enabled. You can test the capture
alone with `cargo run --example capture_probe` (from a source checkout) while
music plays: it prints the peak level of a 5-second capture.

**"Recorded 0.0 s of audio."**
Core Audio never delivered audio to the process. This happens when the program
is launched from a context where macOS cannot ask for permission (e.g. some
IDE-embedded or sandboxed shells). Run it from a regular terminal window.

**"audio buffers were dropped".**
The disk could not keep up with the incoming audio (very rare). Save to a
local, fast disk.

**"audio buffers had an unexpected layout".**
Core Audio delivered the audio in a way map-recorder doesn't recognise (this
may happen with some multi-channel audio interfaces). Please open an issue with
your audio device model.

**"Output file … already exists".**
`map-recorder` never overwrites files. Pick another name with `-o`.

## Limitations

- **Music recordings stop at about 3 hours**: the raw WAV file has a 4 GB limit. The tool stops by itself shortly before, warns you, and trims what it has. Use `--mode meeting` (CAF, no limit) for longer sessions.
- **Changing the output device while recording** (plugging in headphones, switching to AirPods) causes a brief interruption in the captured audio; recording then continues.
- System audio only: the microphone is not recorded.
- macOS only.

## Roadmap

- Transcription of `meeting` recordings.
- Linux support (PipeWire / PulseAudio monitor sources).

## Development

```sh
cargo test                                   # unit + integration tests
cargo clippy --all-targets -- -D warnings    # lints
cargo fmt                                    # formatting
cargo run --example capture_probe            # manual capture check (play some audio first)
```

Code layout:

| Path | Responsibility |
|---|---|
| `src/capture/` | Core Audio process tap — the only `unsafe` code; see [docs/core-audio-tap.md](docs/core-audio-tap.md) |
| `src/recorder.rs` | Recording session: ring buffer, writer thread, stop handling, live meter |
| `src/trim.rs` | Silence detection (pure logic) |
| `src/process.rs` | Two-pass streaming trim of a file |
| `src/encode.rs` | Final output (24-bit WAV / AAC via `afconvert`) |
| `src/audio/` | WAV and CAF streaming readers/writers |
| `src/mode.rs` | `music` / `meeting` presets |
| `src/cli.rs` | Command-line interface |

The capture layer can't be tested automatically (it needs real audio and a
permission granted by a person), so changes to `src/capture/` or
`src/recorder.rs` should be checked by hand with the probe example and a real
recording.

## License

[MIT](LICENSE)
