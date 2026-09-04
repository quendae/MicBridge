//! Ikona w zasobniku systemowym.
//!
//! Program tego rodzaju stoi włączony całymi dniami i nie ma po co zajmować
//! paska zadań. Zamknięcie okna go więc chowa, a nie kończy — wyjście jest
//! w menu ikony, żeby nikt nie zamknął przez pomyłkę sesji, która akurat gra.
//!
//! Dwa systemy, dwie drogi. W Windows ikona żyje w pętli komunikatów okna,
//! którą i tak mamy. W Linuksie wymaga GTK, a GTK nie pozwala się dotykać
//! z innego wątku niż ten, który je zainicjował — pętla okna należy do winit
//! i podzielić się nią nie da, więc cała ikona powstaje i mieszka na własnym
//! wątku. Rozmawia z nami przez globalne kanały biblioteki, więc to, gdzie
//! stoi, nie ma dla reszty programu znaczenia.

use anyhow::{anyhow, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Co użytkownik zrobił z ikoną.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Show,
    Quit,
}

/// Identyfikatory pozycji menu — jedyne, co musi wrócić z wątku ikony.
struct Ids {
    show: MenuId,
    quit: MenuId,
}

pub struct Tray {
    /// Uchwyt trzymany po to, żeby ikona nie zniknęła razem z nim.
    ///
    /// W Linuksie jest pusty: ikona żyje na wątku GTK i nie wolno jej stamtąd
    /// zabierać ani tam zaglądać.
    _icon: Option<TrayIcon>,
    ids: Ids,
}

impl Tray {
    /// Zbiera to, co wydarzyło się od ostatniego zajrzenia.
    ///
    /// Kanały są globalne i nie blokują, więc wołamy to przy odrysowaniu okna.
    pub fn poll(&self) -> Option<Action> {
        let mut action = None;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.ids.quit {
                return Some(Action::Quit);
            }
            if event.id == self.ids.show {
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

#[cfg(not(target_os = "linux"))]
impl Tray {
    pub fn new() -> Result<Self> {
        let (icon, ids) = build()?;
        Ok(Self {
            _icon: Some(icon),
            ids,
        })
    }
}

#[cfg(target_os = "linux")]
impl Tray {
    /// Startuje GTK na własnym wątku i tam buduje ikonę.
    ///
    /// Wraca dopiero, gdy ikona stoi albo gdy wiadomo, że nie stanie —
    /// inaczej okno zdążyłoby się narysować bez wiedzy, czy ma dokąd się
    /// chować.
    pub fn new() -> Result<Self> {
        use std::sync::mpsc;

        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<Ids, String>>(1);
        std::thread::Builder::new()
            .name("micbridge-tray".into())
            .spawn(move || {
                if let Err(e) = gtk::init() {
                    let _ = ready_tx.try_send(Err(format!("GTK nie wystartowało: {e}")));
                    return;
                }
                match build() {
                    Ok((icon, ids)) => {
                        let _ = ready_tx.try_send(Ok(ids));
                        // Ikona musi przeżyć pętlę — i zniknąć dopiero z nią.
                        gtk::main();
                        drop(icon);
                    }
                    Err(e) => {
                        let _ = ready_tx.try_send(Err(e.to_string()));
                    }
                }
            })
            .map_err(|e| anyhow!("nie mogę uruchomić wątku ikony: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(ids)) => Ok(Self { _icon: None, ids }),
            Ok(Err(e)) => Err(anyhow!("{e}")),
            Err(_) => Err(anyhow!("wątek ikony zakończył się przed startem")),
        }
    }
}

/// Buduje menu i ikonę. Wołane na tym wątku, który dla danego systemu jest
/// właściwy — w Linuksie na wątku GTK, gdzie indziej na wątku okna.
fn build() -> Result<(TrayIcon, Ids)> {
    let show = MenuItem::new("Pokaż okno", true, None);
    let quit = MenuItem::new("Zakończ", true, None);
    let menu = Menu::new();
    let bad = |e: tray_icon::menu::Error| anyhow!("nie mogę zbudować menu ikony: {e}");
    menu.append(&show).map_err(bad)?;
    menu.append(&PredefinedMenuItem::separator()).map_err(bad)?;
    menu.append(&quit).map_err(bad)?;

    let ids = Ids {
        show: show.id().clone(),
        quit: quit.id().clone(),
    };

    let icon = TrayIconBuilder::new()
        .with_tooltip("MicBridge")
        .with_menu(Box::new(menu))
        .with_icon(crate::icon::tray()?)
        .build()
        .map_err(|e| anyhow!("nie mogę utworzyć ikony w zasobniku: {e}"))?;

    Ok((icon, ids))
}
