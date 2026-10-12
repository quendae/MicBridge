# MicBridge on a Mac

A Mac runs MicBridge as a **sender**: its microphone goes to a Linux or
Windows machine that runs MicBridge as the receiver. The other direction, the
remote microphone showing up as an input device on the Mac, is not supported.
`micbridge doctor` says so, and the window greys out the Receiving half.

There is no installer. You build it once and run it from Terminal.

## Build once

1. Xcode command line tools (skip if `xcode-select -p` prints a path):

   ```bash
   xcode-select --install
   ```

2. Homebrew, if you do not have it: https://brew.sh

3. cmake (it builds the bundled Opus codec) and the Rust toolchain:

   ```bash
   brew install cmake
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source "$HOME/.cargo/env"
   ```

4. Build and put both programs in `~/.cargo/bin`, which the line above added
   to your `PATH`:

   ```bash
   cd MicBridgeMac
   cargo install --path crates/mb-app
   cargo install --path crates/mb-gui
   ```

   The first build takes a few minutes. Later ones are quick.

## Check

```bash
micbridge doctor
```

Expected shape:

```
✓ microphone        2 devices, default: MacBook Pro Microphone
• virtual input     Receiving as a microphone is not supported on macOS yet.
• pairing           nothing paired yet
Usable, but not every role is available.
```

The first two lines are the point: the microphone is there, receiving is off.
The third goes away after the first connection.

## Microphone permission

The first time MicBridge opens the microphone, macOS asks whether
**Terminal** may use it. Say yes. The permission belongs to whichever app
launched the binary, so if you run it from iTerm or from an IDE's terminal,
that app is the one asking.

If you clicked "Don't Allow" once: System Settings → Privacy & Security →
Microphone, switch Terminal on, and start MicBridge again. A denied
microphone does not produce an error, it produces silence, so this is the
first thing to check when the other side hears nothing.

## Send

On the **other** machine, start the receiver first:

```bash
micbridge recv --sink auto
```

On the Mac:

```bash
micbridge devices            # which microphone is which
micbridge send --device "MacBook"
```

`--device` takes part of a name, or `@N` from the list. The sender finds the
receiver on the network by itself. The first time, the receiver shows a
six-digit code and the Mac asks for it; after that they know each other.

If several receivers are visible, name one: `--to "living room"` or
`--to 192.168.1.40`. If the two cannot find each other at all, the Wi-Fi
router is probably blocking multicast; `--to <address>` skips discovery.

## The window

```bash
micbridge-gui
```

Same thing with a switch and a latency graph. On a Mac the Receiving half is
greyed out, the autostart checkbox is not there, and closing the window quits
the program rather than hiding it in the menu bar. The menu-bar icon still
offers Show and Quit while the window is open.

Launching it from Terminal is fine; the window gets a Dock icon like any app.

## Update

```bash
git pull
cargo install --path crates/mb-app
cargo install --path crates/mb-gui
```

## Where things live

| What | Where |
|---|---|
| Pairing keys and language choice | `~/Library/Application Support/micbridge/` |
| The two programs | `~/.cargo/bin/micbridge`, `~/.cargo/bin/micbridge-gui` |

Forgetting a peer: `micbridge forget <name>`, or **Pair again** in the window.
