//! Ikona programu w dwóch miejscach, gdzie rysuje ją sam program: w zasobniku
//! systemowym i na pasku okna.
//!
//! Trzymamy ją jako gotowe piksele. Dekoder PNG wniesiony po to, żeby raz przy
//! starcie rozpakować dwa obrazki, kosztowałby więcej niż dwadzieścia kilobajtów
//! w repozytorium — a przy okazji dołożyłby błąd tam, gdzie teraz go nie ma.
//!
//! Pliki robi `packaging/icons/generate.py` z tego samego opisu kształtu, co
//! ikonę w menu aplikacji i w instalatorze. Dlatego program wygląda wszędzie
//! tak samo i nie da się poprawić jednej postaci, zapominając o drugiej.

use anyhow::{anyhow, Result};
use eframe::egui;

const TRAY_SIDE: u32 = 32;
const WINDOW_SIDE: u32 = 64;

const TRAY: &[u8] = include_bytes!("../../../packaging/icons/tray-32.rgba");
const WINDOW: &[u8] = include_bytes!("../../../packaging/icons/window-64.rgba");

// Rozmiar buforów sprawdzamy w czasie budowania: gdyby ktoś przegenerował
// ikony w innym rozmiarze i nie poprawił stałych, program nie skompiluje się
// zamiast pokazać przesuniętą kaszę.
const _: () = assert!(TRAY.len() == (TRAY_SIDE * TRAY_SIDE * 4) as usize);
const _: () = assert!(WINDOW.len() == (WINDOW_SIDE * WINDOW_SIDE * 4) as usize);

pub fn tray() -> Result<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(TRAY.to_vec(), TRAY_SIDE, TRAY_SIDE)
        .map_err(|e| anyhow!("nie mogę zbudować ikony w zasobniku: {e}"))
}

pub fn window() -> egui::IconData {
    egui::IconData {
        rgba: WINDOW.to_vec(),
        width: WINDOW_SIDE,
        height: WINDOW_SIDE,
    }
}
