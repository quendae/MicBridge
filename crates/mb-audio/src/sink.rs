//! Gdzie ląduje odebrany dźwięk.
//!
//! Dwie drogi, bo dwa systemy dają co innego (§4 architektury):
//!
//! * **Linux** — tworzymy własny węzeł PipeWire i sami jesteśmy mikrofonem.
//!   Nic nie trzeba instalować.
//! * **Windows** — nie da się utworzyć urządzenia wejściowego bez sterownika
//!   trybu jądra, więc piszemy do cudzego wirtualnego kabla, który znajdujemy
//!   po nazwie.
//!
//! Poza tym jednym kafelkiem cała reszta silnika jest symetryczna, dlatego
//! różnica siedzi tutaj i nigdzie indziej.

use anyhow::{bail, Result};

use crate::{start_playback, Direction, PlaybackHandle};

/// Nazwa, pod którą chcemy się pokazać na liście mikrofonów.
pub const DISPLAY_NAME: &str = "MicBridge";

/// Nazwy cudzych wirtualnych kabli, w kolejności, w jakiej ich szukamy.
/// Dopasowanie jest po fragmencie, bez rozróżniania wielkości liter, bo
/// producenci doklejają sufiksy w rodzaju „(VB-Audio Virtual Cable)”.
pub const VIRTUAL_SINK_HINTS: &[&str] = &[
    "cable input", // VB-CABLE
    "voicemeeter input",
    "voicemeeter aux input",
    "line 1 (virtual audio cable)",
];

pub enum Sink {
    /// Zwykłe urządzenie wyjściowe systemu — także wirtualny kabel, bo dla
    /// systemu to po prostu głośnik.
    Device(PlaybackHandle),
    /// Własny węzeł PipeWire udający mikrofon.
    #[cfg(target_os = "linux")]
    Virtual(crate::pipewire_source::VirtualSource),
}

impl Sink {
    pub fn name(&self) -> &str {
        match self {
            Sink::Device(h) => &h.device_name,
            #[cfg(target_os = "linux")]
            Sink::Virtual(v) => v.description(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        match self {
            Sink::Device(h) => h.sample_rate,
            #[cfg(target_os = "linux")]
            Sink::Virtual(v) => v.sample_rate(),
        }
    }

    /// Czy aplikacje zobaczą to jako mikrofon bez dalszych zabiegów.
    pub fn is_input_device(&self) -> bool {
        match self {
            Sink::Device(_) => false,
            #[cfg(target_os = "linux")]
            Sink::Virtual(_) => true,
        }
    }
}

/// Otwórz ujście wskazane selektorem.
///
/// * `auto` — w Linuksie tworzy własny węzeł, w Windows szuka wirtualnego kabla
/// * `virtual` — wymusza własny węzeł (tylko Linux)
/// * `device` — wymusza urządzenie systemowe, nawet w Linuksie
/// * cokolwiek innego — fragment nazwy albo `@N`, tak jak przy mikrofonie
pub fn open_sink<F>(selector: &str, rate: u32, fill: F) -> Result<Sink>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    let selector = selector.trim();

    if selector.eq_ignore_ascii_case("virtual") {
        return open_virtual(rate, fill);
    }

    if selector.eq_ignore_ascii_case("auto") {
        #[cfg(target_os = "linux")]
        {
            return open_virtual(rate, fill);
        }
        #[cfg(not(target_os = "linux"))]
        {
            return open_cable(rate, fill);
        }
    }

    let device = if selector.eq_ignore_ascii_case("device") {
        "default"
    } else {
        selector
    };
    Ok(Sink::Device(start_playback(device, rate, fill)?))
}

#[cfg(target_os = "linux")]
fn open_virtual<F>(rate: u32, fill: F) -> Result<Sink>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    Ok(Sink::Virtual(crate::pipewire_source::VirtualSource::new(
        DISPLAY_NAME,
        rate,
        fill,
    )?))
}

#[cfg(not(target_os = "linux"))]
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

/// Znajdź cudzy wirtualny kabel po nazwie.
#[allow(dead_code)]
fn open_cable<F>(rate: u32, fill: F) -> Result<Sink>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    let devices = crate::list(Direction::Output)?;
    let found = VIRTUAL_SINK_HINTS.iter().find_map(|hint| {
        devices
            .iter()
            .find(|d| d.name.to_lowercase().contains(hint))
            .map(|d| d.name.clone())
    });

    let Some(name) = found else {
        bail!(
            "nie znaleziono wirtualnego urządzenia audio.\n\
             \n\
             Windows nie pozwala programowi utworzyć własnego mikrofonu — \
             wymaga sterownika\n\
             podpisanego przez producenta. Potrzebny jest jednorazowo \
             VB-CABLE:\n\
             \n\
               1. pobierz i zainstaluj https://vb-audio.com/Cable/\n\
               2. uruchom ponownie MicBridge\n\
               3. w aplikacji (Discord, gra) wybierz mikrofon \
             „CABLE Output”\n\
             \n\
             Albo wskaż urządzenie ręcznie: --sink \"<fragment nazwy>\".\n\
             \n\
             Widoczne urządzenia wyjściowe:\n  {}",
            devices
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    };

    tracing::info!(device = %name, "wykryto wirtualny kabel");
    Ok(Sink::Device(start_playback(&name, rate, fill)?))
}

/// Czy selektor ma szansę zadziałać na tym systemie.
///
/// Ujście otwieramy dopiero po uzgodnieniu, żeby komunikat o braku kabla padał
/// w kontekście konkretnej próby połączenia. Konfiguracja niemożliwa z zasady
/// nie ma jednak na co czekać — inaczej odbiornik w Windows z `--sink virtual`
/// wygląda na działający, dopóki ktoś się nie połączy.
pub fn validate(selector: &str) -> Result<()> {
    if selector.trim().eq_ignore_ascii_case("virtual") && !cfg!(target_os = "linux") {
        bail!(
            "`--sink virtual` działa tylko w Linuksie, gdzie PipeWire pozwala \
             utworzyć własny mikrofon.\n\
             W Windows użyj `--sink auto` (znajdzie wirtualny kabel) albo podaj \
             fragment nazwy urządzenia."
        );
    }
    Ok(())
}

/// Czy nazwa wygląda na wirtualny kabel — do podpowiedzi w `micbridge devices`.
pub fn looks_like_virtual_cable(name: &str) -> bool {
    let lowered = name.to_lowercase();
    VIRTUAL_SINK_HINTS.iter().any(|h| lowered.contains(h))
}

/// Wskazówka wyświetlana po połączeniu, gdy dźwięk idzie przez cudzy kabel.
///
/// Domyślne 7168 sampli w VB-CABLE to 149 ms zbędnego buforowania — więcej niż
/// cały reszta budżetu opóźnienia razem wzięta (§7 architektury). Nie umiemy
/// tego odczytać spoza sterownika, więc mówimy o tym wprost zamiast udawać, że
/// sprawdziliśmy.
pub fn latency_hint(sink_name: &str) -> Option<&'static str> {
    if sink_name.to_lowercase().contains("cable input") {
        Some(
            "Wskazówka: w VBCABLE_ControlPanel.exe ustaw „Max Latency” na 2048 \
             sampli.\n  Domyślne 7168 dokłada ~130 ms opóźnienia — więcej niż \
             cała reszta łańcucha.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_virtual_sink_is_refused_up_front_off_linux() {
        let result = validate("virtual");
        assert_eq!(result.is_err(), !cfg!(target_os = "linux"));
    }

    #[test]
    fn every_other_selector_passes_validation() {
        for s in ["auto", "device", "@0", "cable input", "  AUTO  "] {
            assert!(validate(s).is_ok(), "odrzucono `{s}`");
        }
    }

    #[test]
    fn cable_names_are_recognised_regardless_of_case_and_suffix() {
        assert!(looks_like_virtual_cable(
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(looks_like_virtual_cable("VoiceMeeter Input (VB-Audio)"));
        assert!(!looks_like_virtual_cable("Głośniki (Realtek Audio)"));
    }

    #[test]
    fn the_latency_hint_fires_only_for_vb_cable() {
        assert!(latency_hint("CABLE Input (VB-Audio Virtual Cable)").is_some());
        assert!(latency_hint("Głośniki").is_none());
    }
}
