# Compact output, `convert` command, shell completions — design

## Intent

Music recordings are saved as 24-bit WAV, about 17 MB per minute (a song is
~80 MB). The user listens to them in a music playlist, on macOS and on an
Android phone, so they need small, widely playable files rather than
studio-grade masters. They also want to shrink recordings they already have,
and to type commands and file names faster.

## Goals

- `music` recordings end up as stereo AAC `.m4a` by default (~9 MB per song).
- Lossless WAV stays available on request.
- A `convert` command turns existing audio files into `.m4a` without trimming.
- Tab completion for commands, options and values in zsh.

## Non-goals

- MP3 output. macOS has no MP3 encoder (`afconvert -f MPG3` fails with
  `'fmt?'`); it would need `lame`/`ffmpeg` or a bundled library, for no gain
  in quality or size over AAC. Android plays AAC `.m4a` natively.
- FLAC/ALAC output (possible later; not needed for playlists).
- Encoding AAC while recording. The raw capture stays a 32-bit float WAV/CAF:
  trimming needs uncompressed samples, cutting AAC would mean re-encoding
  (a second generation of loss), and a half-written `.m4a` is unreadable
  after a crash, whereas the raw file is recoverable.
- Completion that offers only audio files (needs a hand-written zsh script).

## Approaches considered

1. **AAC via `afconvert`, extending the existing meeting path** — chosen.
   No new tools to install, small change, quality equal or better than MP3.
2. AAC plus FLAC — FLAC halves size losslessly but adds nothing for a playlist.
3. Real MP3 — external dependency, more fragile, no quality benefit.

## Behaviour

### `--format <m4a|wav>` (record, trim)

- Default `m4a` for both modes.
- `music` + `m4a`: AAC, stereo (original channel count), original sample rate,
  256 kbps.
- `meeting` + `m4a`: unchanged — AAC, mono, 48 kHz, 64 kbps.
- `music` + `wav`: unchanged — 24-bit PCM WAV, original rate/channels.
- `meeting` + `wav`: mono 24-bit WAV (builder's call: keeps the meeting
  downmix consistent; acceptable alternative is original channels).
- Default output paths and `trim`'s `<stem>.trimmed.<ext>` follow the chosen
  format's extension.
- Raw capture during recording is unchanged (`.raw.wav` for music, `.raw.caf`
  for meeting); conversion happens after trimming, as meeting mode does today.
  The music path writes a temporary file and runs `afconvert`, like meeting.
- An `-o` path whose extension disagrees with `--format` is the builder's
  call; suggested: the extension of `-o` wins when it is `.wav`/`.m4a` and no
  `--format` was given, otherwise error with a clear message.

### `convert <FILES>...`

- Converts each file to `.m4a` next to it (`song.wav` → `song.m4a`); the
  original is never modified. No silence trimming.
- Accepts several files (`map-recorder convert *.wav`); one failure is
  reported and the rest continue; non-zero exit if any failed.
- Input: anything `afconvert` can read (WAV, AIFF, CAF, FLAC, MP3, …) —
  passed straight to `afconvert`, no decoding in Rust.
- `--mode music` (default): stereo AAC 256 kbps; `--mode meeting`: mono
  64 kbps.
- `-o <path>` allowed only with a single input.
- Refuses to overwrite an existing output file (builder may add `--force`).
- Same `.part`-then-rename safety as other outputs.

### `completions <shell>`

- `map-recorder completions zsh` prints a completion script generated with
  `clap_complete` (new dependency). File arguments carry
  `ValueHint::FilePath` so zsh completes paths.
- README documents the one-line `~/.zshrc` setup.

## Docs and tests

- README: new defaults, `--format`, `convert`, completions, updated size
  figures and output table. `CLAUDE.md` module map updated.
- CLI help: update the `EXAMPLES` block in `src/cli.rs` (shown by
  `map-recorder --help`) to cover the m4a default, `--format wav`, `convert`
  and `completions`.
- TDD as usual; `afconvert`-backed tests check results with `afinfo`
  (channels, codec, sample rate), as the existing meeting test does.
