# How system audio capture works

This document explains the low-level module `src/capture/` — the only part
of map-recorder that talks directly to macOS and the only place that uses
`unsafe` Rust. It is written for readers who have never used Core Audio.

## The problem

macOS has no "record what the speakers play" device. Microphones are input
devices; speakers are output devices, and their audio is not readable by
other apps. Historically you needed a virtual driver (BlackHole, Soundflower)
and a manual Multi-Output setup.

Since **macOS 14.2** Core Audio offers **process taps**: an official way to
receive a copy of the audio that one, several or all processes are playing.
map-recorder uses a *global* tap, i.e. all processes.

## The pieces

| Piece | What it is | Why we need it |
|---|---|---|
| `CATapDescription` | An Objective-C object describing *what* to tap | Says "all processes, mixed to stereo, don't mute them" |
| Process tap | A Core Audio object (`AudioObjectID`) created from the description | The actual source of the copied audio |
| Aggregate device | A virtual device combining real devices and taps | A tap cannot be read directly: audio is only delivered through a device's IO cycle |
| IO block | A closure Core Audio calls on every IO cycle | Where we receive the samples |

## Step by step (`TapSession::start`)

1. **Check the OS.** If the `CATapDescription` class doesn't exist, the OS is
   older than 14.2 → `CaptureError::UnsupportedOs`.

2. **Describe the tap.**
   `initStereoGlobalTapButExcludeProcesses([])` builds a global stereo tap
   that excludes nobody. We also set:
   - a fresh `UUID` — used later to reference the tap from the aggregate device;
   - `private = true` — the tap is visible only to our process;
   - `muteBehavior = Unmuted` — you keep hearing the audio while recording.

3. **Create the tap.** `AudioHardwareCreateProcessTap(description, &tap_id)`.
   The first time a program does this, macOS asks for the
   **System Audio Recording** permission (see *Permissions* below).

4. **Read the stream format.** The `kAudioTapPropertyFormat` property returns
   an `AudioStreamBasicDescription` (sample rate, channels, sample type).
   We require 32-bit float linear PCM — that's what taps produce — and
   expose rate and channel count as `StreamFormat`.

5. **Create a private aggregate device.**
   `AudioHardwareCreateAggregateDevice` takes a dictionary:

   | Key | Value | Purpose |
   |---|---|---|
   | `name`, `uid` | `"map-recorder system tap"`, random UUID | Identity of the device |
   | `master` (main sub-device) | UID of the current default output device | Clock source for the IO cycle |
   | `private` | `true` | Hidden from System Settings and other apps |
   | `stacked` | `false` | Normal (non-stacked) aggregate |
   | `tapautostart` | `true` | The tap starts delivering audio when the device starts |
   | `subdevices` | `[{uid: output UID}]` | The real device that drives the clock |
   | `taps` | `[{uid: tap UUID, drift: true}]` | The tap as an input; drift compensation keeps it in sync |

   The dictionary is built as an `NSDictionary` and passed as a
   `CFDictionary` (the two types are "toll-free bridged": same memory layout).

6. **Install the IO block.** `AudioDeviceCreateIOProcIDWithBlock` registers a
   closure that Core Audio calls on its **real-time thread** (we pass no
   dispatch queue). Each call receives an `AudioBufferList` with the tapped
   audio. If the output device also has inputs (e.g. an audio interface),
   their buffers come first and the tap's buffers are **at the end** of the
   list, so the block always reads the trailing buffers. Two layouts are
   handled:
   - **interleaved** — one buffer holding all channels (`L R L R …`): passed
     through as-is, no copy. This is what we observe on macOS 26;
   - **non-interleaved** — one buffer per channel: interleaved into a scratch
     buffer that was allocated *before* starting, in slices if needed.

   Any other layout is skipped and counted (`SystemAudioTap::unreadable_cycles`);
   the CLI warns at the end of the recording if the count is non-zero.

7. **Start.** `AudioDeviceStart(aggregate, io_proc)`.

## Teardown (`Drop for TapSession`)

Always in reverse order, and also when `start` fails half-way (the partially
built session is dropped, releasing only what was created):

1. `AudioDeviceStop`
2. `AudioDeviceDestroyIOProcID`
3. `AudioHardwareDestroyAggregateDevice`
4. `AudioHardwareDestroyProcessTap`

The IO block is kept alive inside the session until after step 2, so Core
Audio never calls freed memory.

## Real-time rules

The IO block runs on a high-priority thread with a deadline of a few
milliseconds. Missing it causes audible glitches. Inside the block, and inside
the callback the rest of the program passes to `SystemAudioTap::start`:

- no memory allocation;
- no locks (`Mutex`), no blocking;
- no file or terminal I/O, no logging.

map-recorder's callback only pushes samples into a lock-free ring buffer
(`rtrb`); a normal thread drains it and writes to disk.

## Permissions

macOS protects system audio with the **System Audio Recording** privacy
permission (System Settings → Privacy & Security → Screen & System Audio
Recording → *System Audio Recording Only*).

- The permission is granted to the **responsible app** — for a command-line
  tool, that's the terminal you launch it from (Terminal, iTerm2, VS Code…).
- macOS only prompts executables that declare `NSAudioCaptureUsageDescription`.
  A CLI has no app bundle, so `build.rs` links `Info.plist` into the
  executable's `__TEXT,__info_plist` section. Check with:
  `otool -s __TEXT __info_plist target/debug/map-recorder`.

## Debugging "my recording is silent / empty"

Symptoms observed during development:

- **Zero frames captured** (the IO block never runs) while every Core Audio
  call returns success: the host process has no permission and macOS could
  not show a prompt (e.g. launched from a non-interactive/sandboxed shell).
  Run from your own terminal and grant the permission.
- **Frames captured but all silent**: permission denied for the terminal,
  or nothing was playing. Check the setting above, toggle it off/on, and
  restart the terminal.
- To test the capture layer alone: `cargo run --example capture_probe`
  while music plays. It records 5 s into `probe.wav` and prints the peak
  level; anything above about −40 dBFS means real audio is coming through.
