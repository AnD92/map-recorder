# Compact Output, `convert`, Completions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `.m4a` (AAC) the default final format, keep WAV behind `--format wav`, add a `convert` command for existing files and a `completions` command for zsh.

**Architecture:** A new `Format` enum (in `src/mode.rs`) travels next to `Mode` from the CLI through `trim_file` into `FinalWriter`. `Mode` still picks silence settings, downmix and AAC quality; `Format` picks the container. All AAC encoding goes through one `convert_to_aac(input, output, AacSettings)` helper around macOS's `afconvert`, reused by the new `src/convert.rs`.

**Tech Stack:** Rust 2024, clap 4 (derive), new dependency `clap_complete`, hound, `afconvert`/`afinfo` (ship with macOS).

**Spec:** `docs/superpowers/specs/2026-11-26-compact-output-design.md`

## Global Constraints

- `source ~/.cargo/env` before cargo commands.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` green after every task.
- No `unsafe` outside `src/capture/` (none needed here).
- User-facing text and docs in English; comments explain purpose, not trivia.
- TDD: failing test first.
- music + m4a: AAC, original channel count, 48 kHz, 256 kbps → `afconvert -f m4af -d aac@48000 -b 256000`.
- meeting + m4a: AAC, mono, 48 kHz, 64 kbps → `afconvert -f m4af -d aac@48000 -b 64000 -c 1` (unchanged).
- music + wav: 24-bit PCM, original rate/channels (unchanged). meeting + wav: mono 24-bit PCM, original rate.
- Default format: `m4a` for both modes.
- Every final file is written as `<name>.part` and renamed on success; temporaries are removed on failure.
- Raw capture during recording unchanged (`.raw.wav` music, `.raw.caf` meeting).

## Review Focus

1. `record -o take.mp3` (or `--format wav -o take.m4a`) must fail **before** recording starts, not after the user has captured a song. → test in Task 3.
2. `convert song.m4a` (input already `.m4a`, default output = same path) must refuse with the "already exists" error and leave the file untouched. → test in Task 4.
3. `convert good.wav missing.wav other.wav`: the missing file is reported, the other two are converted, exit code is non-zero, no `.part` left. → test in Task 4.
4. `-o Song.WAV` (uppercase extension) is accepted as WAV. → test in Task 1.
5. music + m4a from a mono or 44.1 kHz source still encodes (48 kHz output, channels preserved). → test in Task 2.

---

## File Structure

- `src/mode.rs` — add `Format`, `resolve_format`; remove `Mode::output_extension`.
- `src/encode.rs` — `FinalWriter::create(mode, format, path, stream_format)`; `AacSettings`; `pub fn convert_to_aac`; `pub(crate)` `part_path_for`/`publish`.
- `src/process.rs`, `src/recorder.rs` — pass `Format` through.
- `src/convert.rs` (new) — `convert_file`, `default_converted_path`.
- `src/cli.rs` — `--format`, `convert`, `completions`, `EXAMPLES`.
- `tests/trim_cli.rs`, new `tests/convert_cli.rs` — CLI end-to-end.
- `README.md`, `CLAUDE.md` — docs.

---

### Task 1: `Format` enum and format resolution

**Files:**
- Modify: `src/mode.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)] pub enum Format { M4a, Wav }` (clap values `m4a`, `wav`; doc comments become help text).
  - `impl Format { pub fn extension(self) -> &'static str }` → `"m4a"` / `"wav"`.
  - `pub fn resolve_format(flag: Option<Format>, output: Option<&Path>) -> anyhow::Result<Format>`.
  - `Mode::output_extension` is **removed** (callers move to `Format::extension` in Task 3; keep it until then if needed for compilation, delete in Task 3).

- [ ] **Step 1: Write failing tests** in `mode.rs` tests:

```rust
#[test]
fn format_extensions() {
    assert_eq!(Format::M4a.extension(), "m4a");
    assert_eq!(Format::Wav.extension(), "wav");
}

#[test]
fn resolve_format_defaults_to_m4a() {
    assert_eq!(resolve_format(None, None).unwrap(), Format::M4a);
}

#[test]
fn resolve_format_flag_without_output() {
    assert_eq!(resolve_format(Some(Format::Wav), None).unwrap(), Format::Wav);
}

#[test]
fn resolve_format_from_output_extension_any_case() {
    assert_eq!(resolve_format(None, Some(Path::new("a/Song.WAV"))).unwrap(), Format::Wav);
    assert_eq!(resolve_format(None, Some(Path::new("call.m4a"))).unwrap(), Format::M4a);
}

#[test]
fn resolve_format_rejects_unknown_extension() {
    let err = resolve_format(None, Some(Path::new("take.mp3"))).unwrap_err().to_string();
    assert!(err.contains(".wav") && err.contains(".m4a"), "{err}");
    assert!(resolve_format(None, Some(Path::new("take"))).is_err());
}

#[test]
fn resolve_format_rejects_flag_extension_mismatch() {
    let err = resolve_format(Some(Format::Wav), Some(Path::new("take.m4a")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("--format wav"), "{err}");
    assert_eq!(resolve_format(Some(Format::M4a), Some(Path::new("t.M4A"))).unwrap(), Format::M4a);
}
```

- [ ] **Step 2: Run** `cargo test --lib mode::` → FAIL (`Format` not found).
- [ ] **Step 3: Implement** `Format`, `extension`, `resolve_format`. Error copy: unknown extension → `"output file {path} must end in .wav or .m4a"`; mismatch → `"--format {flag} does not match the output file {path}; use a .{flag} name or drop --format"`.
- [ ] **Step 4: Run** `cargo test --lib mode::` → PASS.
- [ ] **Step 5: Commit** `feat: add output Format and resolve it from --format / -o`.

---

### Task 2: `FinalWriter` writes both formats in both modes

**Files:**
- Modify: `src/encode.rs`

**Interfaces:**
- Consumes: `Format` (Task 1).
- Produces:
  - `pub fn FinalWriter::create(mode: Mode, format: Format, final_path: &Path, stream: StreamFormat) -> Result<FinalWriter>` (write/finish unchanged).
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct AacSettings { pub bitrate: u32, pub mono: bool }` and `pub fn aac_settings(mode: Mode) -> AacSettings` (music: 256 000 / false; meeting: 64 000 / true). Sample rate is always 48 000.
  - `pub fn convert_to_aac(input: &Path, output: &Path, settings: AacSettings) -> Result<()>` — args `-f m4af -d aac@48000 -b <bitrate>` plus `-c 1` when `mono`; removes `output` on failure; error text as today.
  - `pub(crate) fn part_path_for(&Path) -> PathBuf`, `pub(crate) fn publish(&Path, &Path) -> Result<()>` (existing, visibility widened).
- Internals (builder's shape, suggested): downmix-to-mono when `mode == Meeting`, applied before either sink; sink `Wav` (hound 24-bit, channels = 1 if downmixed else original) or `Aac` (temp CAF with the output channel count, converted on `finish`). `partial_paths_for` unchanged.

- [ ] **Step 1: Write failing tests** — update every existing `FinalWriter::create(Mode::X, &path, format)` call: music tests pass `Format::Wav`, meeting tests `Format::M4a`; the `final_name_appears_only_when_finished` loop covers all four `(mode, format, name)` combinations. Add (use an `afinfo` helper returning lowercase stdout):

```rust
#[test]
fn music_m4a_is_stereo_aac_48k() {
    // 44.1 kHz stereo source → 2 ch, 48000 Hz AAC.
    // assert info contains "aac", "2 ch", "48000"; only song.m4a left in the dir.
}

#[test]
fn music_m4a_keeps_mono_source_mono() {
    // 44.1 kHz mono source → assert "1 ch", "48000".
}

#[test]
fn meeting_wav_is_mono_24bit() {
    // 48 kHz stereo source, Format::Wav → hound spec: channels 1, 24 bits, 48000,
    // reader.len() == frames written.
}

#[test]
fn aac_settings_per_mode() {
    assert_eq!(aac_settings(Mode::Music), AacSettings { bitrate: 256_000, mono: false });
    assert_eq!(aac_settings(Mode::Meeting), AacSettings { bitrate: 64_000, mono: true });
}
```

- [ ] **Step 2: Run** `cargo test --lib encode::` → FAIL (signature / missing items).
- [ ] **Step 3: Implement** the new `create`, `AacSettings`, `aac_settings`, `convert_to_aac(…, settings)`; update the module doc comment to describe the four combinations. Fix `process.rs` callers minimally (temporarily pass `Format::Wav` for music, `Format::M4a` for meeting) so the crate compiles; Task 3 threads the real value.
- [ ] **Step 4: Run** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: encode m4a or wav in either mode`.

---

### Task 3: `--format` on `record` and `trim`

**Files:**
- Modify: `src/process.rs`, `src/recorder.rs`, `src/cli.rs`, `src/mode.rs` (delete `output_extension` and its test), `tests/trim_cli.rs`

**Interfaces:**
- Consumes: `Format`, `resolve_format` (Task 1); `FinalWriter::create(mode, format, …)` (Task 2).
- Produces:
  - `pub fn trim_file(input: &Path, output: &Path, mode: Mode, format: Format, settings: &Settings) -> Result<TrimOutcome>`.
  - `RecordOptions { mode, format: Format, settings, output, keep_raw }`.
  - `TuningArgs.format: Option<Format>` — `#[arg(long, value_enum)]`, help `"Final file format [default: m4a]"`.
  - `pub fn default_output_path(dir: &Path, format: Format, now) -> PathBuf`; `fn trimmed_path_for(input: &Path, format: Format) -> PathBuf`.
- Flow in `run`: `let format = resolve_format(args.tuning.format, args.output.as_deref())?;` **before** `ensure_absent` and before `record(...)`, so a bad `-o` fails before capturing. Default paths use `format.extension()`.

- [ ] **Step 1: Write failing tests**
  - `cli.rs`: update `default_output_path_format` to `(dir, Format::M4a, now)` → `.m4a`, `(dir, Format::Wav, now)` → `.wav`. Add `record_parses_format_flag` (`record --format wav` → `Some(Format::Wav)`; no flag → `None`).
  - `cli.rs`: add `record_rejects_bad_output_before_recording`: `run(Cli::parse_from(["map-recorder","record","-o",<tmp>/take.mp3]))` returns `Err` whose text contains `".wav or .m4a"`, and nothing was created in `<tmp>` (proves no capture started).
  - `process.rs`: existing tests pass `Format::Wav` for music / `Format::M4a` for meeting.
  - `tests/trim_cli.rs`: `trim_command_end_to_end` now expects `take.trimmed.m4a` (check exists + stdout mentions it); add `trim_format_wav_writes_wav` (`trim take.wav --format wav` → `take.trimmed.wav`, readable by hound, 24-bit); `trim_silent_file_exits_2` checks `quiet.trimmed.m4a` absent.
- [ ] **Step 2: Run** `cargo test` → FAIL.
- [ ] **Step 3: Implement** the threading; `recorder.rs` passes `opts.format` to `trim_file`; remove `Mode::output_extension`; update `RecordArgs.output` doc to `<wav|m4a>` default note (already worded that way — keep).
- [ ] **Step 4: Run** `cargo test` → PASS. Smoke test plumbing: `cargo build && (sleep 1; echo) | ./target/debug/map-recorder record -o <scratch>/x.m4a; echo $?` → "No sound detected", exit 2.
- [ ] **Step 5: Commit** `feat: --format m4a|wav for record and trim, m4a by default`.

---

### Task 4: `convert` command

**Files:**
- Create: `src/convert.rs`, `tests/convert_cli.rs`
- Modify: `src/lib.rs` (`pub mod convert;`), `src/cli.rs`

**Interfaces:**
- Consumes: `aac_settings`, `convert_to_aac`, `part_path_for`, `publish` (Task 2); `ensure_absent` (`cli.rs`, may move to `convert.rs`/a shared spot if it avoids a cycle — builder's call).
- Produces:
  - `pub fn default_converted_path(input: &Path) -> PathBuf` → `input.with_extension("m4a")`.
  - `pub fn convert_file(input: &Path, output: &Path, mode: Mode) -> Result<()>` — errors if `input` is not a file (`"cannot read {input}: no such file"`), if `output` exists (same message as `ensure_absent`), or if `output` lacks a `.m4a` extension (case-insensitive); encodes to `part_path_for(output)` then `publish`.
  - CLI: `Command::Convert(ConvertArgs)`, doc `"Convert existing audio files to compact .m4a (no trimming)."`; `ConvertArgs { #[arg(required = true, value_hint = ValueHint::FilePath)] files: Vec<PathBuf>, #[arg(short, long, value_hint = ValueHint::FilePath)] output: Option<PathBuf>, #[arg(long, value_enum, default_value = "music")] mode: Mode }`.
  - `run`: `-o` with more than one file → `Err("-o can only be used with a single input file")` before converting anything. For each file: success prints `Saved {output}`; failure prints `error: {input}: {err:#}` to stderr and continues. Exit `SUCCESS` if all succeeded, else `FAILURE`.

- [ ] **Step 1: Write failing tests**
  - `convert.rs` unit: `default_converted_path("a/song.wav") == "a/song.m4a"`; `convert_file_music_stereo` (WAV 44.1 kHz stereo via hound → afinfo `aac`, `2 ch`, `48000`; input still exists, unchanged length); `convert_file_meeting_is_mono`; `convert_file_refuses_existing_output` (create output first; error contains "already exists"; output bytes unchanged); `convert_file_rejects_non_m4a_output`; `convert_file_missing_input` (error, no `.part` left).
  - `tests/convert_cli.rs` (binary via `env!("CARGO_BIN_EXE_map-recorder")`, same style as `trim_cli.rs`):
    - `convert_multiple_files_continues_after_failure`: `convert a.wav missing.wav b.wav` → exit code non-zero, `a.m4a` and `b.m4a` exist, stderr mentions `missing.wav`, no `*.part` in dir.
    - `convert_m4a_input_refuses_to_overwrite_itself`: create `song.m4a` (convert a WAV with `-o song.m4a` first), then `convert song.m4a` → non-zero, file bytes unchanged.
    - `convert_output_flag_with_many_files_is_an_error`: `convert a.wav b.wav -o x.m4a` → non-zero, no `.m4a` created.
- [ ] **Step 2: Run** `cargo test` → FAIL.
- [ ] **Step 3: Implement** `src/convert.rs` and the CLI wiring.
- [ ] **Step 4: Run** `cargo test` → PASS.
- [ ] **Step 5: Commit** `feat: convert command for existing recordings`.

---

### Task 5: `completions` command

**Files:**
- Modify: `Cargo.toml` (`cargo add clap_complete`), `src/cli.rs`

**Interfaces:**
- Produces: `Command::Completions { #[arg(value_enum)] shell: clap_complete::Shell }`, doc `"Print a shell completion script (e.g. add `source <(map-recorder completions zsh)` to ~/.zshrc)."`. `run` writes `clap_complete::generate(shell, &mut Cli::command(), "map-recorder", &mut std::io::stdout())` and returns `SUCCESS`.
- Add `value_hint = ValueHint::FilePath` to `TrimArgs.input` and the `output` args of `record`/`trim` so zsh completes paths there.

- [ ] **Step 1: Write failing test** in `cli.rs`: `zsh_completions_cover_commands_and_formats` — generate zsh script into a `Vec<u8>`; assert it contains `"convert"`, `"completions"`, `"--format"`, `"m4a"`, `"_files"`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test` → PASS; `cargo run -q -- completions zsh | head -5` prints `#compdef map-recorder`.
- [ ] **Step 5: Commit** `feat: shell completions command`.

---

### Task 6: Help examples and documentation

**Files:**
- Modify: `src/cli.rs` (`EXAMPLES`, test `top_level_help_shows_mode_examples`), `README.md`, `CLAUDE.md`

- [ ] **Step 1: Write failing test** — extend the help test: help contains `"map-recorder record --format wav"`, `"map-recorder convert"`, `"map-recorder completions zsh"`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Update `EXAMPLES`**, aligned like today:

```
Examples:
  map-recorder record                  Record music (compact .m4a)
  map-recorder record --format wav     Record music (lossless WAV)
  map-recorder record --mode meeting   Record a meeting (mono .m4a)
  map-recorder trim take.raw.wav       Re-trim an existing recording
  map-recorder convert *.wav           Convert existing files to .m4a
  map-recorder completions zsh         Print the zsh completion script
  map-recorder record --help           All recording options
```

- [ ] **Step 4: Run** `cargo test` → PASS.
- [ ] **Step 5: README** — update: feature list (m4a default, `--format wav`, `convert`, completions); "Record music" section (result `.m4a`, ~2 MB/min i.e. ~9 MB per song, plays on Android/iPhone/Mac; `--format wav` for lossless, ~17 MB/min); `trim` section (default `<name>.trimmed.<m4a|wav>`, `--format`); new "Convert existing files" section (no trimming, original kept, multiple files, `--mode meeting`, refuses to overwrite); new "Tab completion" section (`echo 'source <(map-recorder completions zsh)' >> ~/.zshrc`, must be after `compinit`; open new terminal); options table gains `--format` row (record, trim; default `m4a`); output-files table gains the format column (m4a/wav per mode, AAC settings from Global Constraints); "Why not MP3?" one-paragraph note (no encoder in macOS, AAC same quality and plays everywhere); project-layout table gains `src/convert.rs`. Fix every example path that shows `.wav` as the default result.
- [ ] **Step 6: CLAUDE.md** — module map: `encode.rs` line → "24-bit WAV or AAC m4a (via `afconvert`), any mode"; add `src/convert.rs`; `mode.rs` mentions `Format`; `cli.rs` mentions `convert`/`completions`. Add gotcha: "macOS has no MP3 encoder (`afconvert` decodes only); AAC at 256 kbps fails below ~32 kHz, so m4a output is always 48 kHz."
- [ ] **Step 7: Verify** `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` all clean; `cargo run -q -- --help` shows the new examples.
- [ ] **Step 8: Commit** `docs: m4a default, convert and completions in README, help and CLAUDE.md`.
