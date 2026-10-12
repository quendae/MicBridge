# MicBridge on macOS, send-only — design

Date: 2026-10-12
Status: approved in conversation, awaiting spec review

## Goal

Run MicBridge natively on a Mac as a **sender**: the Mac's microphone goes to a
Linux or Windows machine that already runs MicBridge. Both the command line
(`micbridge`) and the existing window (`micbridge-gui`) build and run on macOS.
Receiving on the Mac is explicitly out of scope and the program says so instead
of failing obscurely.

This is for personal use. There is no app bundle, dmg, notarization or
Homebrew formula. A short guide replaces packaging.

## What the user said / what is assumed

Said: Mac role is send only. Personal use. Easy to run and set up. Minimal GUI
or CLI is fine. A guide with setup commands instead of packaging.

Assumed:
- The existing eframe window is already "minimal" and is the GUI we keep.
  No new GUI is written.
- The fork should stay mergeable with upstream: every macOS change is a `cfg`
  gate or an added arm, never a restructuring. Comments in touched files follow
  the surrounding language (Polish) where they sit inside Polish files.
- Receiving via a third-party virtual device (BlackHole, VB-CABLE for Mac) is
  a possible later step, so the sink module gains a macOS arm that refuses
  today rather than being deleted.

## Current state

Nearly all of the code is portable already:

- `cpal` has a CoreAudio backend; device listing and capture need no change.
- `mdns-sd`, SPAKE2, Noise, Opus and the resampler are platform-independent.
- `eframe` and `tray-icon` both support macOS and the lockfile already pulls
  the AppKit backends.
- `mb-i18n` reads `LANG`-style variables on non-Windows, which Terminal sets.
- `mb-net` stores keys under `dirs::config_dir()`, which exists on macOS.

The code currently gates on `target_os = "linux"` versus everything else, so
macOS silently takes the Windows path and talks about VB-CABLE. That is the
part to fix.

## Design

### 1. Audio sink (`crates/mb-audio/src/sink.rs`)

Add a macOS arm alongside Linux and Windows.

- `validate(selector)`: on macOS, `auto` and `virtual` are refused up front
  with a new message: receiving as a microphone is not supported on macOS yet;
  to hear the remote microphone through the speakers, use `--sink device` or a
  device name. `device` and name fragments keep working because they are plain
  CoreAudio output devices.
- `open_sink`: on macOS, `auto` goes to the same refusal as `virtual` instead
  of `open_cable`. Nothing else changes.
- `open_cable`, `VIRTUAL_SINK_HINTS`, `looks_like_virtual_cable` and
  `latency_hint` stay as they are; they are Windows-only in practice and
  already `#[allow(dead_code)]` where needed.
- The existing test `a_virtual_sink_is_refused_up_front_off_linux` keeps
  passing. A new test asserts that `auto` is refused on macOS and accepted
  elsewhere.

### 2. Doctor (`crates/mb-app/src/doctor.rs`)

Add a `#[cfg(target_os = "macos")] fn sink()` that returns `Grade::Warn`,
detail "receiving is not supported on macOS", advice "this Mac can send its
microphone; to receive, use the Linux or Windows machine as the receiver". The
Windows variant becomes `#[cfg(not(any(target_os = "linux", target_os =
"macos")))]`. The microphone check and the pairing check are unchanged.

### 3. Receiving entry point (`crates/mb-app/src/recv.rs`)

No change. It already calls `mb_audio::sink::validate` at startup, so the
macOS refusal from section 1 surfaces there with the right message.

### 4. Window (`crates/mb-gui`)

- Tray: no change. The non-Linux path builds the icon on the window thread,
  which is what AppKit requires.
- Hiding to the tray: no change. `Waker::can_restore()` is already `false` off
  Windows, so closing the window quits. This is the honest behaviour until a
  macOS restore path exists. The tray icon still offers Show and Quit.
- Receiving half: on macOS the Receiving switch is disabled and the hint under
  it shows the "not supported on macOS" message from section 1. The sink and
  buffer controls under it stay as they are but are inert.
- Autostart: on macOS the checkbox is not drawn. `autostart::imp` gets a macOS
  module whose `enabled()` returns `false` and whose `enable()` bails with a
  new message. The XDG implementation is gated to `not(any(windows, macos))`.

### 5. Strings (`crates/mb-i18n/src/catalog.rs`)

New keys, each in all seven languages, Polish first:

- `MacNoReceive` — "Receiving as a microphone is not supported on macOS yet."
- `MacNoReceiveAdvice` — "This Mac can send its microphone. To hear the
  remote one through the speakers use `--sink device`." (doctor advice and
  CLI refusal share this)
- `MacNoAutostart` — "Starting with the system is not set up on macOS."

The catalog test that counts `{}` placeholders covers them automatically.

### 6. Build, CI and guide

- `docs/MACOS.md` (English): install Xcode command line tools, Homebrew,
  `brew install cmake`, rustup, `cargo build --release`, `cargo install
  --path crates/mb-app` and `--path crates/mb-gui`, then `micbridge doctor`,
  `micbridge send --device "<name>"`, and the window. Explains the microphone
  permission prompt (it is attributed to Terminal when the binary is run from
  Terminal), pairing against the other machine, and the mDNS troubleshooting
  pointer already in the README.
- `.github/workflows/ci.yml`: add `macos-latest` to the test matrix. The
  Linux-only dependency step is already conditioned on `runner.os`.
- `README.md`: one line in the Install table pointing to `docs/MACOS.md`,
  and a note in "Where the audio lands" that macOS is send-only.

### 7. Error handling

All macOS refusals happen at startup or in `doctor`, never mid-session, and
each carries the advice string. The sending path's errors are unchanged.

### 8. Testing

- Unit: the sink validation tests and the catalog completeness test, run on
  macOS.
- Workspace: `cargo fmt --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace`, `cargo build --release` on this Mac.
- Manual: `micbridge doctor` shows mic OK and receive Warn; `micbridge send`
  pairs with and streams to the other machine; the window opens, lists the
  Mac's microphones, and the Sending switch streams.

## Out of scope

Receiving on macOS, hiding the window to the tray on macOS, a LaunchAgent for
autostart, an app bundle, signing, notarization, Homebrew.
