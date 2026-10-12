# macOS Send-Only Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `micbridge` and `micbridge-gui` build and run natively on macOS as a sender, with receiving refused honestly, plus a setup guide and a macOS CI job.

**Architecture:** The engine is already cross-platform; the work is adding a `target_os = "macos"` arm wherever the code currently assumes "not Linux means Windows". Every change is a `cfg` gate or an added branch so the fork stays mergeable with upstream. New user-facing text goes through the i18n catalog like everything else.

**Tech Stack:** Rust 1.85+, cargo, cpal (CoreAudio backend), eframe 0.32, tray-icon 0.24, mb-i18n catalog macro. Build deps on macOS: Xcode Command Line Tools (present), Homebrew (present), cmake (for vendored libopus), rustup.

**Spec:** `docs/superpowers/specs/2026-10-12-macos-send-only-design.md`

## Global Constraints

- Every macOS change is a `cfg` gate or an added arm; no restructuring of existing modules.
- Comments inside Polish-commented files are written in Polish, matching their surroundings. New English-only files (`docs/MACOS.md`) are English.
- Every new user-visible string is a row in `crates/mb-i18n/src/catalog.rs` with all seven columns in this order: Polish, English, German, Spanish, French, Italian, Ukrainian. Polish is the source.
- `cargo clippy --workspace --all-targets -- -D warnings` must stay clean on macOS; unused items behind a cfg are a build failure, not a warning.
- On macOS `--sink auto` and `--sink virtual` are refused at startup. `--sink device` and device-name fragments keep working.
- Receiving on macOS, hiding to the tray on macOS, LaunchAgent autostart, app bundle, signing, Homebrew: all out of scope.
- Commit messages: English, imperative, with the trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## Review Focus

1. `--sink "  AUTO  "` on macOS (whitespace, capitals): must be refused like `auto`. Test pinned in Task 3.
2. `--sink device` on macOS: `micbridge recv` must proceed and play through the speakers, not be refused. Test pinned in Task 3.
3. `micbridge doctor` on a Mac with no paired peers: must print two warnings (receive, pairing) and the "usable but not every role" footer, never a failure. Test pinned in Task 4 (worst-grade logic already tested; the macOS sink check is asserted to be Warn).
4. The Receiving toggle in the window on macOS: clicking it must do nothing and must not start a session that then fails. Pinned by the `add_enabled_ui(false)` wrapper in Task 6 and the manual check in Task 8.
5. Microphone permission denied in System Settings: cpal delivers silence rather than an error, so the sender looks alive and the receiver hears nothing. The guide in Task 7 tells the reader where to look. No code change.

---

### Task 1: Toolchain bootstrap and baseline build on macOS

**Files:**
- None in the repo. This task installs tools and records what the untouched tree does on macOS.

**Interfaces:**
- Consumes: nothing.
- Produces: a working `cargo` in `PATH`, `cmake` from Homebrew, and a known baseline (build passes or a recorded list of failures) that later tasks build on.

- [ ] **Step 1: Install cmake**

Run:
```bash
brew install cmake
cmake --version
```
Expected: a version line, 3.x or newer.

- [ ] **Step 2: Install the Rust toolchain**

Run:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default
source "$HOME/.cargo/env"
rustc --version
cargo --version
rustup component list --installed | grep -E 'clippy|rustfmt'
```
Expected: rustc 1.85 or newer; clippy and rustfmt listed.

- [ ] **Step 3: Baseline build of the whole workspace**

Run:
```bash
cd /Users/elijah/Documents/git-repositories/MicBridgeMac
source "$HOME/.cargo/env"
cargo build --workspace 2>&1 | tail -30
```
Expected: `Finished` for both binaries. If any crate fails to compile on macOS, copy the error verbatim into a note at the end of this task and stop: that is new information the plan has to absorb before Task 2. The lockfile already resolves the macOS backends for cpal (coreaudio-rs) and tray-icon (objc2-app-kit), so a failure here would be unexpected.

- [ ] **Step 4: Baseline tests, fmt and clippy**

Run:
```bash
cargo test --workspace 2>&1 | grep -E 'test result|FAILED|panicked' 
cargo fmt --all --check && echo FMT-OK
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: every `test result:` line says `ok`; `FMT-OK`; clippy ends with `Finished`. Record any deviation.

- [ ] **Step 5: Confirm what the untouched tree says on macOS**

Run:
```bash
./target/debug/micbridge doctor
./target/debug/micbridge devices | tail -6
```
Expected: doctor's second line talks about a missing virtual cable and VB-CABLE (the Windows text), and `devices` ends with the Windows sink note. This is the wrong behaviour the next tasks fix. The first line should list the Mac's microphones; if it says "cannot query the system", note it.

No commit for this task; nothing in the tree changed.

---

### Task 2: macOS strings in the i18n catalog

**Files:**
- Modify: `crates/mb-i18n/src/catalog.rs` (append rows after `DocCheckConfigDir` at line 209, and after `CliSinkWindowsNote` at line 290)

**Interfaces:**
- Produces: `mb_i18n::Key::{MacNoReceive, MacNoReceiveAdvice, MacNoAutostart, CliSinkMac, CliSinkMacNote}`, each readable with `mb_i18n::t(key) -> &'static str`. None of them has a `{}` placeholder.

- [ ] **Step 1: Write the failing test**

Append inside the `mod tests` block at the bottom of `crates/mb-i18n/src/lib.rs` (after the existing tests, before the closing `}`):

```rust
    #[test]
    fn the_macos_strings_exist_in_every_language_without_placeholders() {
        use crate::catalog::text;
        for key in [
            Key::MacNoReceive,
            Key::MacNoReceiveAdvice,
            Key::MacNoAutostart,
            Key::CliSinkMac,
            Key::CliSinkMacNote,
        ] {
            for lang in [
                Lang::Pl,
                Lang::En,
                Lang::De,
                Lang::Es,
                Lang::Fr,
                Lang::It,
                Lang::Uk,
            ] {
                let s = text(key, lang);
                assert!(!s.is_empty(), "{key:?} pusty w {lang:?}");
                assert!(!s.contains("{}"), "{key:?} ma miejsce do wypełnienia w {lang:?}");
            }
        }
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p mb-i18n the_macos_strings 2>&1 | grep -E 'error\[|no variant|test result'`
Expected: compile error `no variant or associated item named `MacNoReceive``.

- [ ] **Step 3: Add the catalog rows**

In `crates/mb-i18n/src/catalog.rs`, directly after the `DocCheckConfigDir:` row (line 209), insert:

```rust
// --- macOS: tylko wysyłanie --------------------------------------------------
MacNoReceive: "Odbieranie jako mikrofon nie działa jeszcze w macOS.", "Receiving as a microphone is not supported on macOS yet.", "Empfangen als Mikrofon wird unter macOS noch nicht unterstützt.", "Recibir como micrófono aún no funciona en macOS.", "La réception comme microphone n’est pas encore prise en charge sur macOS.", "La ricezione come microfono non è ancora supportata su macOS.", "Приймання як мікрофон у macOS ще не підтримується.";
MacNoReceiveAdvice: "Ten Mac może wysyłać swój mikrofon. Żeby usłyszeć cudzy przez głośniki, użyj --sink device.", "This Mac can send its microphone. To hear the remote one through the speakers use --sink device.", "Dieser Mac kann sein Mikrofon senden. Um das entfernte über die Lautsprecher zu hören, nutze --sink device.", "Este Mac puede enviar su micrófono. Para oír el remoto por los altavoces usa --sink device.", "Ce Mac peut envoyer son microphone. Pour entendre celui d’en face par les haut-parleurs, utilisez --sink device.", "Questo Mac può inviare il suo microfono. Per sentire quello remoto dagli altoparlanti usa --sink device.", "Цей Mac може надсилати свій мікрофон. Щоб почути віддалений через динаміки, використайте --sink device.";
MacNoAutostart: "Uruchamianie przy starcie systemu nie jest ustawione w macOS.", "Starting with the system is not set up on macOS.", "Der Start mit dem System ist unter macOS nicht eingerichtet.", "El inicio con el sistema no está configurado en macOS.", "Le démarrage avec le système n’est pas configuré sur macOS.", "L’avvio con il sistema non è configurato su macOS.", "Запуск разом із системою в macOS не налаштовано.";
```

Directly after the `CliSinkWindowsNote:` row (line 290 before the insert above, now three lines lower), insert:

```rust
CliSinkMac: "Ujście: w macOS --sink auto nie działa — ten Mac tylko wysyła;", "Sink: on macOS --sink auto does not work — this Mac only sends;", "Senke: unter macOS funktioniert --sink auto nicht — dieser Mac sendet nur;", "Salida: en macOS --sink auto no funciona; este Mac solo envía;", "Sortie : sur macOS --sink auto ne fonctionne pas — ce Mac ne fait qu’envoyer ;", "Uscita: su macOS --sink auto non funziona — questo Mac invia soltanto;", "Вихід: у macOS --sink auto не працює — цей Mac лише надсилає;";
CliSinkMacNote: "        nazwa urządzenia wyjściowego odtworzy cudzy mikrofon przez głośniki.", "        an output device name plays the remote microphone through the speakers.", "        ein Ausgabegerätename spielt das entfernte Mikrofon über die Lautsprecher ab.", "        el nombre de un dispositivo de salida reproduce el micrófono remoto por los altavoces.", "        un nom de périphérique de sortie joue le microphone distant par les haut-parleurs.", "        il nome di un dispositivo di uscita riproduce il microfono remoto dagli altoparlanti.", "        назва пристрою виведення відтворить віддалений мікрофон через динаміки.";
```

- [ ] **Step 4: Run the crate's tests**

Run: `cargo test -p mb-i18n 2>&1 | grep -E 'test result|FAILED'`
Expected: `test result: ok` with the new test counted, and the existing placeholder-count test still passing.

- [ ] **Step 5: Commit**

```bash
git add crates/mb-i18n/src/catalog.rs crates/mb-i18n/src/lib.rs
git commit -m "$(cat <<'EOF'
i18n: strings for the macOS send-only mode

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Sink refuses `auto` and `virtual` on macOS

**Files:**
- Modify: `crates/mb-audio/src/sink.rs` (header comment lines 1-12, `open_sink` lines 76-99, `open_virtual` lines 101-125, `validate` lines 177-193, tests lines 211-242)

**Interfaces:**
- Consumes: `mb_i18n::Key::{MacNoReceive, MacNoReceiveAdvice}` from Task 2.
- Produces: `mb_audio::sink::validate(&str) -> Result<()>` now errs on macOS for `auto`/`virtual`; `open_sink` on macOS routes `auto` to the same refusal. Signatures unchanged.

- [ ] **Step 1: Write the failing tests**

In the `mod tests` block of `crates/mb-audio/src/sink.rs`, replace the test `every_other_selector_passes_validation` with these two tests:

```rust
    #[test]
    fn auto_is_refused_up_front_on_macos_only() {
        for s in ["auto", "  AUTO  "] {
            assert_eq!(
                validate(s).is_err(),
                cfg!(target_os = "macos"),
                "`{s}` w macOS ma odpaść od razu, gdzie indziej przejść"
            );
        }
    }

    #[test]
    fn plain_devices_pass_validation_everywhere() {
        for s in ["device", "@0", "cable input", "MacBook Pro Speakers"] {
            assert!(validate(s).is_ok(), "odrzucono `{s}`");
        }
    }
```

- [ ] **Step 2: Run them to confirm the macOS one fails**

Run: `cargo test -p mb-audio sink::tests 2>&1 | grep -E 'test |test result'`
Expected: `auto_is_refused_up_front_on_macos_only ... FAILED` (because `validate("auto")` currently passes), the other three tests ok.

- [ ] **Step 3: Implement the macOS arm**

Replace the module header comment (lines 1-12) with:

```rust
//! Gdzie ląduje odebrany dźwięk.
//!
//! Trzy systemy, trzy drogi (§4 architektury):
//!
//! * **Linux** — tworzymy własny węzeł PipeWire i sami jesteśmy mikrofonem.
//!   Nic nie trzeba instalować.
//! * **Windows** — nie da się utworzyć urządzenia wejściowego bez sterownika
//!   trybu jądra, więc piszemy do cudzego wirtualnego kabla, który znajdujemy
//!   po nazwie.
//! * **macOS** — też wymagałby cudzego sterownika, a tego jeszcze nie
//!   obsługujemy. Ten system tylko wysyła; odbieranie jako mikrofon odpada
//!   od razu z jasnym komunikatem. Zwykłe urządzenie wyjściowe nadal działa.
//!
//! Poza tym jednym kafelkiem cała reszta silnika jest symetryczna, dlatego
//! różnica siedzi tutaj i nigdzie indziej.
```

Replace the `use` lines (13-15) with:

```rust
use anyhow::{bail, Result};
use mb_i18n::{t, Key as K};

use crate::{start_playback, Direction, PlaybackHandle};
```

In `open_sink`, replace the `auto` block:

```rust
    if selector.eq_ignore_ascii_case("auto") {
        #[cfg(target_os = "linux")]
        {
            return open_virtual(rate, fill);
        }
        // W macOS `auto` znaczy to samo, co `virtual` — i tak samo odpada.
        #[cfg(target_os = "macos")]
        {
            return open_virtual(rate, fill);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            return open_cable(rate, fill);
        }
    }
```

Replace the non-Linux `open_virtual` (the `#[cfg(not(target_os = "linux"))]` one) with two functions:

```rust
#[cfg(target_os = "macos")]
fn open_virtual<F>(_rate: u32, _fill: F) -> Result<Sink>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    bail!("{}\n{}", t(K::MacNoReceive), t(K::MacNoReceiveAdvice))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_virtual<F>(_rate: u32, _fill: F) -> Result<Sink>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    bail!(
        "własne wejście wirtualne umiemy zrobić tylko w Linuksie (PipeWire).\n\
         W Windows potrzebny jest sterownik — zainstaluj VB-CABLE i użyj \
         `--sink auto`."
    )
}
```

Replace `validate` with:

```rust
pub fn validate(selector: &str) -> Result<()> {
    let selector = selector.trim();
    let wants_virtual = selector.eq_ignore_ascii_case("virtual");
    let wants_auto = selector.eq_ignore_ascii_case("auto");

    // macOS: ani własnego węzła, ani cudzego kabla — obie drogi odpadają.
    if cfg!(target_os = "macos") && (wants_virtual || wants_auto) {
        bail!("{}\n{}", t(K::MacNoReceive), t(K::MacNoReceiveAdvice));
    }

    if wants_virtual && !cfg!(target_os = "linux") {
        bail!(
            "`--sink virtual` działa tylko w Linuksie, gdzie PipeWire pozwala \
             utworzyć własny mikrofon.\n\
             W Windows użyj `--sink auto` (znajdzie wirtualny kabel) albo podaj \
             fragment nazwy urządzenia."
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Run the crate's tests and clippy**

Run:
```bash
cargo test -p mb-audio 2>&1 | grep -E 'test result|FAILED'
cargo clippy -p mb-audio --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: all ok; clippy `Finished`. `open_cable` already carries `#[allow(dead_code)]`, so being unused on macOS is fine.

- [ ] **Step 5: Confirm the CLI refusal end to end**

Run:
```bash
cargo build -p mb-app
./target/debug/micbridge recv 2>&1 | head -3
./target/debug/micbridge recv --sink virtual 2>&1 | head -3
```
Expected: both print the "Receiving as a microphone is not supported on macOS yet." line and the advice, and exit non-zero. (The system language decides the language; `MICBRIDGE_LANG=en` forces English.)

- [ ] **Step 6: Commit**

```bash
git add crates/mb-audio/src/sink.rs
git commit -m "$(cat <<'EOF'
sink: refuse auto and virtual on macOS with a clear message

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Doctor reports send-only on macOS

**Files:**
- Modify: `crates/mb-app/src/doctor.rs` (header lines 1-11, `sink()` variants lines 133-178, tests lines 208-239)

**Interfaces:**
- Consumes: `mb_i18n::Key::{MacNoReceive, MacNoReceiveAdvice}`.
- Produces: `doctor::check()` on macOS returns a `Report` whose virtual-input check is `Grade::Warn`. Public API unchanged.

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `crates/mb-app/src/doctor.rs`:

```rust
    #[test]
    fn on_macos_the_sink_check_is_a_warning_about_sending_only() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let check = sink();
        assert_eq!(check.grade, Grade::Warn, "odbieranie w macOS to ostrzeżenie, nie błąd");
        assert_eq!(check.detail, t(K::MacNoReceive));
        assert_eq!(check.advice, t(K::MacNoReceiveAdvice));
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p mb-app doctor::tests 2>&1 | grep -E 'test |test result|panicked'`
Expected: `on_macos_the_sink_check_is_a_warning_about_sending_only ... FAILED` with the detail mismatch (today the Windows variant runs and says "no virtual cable").

- [ ] **Step 3: Add the macOS variant**

Change the module header's last paragraph (lines 4-7) to:

```rust
//! Wymagania obu ról są różne i to jest źródłem większości nieporozumień:
//! wysyłanie potrzebuje tylko mikrofonu, odbieranie potrzebuje miejsca, w które
//! da się wpuścić cudzy dźwięk tak, żeby wyglądał na mikrofon. W Linuksie to
//! drugie robimy sami, w Windows wymaga cudzego sterownika, a w macOS jeszcze
//! tego nie umiemy — tam ta maszyna tylko wysyła.
```

Insert before the `#[cfg(not(target_os = "linux"))] fn sink()`:

```rust
/// macOS tylko wysyła. Nie ma czego sprawdzać — jest o czym powiedzieć.
#[cfg(target_os = "macos")]
fn sink() -> Check {
    bad(
        Grade::Warn,
        t(K::DocVirtualInput),
        t(K::MacNoReceive),
        t(K::MacNoReceiveAdvice),
    )
}
```

Change the attribute on the Windows variant from `#[cfg(not(target_os = "linux"))]` to:

```rust
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
```

- [ ] **Step 4: Run tests and clippy**

Run:
```bash
cargo test -p mb-app doctor 2>&1 | grep -E 'test result|FAILED'
cargo clippy -p mb-app --all-targets -- -D warnings 2>&1 | tail -3
```
Expected: ok and `Finished`.

- [ ] **Step 5: Look at the real report**

Run: `MICBRIDGE_LANG=en cargo run -q -p mb-app -- doctor`
Expected, in shape:
```
✓ microphone                N devices, default: <your mic>
• virtual input             Receiving as a microphone is not supported on macOS yet.
    This Mac can send its microphone. To hear the remote one through the speakers use --sink device.
• pairing                   nothing paired yet
    ...
Usable, but not every role is available.
```

- [ ] **Step 6: Commit**

```bash
git add crates/mb-app/src/doctor.rs
git commit -m "$(cat <<'EOF'
doctor: say that a Mac only sends

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: `micbridge devices` ends with the macOS note

**Files:**
- Modify: `crates/mb-app/src/main.rs:314-320`

**Interfaces:**
- Consumes: `mb_i18n::Key::{CliSinkMac, CliSinkMacNote}`.
- Produces: nothing new.

- [ ] **Step 1: Change the branch**

Replace lines 314-320 of `crates/mb-app/src/main.rs`:

```rust
    if cfg!(target_os = "linux") {
        println!("{}", t1(K::CliSinkLinux, mb_audio::DISPLAY_NAME));
        println!("{}", t(K::CliSinkLinuxNote));
    } else if cfg!(target_os = "macos") {
        println!("{}", t(K::CliSinkMac));
        println!("{}", t(K::CliSinkMacNote));
    } else {
        println!("{}", t(K::CliSinkWindows));
        println!("{}", t(K::CliSinkWindowsNote));
    }
```

- [ ] **Step 2: Run it**

Run: `MICBRIDGE_LANG=en cargo run -q -p mb-app -- devices | tail -4`
Expected: the last lines are the "Sink: on macOS --sink auto does not work" pair, then the "Without a microphone" tone line. No Windows text.

- [ ] **Step 3: Clippy and commit**

Run: `cargo clippy -p mb-app --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`.

```bash
git add crates/mb-app/src/main.rs
git commit -m "$(cat <<'EOF'
devices: macOS footnote instead of the Windows one

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Window: Receiving disabled and autostart hidden on macOS

**Files:**
- Modify: `crates/mb-gui/src/widgets.rs` (`recv_ui` lines 125-170, `footer_ui` lines 271-283)
- Modify: `crates/mb-gui/src/autostart.rs` (lines 1-28 and the `#[cfg(not(windows))] mod imp` at line 69)

**Interfaces:**
- Consumes: `mb_i18n::Key::{MacNoReceive, MacNoAutostart}`.
- Produces: `autostart::enabled()` is `false` and `autostart::set(true)` errs on macOS. The Receiving toggle and its settings row are inert on macOS.

- [ ] **Step 1: Disable the Receiving half on macOS**

In `crates/mb-gui/src/widgets.rs`, replace the body of `recv_ui` up to and including the `ui.add_enabled_ui(!busy, |ui| {` line with:

```rust
    pub(crate) fn recv_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // W macOS odbieranie odpada (patrz `mb_audio::sink`), więc przełącznik
        // jest wygaszony, a podpis mówi dlaczego. Reszta rysuje się jak
        // zwykle — żeby okno wyglądało tak samo na każdym systemie.
        let can_recv = !cfg!(target_os = "macos");
        let hint = if can_recv {
            t(K::ReceivingHint)
        } else {
            t(K::MacNoReceive)
        };

        let mut wanted = self.recv.is_running();
        section(ui, t(K::Receiving), hint, |ui| {
            let text = label(wanted);
            ui.add_enabled_ui(can_recv, |ui| {
                if ui.toggle_value(&mut wanted, text).clicked() {
                    if wanted {
                        self.start_recv(ctx);
                    } else {
                        self.recv.stop();
                    }
                }
            });
        });

        let busy = self.recv.is_running();
        ui.add_enabled_ui(!busy && can_recv, |ui| {
```

The rest of the function (the `horizontal_wrapped` block, the `busy` label, the status) stays exactly as it is.

- [ ] **Step 2: Hide the autostart checkbox on macOS**

In `footer_ui`, wrap the checkbox so the block reads:

```rust
        ui.horizontal_wrapped(|ui| {
            // W macOS autostartu nie ma (patrz `autostart.rs`), więc nie ma
            // co pokazywać przełącznika, który i tak by odmówił.
            if !cfg!(target_os = "macos")
                && ui.checkbox(&mut self.autostart, t(K::Autostart)).changed()
            {
                self.footer_error = match autostart::set(self.autostart) {
                    Ok(()) => None,
                    Err(e) => {
                        // Przełącznik ma pokazywać stan faktyczny, nie życzenie.
                        self.autostart = !self.autostart;
                        Some(format!("{e}"))
                    }
                };
            }

            ui.add_space(12.0);
            self.language_ui(ui);
        });
```

- [ ] **Step 3: Give autostart a macOS implementation**

In `crates/mb-gui/src/autostart.rs`, replace the header comment (lines 1-5) with:

```rust
//! Uruchamianie razem z systemem — opcja, nie domyślne zachowanie.
//!
//! Każdy system robi to inaczej i żaden nie potrzebuje do tego biblioteki:
//! w Windows to wpis w rejestrze, w Linuksie plik `.desktop` w katalogu
//! autostartu. Obie drogi da się cofnąć jednym skasowaniem. W macOS byłby to
//! LaunchAgent — jeszcze go nie ma, więc tam opcja uczciwie odmawia.
```

Gate the import and the two helpers that only the Windows and Linux paths
use, so clippy's `-D warnings` stays clean on macOS. Change line 7:

```rust
use anyhow::{Context, Result};
```
to
```rust
#[cfg(not(target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;
```

Change:

```rust
/// Pod tą nazwą wpis widnieje w systemie.
const ENTRY: &str = "MicBridge";
```
to
```rust
/// Pod tą nazwą wpis widnieje w systemie.
#[cfg(not(target_os = "macos"))]
const ENTRY: &str = "MicBridge";
```
and
```rust
fn exe() -> Result<std::path::PathBuf> {
```
to
```rust
#[cfg(not(target_os = "macos"))]
fn exe() -> Result<std::path::PathBuf> {
```

Change the Linux module's attribute from `#[cfg(not(windows))]` to `#[cfg(not(any(windows, target_os = "macos")))]`, and insert this module before it:

```rust
#[cfg(target_os = "macos")]
mod imp {
    use super::Result;

    pub fn enabled() -> Result<bool> {
        Ok(false)
    }

    pub fn enable() -> Result<()> {
        anyhow::bail!("{}", mb_i18n::t(mb_i18n::Key::MacNoAutostart))
    }

    /// Nie było czego włączać, więc nie ma czego wyłączać.
    pub fn disable() -> Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 4: Build, clippy, fmt**

Run:
```bash
cargo fmt --all
cargo clippy -p mb-gui --all-targets -- -D warnings 2>&1 | tail -3
cargo test -p mb-gui 2>&1 | grep -E 'test result|FAILED'
```
Expected: `Finished`, tests ok. If clippy still reports an unused item in `autostart.rs` on macOS, gate that item with `#[cfg(not(target_os = "macos"))]` the same way as `exe`.

- [ ] **Step 5: Open the window and look**

Run: `MICBRIDGE_LANG=en cargo run -q -p mb-gui`
Expected: the Receiving half's hint reads "Receiving as a microphone is not supported on macOS yet.", its toggle and the Output/Buffer row are greyed out, the Sending half lists the Mac's microphones, and the footer has only the language picker. A menu-bar icon appears with Show and Quit. Close the window with the red button: the program quits (no hiding on macOS). Quit via the tray while a window is open also works.

- [ ] **Step 6: Commit**

```bash
git add crates/mb-gui/src/widgets.rs crates/mb-gui/src/autostart.rs
git commit -m "$(cat <<'EOF'
gui: on macOS disable Receiving and hide autostart

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: CI job, README lines and the macOS guide

**Files:**
- Modify: `.github/workflows/ci.yml:19`
- Modify: `README.md` (Install table line 29, "Where the audio lands" lines 96-104, "Building from source" lines 215-216)
- Create: `docs/MACOS.md`

**Interfaces:**
- Consumes: the CLI behaviour from Tasks 3-5 (the guide quotes expected output).
- Produces: nothing programmatic.

- [ ] **Step 1: Add macOS to the CI matrix**

In `.github/workflows/ci.yml`, change line 19 to:

```yaml
        os: [ubuntu-latest, windows-latest, macos-latest]
```

The Linux dependency step is already conditioned on `runner.os == 'Linux'`; GitHub's macOS runners ship cmake and the Xcode tools, so no new step is needed.

- [ ] **Step 2: README install table**

After the Arch row (line 29) add:

```markdown
| **macOS** | send-only, built from source — see [docs/MACOS.md](docs/MACOS.md) |
```

- [ ] **Step 3: README "Where the audio lands"**

Replace the section's first sentence ("This is the one place where the two systems genuinely differ.") with:

```markdown
This is the one place where the systems genuinely differ.
```

Immediately after the table (after the `device`, or part of a name row), add:

```markdown
On **macOS** MicBridge only sends. `--sink auto` and `--sink virtual` are
refused at startup; `--sink device` or a device name plays the remote
microphone through the speakers, which is useful for a test and for nothing
else. Making the remote microphone appear as an input device on a Mac would
need a virtual audio driver, and that is not done yet.
```

- [ ] **Step 4: README "Building from source"**

Replace the paragraph starting "On Windows the MSVC toolchain plus CMake is enough" with:

```markdown
On Windows the MSVC toolchain plus CMake is enough; everything else comes from
crates. On macOS it is the Xcode command line tools plus `brew install cmake`;
the step-by-step is in [docs/MACOS.md](docs/MACOS.md).
```

- [ ] **Step 5: Write the guide**

Create `docs/MACOS.md`:

````markdown
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
````

- [ ] **Step 6: Check the config path claim in the guide**

Run: `MICBRIDGE_LANG=en micbridge peers 2>&1 | head -3; ls "$HOME/Library/Application Support/micbridge" 2>&1`
Expected: either a listing or "nothing paired yet"; the directory exists or is created on first pairing. If `dirs::config_dir()` resolves somewhere else (it should be `~/Library/Application Support` on macOS), correct the table in the guide to the real path.

- [ ] **Step 7: Commit**

```bash
git add .github/workflows/ci.yml README.md docs/MACOS.md
git commit -m "$(cat <<'EOF'
macOS: CI job, README pointers and a setup guide

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Whole-workspace verification and a real send

**Files:**
- None modified. Verification only.

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: The same gates CI runs**

Run:
```bash
cargo fmt --all --check && echo FMT-OK
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -2
cargo test --workspace 2>&1 | grep -E 'test result|FAILED'
cargo build --workspace --release 2>&1 | tail -2
```
Expected: `FMT-OK`, two `Finished` lines, every `test result: ok`.

- [ ] **Step 2: Install the release binaries the way the guide says**

Run:
```bash
cargo install --path crates/mb-app
cargo install --path crates/mb-gui
which micbridge micbridge-gui
MICBRIDGE_LANG=en micbridge doctor
```
Expected: both in `~/.cargo/bin`; doctor as in Task 4 step 5.

- [ ] **Step 3: Send to the other machine**

With `micbridge recv --sink auto` running on the Linux or Windows machine:

```bash
MICBRIDGE_LANG=en micbridge send --device "<part of your mic name>"
```
Expected: the receiver shows a six-digit code, the Mac asks for it, then both sides print latency and loss figures and the receiver's application hears the Mac. Stop with Ctrl-C; the receiver notes the sender left.

If the Mac cannot find the receiver, retry with `--to <receiver IP>` and note which of the two worked in the task notes.

- [ ] **Step 4: Window round trip**

Run `micbridge-gui`, flip the Sending switch, pick the same microphone, confirm the graph moves and the receiver hears audio. Flip it off. Quit from the menu-bar icon.

- [ ] **Step 5: Record the outcome**

No commit unless something had to change. If a step failed, the fix goes in the task that owns that code, with its own commit, and this task reruns from Step 1.
