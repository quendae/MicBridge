//! Ikona w zasobniku systemowym.
//!
//! Program tego rodzaju stoi włączony całymi dniami i nie ma po co zajmować
//! paska zadań. Zamknięcie okna go więc chowa, a nie kończy — wyjście jest
//! w menu ikony, żeby nikt nie zamknął przez pomyłkę sesji, która akurat gra.
//!
//! W Windows ikona żyje w pętli komunikatów okna, którą i tak mamy. W Linuksie
//! wymaga pętli GTK, osobnej od pętli okna, więc dostaje własny wątek.

use anyhow::{anyhow, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Co użytkownik zrobił z ikoną.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Show,
    Quit,
}

pub struct Tray {
    /// Uchwyt trzymany po to, żeby ikona nie zniknęła razem z nim.
    _icon: TrayIcon,
    show_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

impl Tray {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "linux")]
        start_gtk()?;

        let show = MenuItem::new("Pokaż okno", true, None);
        let quit = MenuItem::new("Zakończ", true, None);
        let menu = Menu::new();
        menu.append(&show)
            .map_err(|e| anyhow!("nie mogę zbudować menu ikony: {e}"))?;
        menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
            .map_err(|e| anyhow!("nie mogę zbudować menu ikony: {e}"))?;
        menu.append(&quit)
            .map_err(|e| anyhow!("nie mogę zbudować menu ikony: {e}"))?;

        let icon = TrayIconBuilder::new()
            .with_tooltip("MicBridge")
            .with_menu(Box::new(menu))
            .with_icon(icon()?)
            .build()
            .map_err(|e| anyhow!("nie mogę utworzyć ikony w zasobniku: {e}"))?;

        Ok(Self {
            _icon: icon,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
        })
    }

    /// Zbiera to, co wydarzyło się od ostatniego zajrzenia.
    ///
    /// Kanały są globalne i nie blokują, więc wołamy to przy odrysowaniu okna.
    pub fn poll(&self) -> Option<Action> {
        let mut action = None;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                return Some(Action::Quit);
            }
            if event.id == self.show_id {
                action = Some(Action::Show);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            // Kliknięcie w samą ikonę też ma przywracać okno — tego się po
            // ikonie w zasobniku spodziewa każdy.
            if let TrayIconEvent::Click { button, .. } = event {
                if button == tray_icon::MouseButton::Left {
                    action = Some(Action::Show);
                }
            }
        }
        action
    }
}

/// Ikona rysowana w kodzie: mikrofon na okrągłym tle.
///
/// Wolę dwadzieścia linii arytmetyki niż plik graficzny do wnoszenia przez
/// wszystkie etapy pakowania.
fn icon() -> Result<tray_icon::Icon> {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);

    let centre = (SIZE as f32 - 1.0) / 2.0;
    let radius = centre - 0.5;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - centre, y as f32 - centre);
            let inside = dx * dx + dy * dy <= radius * radius;

            // Kapsuła mikrofonu: prostokąt u góry, nóżka i podstawka.
            let capsule = dx.abs() <= 3.5 && (-9.0..=1.0).contains(&dy);
            let stem = dx.abs() <= 1.0 && (1.0..=7.0).contains(&dy);
            let base = dy > 6.0 && dy <= 8.0 && dx.abs() <= 5.0;
            let mic = capsule || stem || base;

            rgba.extend_from_slice(&match (inside, mic) {
                (_, true) => [245, 245, 250, 255],
                (true, false) => [40, 90, 150, 255],
                (false, false) => [0, 0, 0, 0],
            });
        }
    }

    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE)
        .map_err(|e| anyhow!("nie mogę zbudować ikony: {e}"))
}

/// GTK musi wystartować, zanim powstanie ikona, i mieć własną pętlę.
///
/// Pętla okna należy do winit i nie da się jej podzielić, więc GTK dostaje
/// osobny wątek. Ikona i tak rozmawia z nami przez globalne kanały.
#[cfg(target_os = "linux")]
fn start_gtk() -> Result<()> {
    use std::sync::mpsc;

    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("micbridge-tray".into())
        .spawn(move || match gtk::init() {
            Ok(()) => {
                let _ = ready_tx.try_send(Ok(()));
                gtk::main();
            }
            Err(e) => {
                let _ = ready_tx.try_send(Err(e.to_string()));
            }
        })
        .map_err(|e| anyhow!("nie mogę uruchomić wątku ikony: {e}"))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow!("GTK nie wystartowało: {e}")),
        Err(_) => Err(anyhow!("wątek ikony zakończył się przed startem")),
    }
}
