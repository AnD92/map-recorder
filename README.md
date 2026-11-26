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
Recording system audio (48000 Hz, 2 ch, music mode) → ~/Music/map-recorder/recording-20261126-160905.m4a
● REC 00:03:27  [##############------]  -18.4 dBFS   (press Enter to stop)
Trimming silence…
Recorded 237.6 s of audio.
Saved /Users/you/Music/map-recorder/recording-20261126-160905.m4a
  kept 205.1 s, removed 2.0 s at the start and 30.5 s at the end
```

## Features

- 🎧 **System audio capture** without drivers, via Core Audio process taps.
- ✂️ **Automatic trimming** of leading and trailing silence.
- 🎼 **Two modes**:
  - `music` — strict silence detection (quiet passages and fade-outs are preserved), stereo **AAC `.m4a` at 256 kbps** (about 2 MB per minute), ready for your playlists;
  - `meeting` — lenient silence detection (room noise counts as silence), compact mono **AAC `.m4a`**, ready for transcription tools.
- 💾 **Lossless when you want it**: `--format wav` saves a 24-bit WAV instead.
- 📈 Live elapsed time and level meter while recording.
- 🛟 **Safe by design**: audio is streamed to disk and the file is kept valid every second, so even a crash or a closed terminal leaves a usable file; the untrimmed capture is kept whenever something goes wrong, and a file with the final name is always complete.
- 🔁 `trim` command to re-process any `.wav` / `.caf` file with different settings.
- 🔄 `convert` command to shrink recordings you already have into `.m4a`.
- ⌨️ Tab completion for zsh (and other shells).

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

### Run `map-recorder` from anywhere

To type just `map-recorder <command>` in any terminal, without pointing at the
project folder, install the binary into Cargo's `bin` directory:

1. Make sure `~/.cargo/bin` is on your `PATH`. The Rust installer usually sets
   this up; if `cargo --version` fails in a new terminal, add this line to
   `~/.zshrc` and open a new terminal:

   ```sh
   . "$HOME/.cargo/env"
   ```

2. From the project folder, build and install an optimised binary:

   ```sh
   cargo install --path .
   ```

3. Open a new terminal and use it from any folder:

   ```sh
   map-recorder --help
   map-recorder record
   map-recorder record --mode meeting
   ```

> **Rebuild after every change.** The installed binary is a copy and does not
> follow the source code. After pulling updates or editing the project, run
> `cargo install --path .` again from the project folder (if you installed with
> `cargo install --git …`, re-run that command with `--force`).
>
> To remove it: `cargo uninstall map-recorder`.

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
The result is saved to `~/Music/map-recorder/recording-YYYYMMDD-HHMMSS.m4a`:
stereo AAC, 48 kHz, 256 kbps — about 2 MB per minute (a 4-minute song is about
8 MB). It plays in Apple Music, on iPhone and Android phones, in VLC and in
practically every music player.

For a lossless master (about 17 MB per minute), ask for WAV:

```sh
map-recorder record --format wav
```

### Record a meeting

```sh
map-recorder record --mode meeting
```

Produces `~/Music/map-recorder/recording-YYYYMMDD-HHMMSS.m4a` (AAC, mono, 48 kHz, 64 kbps — about 30 MB per hour).

### Choose where to save

```sh
map-recorder record -o ~/Desktop/live-session.m4a
map-recorder record -o ~/Desktop/live-session.wav     # lossless
map-recorder record --mode meeting -o standup.m4a
```

The name given with `-o` must end in `.m4a` or `.wav`, and its extension picks
the format. If you also pass `--format`, the two must agree. This is checked
before recording starts.

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
files. By default it writes `<name>.trimmed.m4a` next to the input
(`<name>.trimmed.wav` with `--format wav`).

### Convert existing files

```sh
map-recorder convert song.wav                  # → song.m4a
map-recorder convert *.wav                     # every WAV in the folder
map-recorder convert interview.wav --mode meeting
map-recorder convert song.wav -o ~/Desktop/song.m4a
```

`convert` only changes the format — it does **not** trim silence (use `trim`
for that). It accepts anything macOS can read (WAV, AIFF, CAF, FLAC, MP3, …),
writes `<name>.m4a` next to each file, and never touches the original. With
`--mode music` (default) the result is stereo AAC at 256 kbps; with
`--mode meeting` it is mono AAC at 64 kbps. If one file fails, the others are
still converted and the command exits with an error at the end. Existing
`.m4a` files are never overwritten.

### Tab completion

To complete commands, options and file names with **Tab**, add this line to
`~/.zshrc` (after `compinit`, if your config calls it) and open a new terminal:

```sh
source <(map-recorder completions zsh)
```

Then `map-recorder con<Tab>` becomes `map-recorder convert`, and
`--format <Tab>` offers `m4a` and `wav`. Scripts for other shells are available
too: `map-recorder completions bash|fish|elvish|powershell`.

## Options

| Option | Commands | Default | Description |
|---|---|---|---|
| `--mode <music\|meeting>` | record, trim, convert | `music` | Silence sensitivity (record, trim), channels and quality |
| `--format <m4a\|wav>` | record, trim | `m4a` (or the extension of `-o`) | Final file format |
| `--threshold-db <dB>` | record, trim | `-60` (music), `-40` (meeting) | Loudness below which audio counts as silence |
| `--max-lead <sec>` | record, trim | `2` | Maximum silence kept before the first sound |
| `--tail-pad <sec>` | record, trim | `0.5` | Silence kept after the last sound |
| `-o, --output <path>` | record, trim, convert | see above | Output file (`convert`: single input only) |
| `--keep-raw` | record | off | Keep the untrimmed capture next to the result |

Existing files are never overwritten: choose another name or delete the old file.

### Output formats

| Mode | While recording (raw) | Final file, `--format m4a` (default) | Final file, `--format wav` |
|---|---|---|---|
| `music` | `<name>.raw.wav` — 32-bit float, original rate/channels | `<name>.m4a` — AAC, original channels, 48 kHz, 256 kbps | `<name>.wav` — 24-bit PCM, original rate/channels |
| `meeting` | `<name>.raw.caf` — 32-bit float, original rate/channels | `<name>.m4a` — AAC, mono, 48 kHz, 64 kbps | `<name>.wav` — 24-bit PCM, mono, original rate |

The raw file is deleted after a successful trim (unless `--keep-raw`).

The recording always goes through an uncompressed raw file: trimming needs the
exact samples, and an uncompressed file is still usable if the program is
interrupted. AAC encoding happens once, at the end, with macOS's built-in
`afconvert`.

**Why AAC and not MP3?** macOS ships an AAC encoder but no MP3 encoder, so MP3
would need extra software (`lame` or `ffmpeg`). At the same size AAC sounds as
good as MP3 or better, and every modern phone, computer and music app plays
`.m4a` files.

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
| `src/convert.rs` | `convert` command: existing files → `.m4a` |
| `src/audio/` | WAV and CAF streaming readers/writers |
| `src/mode.rs` | `music` / `meeting` presets, `m4a` / `wav` formats |
| `src/cli.rs` | Command-line interface |

The capture layer can't be tested automatically (it needs real audio and a
permission granted by a person), so changes to `src/capture/` or
`src/recorder.rs` should be checked by hand with the probe example and a real
recording.

## License

[MIT](LICENSE)
