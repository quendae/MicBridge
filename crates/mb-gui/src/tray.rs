//! Ikona w zasobniku systemowym.
//!
//! Program tego rodzaju stoi włączony całymi dniami i nie ma po co zajmować
//! paska zadań. Zamknięcie okna go więc chowa, a nie kończy — wyjście jest
//! w menu ikony, żeby nikt nie zamknął przez pomyłkę sesji, która akurat gra.
//!
//! Zdarzeń nie odbieramy z kanału biblioteki, tylko podstawiamy własną obsługę.
//! Kanał trzeba by odpytywać przy rysowaniu klatki, a schowane okno klatek nie
//! dostaje — menu wyglądałoby na żywe i nie robiło nic. Obsługa woła [`Waker`],
//! ten przywraca okno, a dopiero potem pracę przejmuje zwykły bieg programu.
//!
//! Dwa systemy, dwie drogi. W Windows ikona żyje w pętli komunikatów okna,
//! którą i tak mamy. W Linuksie wymaga GTK, a GTK nie pozwala się dotykać
//! z innego wątku niż ten, który je zainicjował — pętla okna należy do winit
//! i podzielić się nią nie da, więc cała ikona powstaje i mieszka na własnym
//! wątku. To, gdzie stoi, nie ma dla reszty programu znaczenia: obie strony
//! rozmawiają przez skrzynkę niżej.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::wake::Waker;

/// Co użytkownik zrobił z ikoną.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Show,
    Quit,
}

/// Skrzynka na jedno życzenie, wystawiona między obsługę zdarzeń a okno.
///
/// Jedno wystarczy: klikanie w ikonę szybciej, niż okno zdąży się odrysować,
/// nie znaczy nic więcej niż jedno kliknięcie.
type Slot = Arc<Mutex<Option<Action>>>;

pub struct Tray {
    /// Uchwyt trzymany po to, żeby ikona nie zniknęła razem z nim.
    ///
    /// W Linuksie jest pusty: ikona żyje na wątku GTK i nie wolno jej stamtąd
    /// zabierać ani tam zaglądać.
    _icon: Option<TrayIcon>,
    slot: Slot,
}

impl Tray {
    /// Zabiera życzenie ze skrzynki, jeśli jakieś czeka.
    pub fn poll(&self) -> Option<Action> {
        self.slot.lock().ok().and_then(|mut slot| slot.take())
    }
}

#[cfg(not(target_os = "linux"))]
impl Tray {
    pub fn new(waker: &Waker) -> Result<Self> {
        let slot = Slot::default();
        let icon = build(&slot, waker)?;
        Ok(Self {
            _icon: Some(icon),
            slot,
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
    pub fn new(waker: &Waker) -> Result<Self> {
        use std::sync::mpsc;

        let slot = Slot::default();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let theirs = Arc::clone(&slot);
        let waker = waker.clone();
        std::thread::Builder::new()
            .name("micbridge-tray".into())
            .spawn(move || {
                if let Err(e) = gtk::init() {
                    let _ = ready_tx.try_send(Err(format!("GTK nie wystartowało: {e}")));
                    return;
                }
                match build(&theirs, &waker) {
                    Ok(icon) => {
                        let _ = ready_tx.try_send(Ok(()));
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
            Ok(Ok(())) => Ok(Self { _icon: None, slot }),
            Ok(Err(e)) => Err(anyhow!("{e}")),
            Err(_) => Err(anyhow!("wątek ikony zakończył się przed startem")),
        }
    }
}

/// Wkłada życzenie do skrzynki.
///
/// Zakończenie ma pierwszeństwo: gdyby oba trafiły przed jedną klatką,
/// pokazanie okna tylko odwlekłoby wyjście, o które ktoś właśnie poprosił.
fn note(slot: &Slot, action: Action) {
    if let Ok(mut slot) = slot.lock() {
        if *slot != Some(Action::Quit) {
            *slot = Some(action);
        }
    }
}

/// Buduje menu i ikonę. Wołane na tym wątku, który dla danego systemu jest
/// właściwy — w Linuksie na wątku GTK, gdzie indziej na wątku okna.
fn build(slot: &Slot, waker: &Waker) -> Result<TrayIcon> {
    let show = MenuItem::new(mb_i18n::t(mb_i18n::Key::ShowWindow), true, None);
    let quit = MenuItem::new(mb_i18n::t(mb_i18n::Key::Quit), true, None);
    let menu = Menu::new();
    let bad = |e: tray_icon::menu::Error| anyhow!("nie mogę zbudować menu ikony: {e}");
    menu.append(&show).map_err(bad)?;
    menu.append(&PredefinedMenuItem::separator()).map_err(bad)?;
    menu.append(&quit).map_err(bad)?;

    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
    let (theirs, their_waker) = (Arc::clone(slot), waker.clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id == quit_id {
            Action::Quit
        } else if event.id == show_id {
            Action::Show
        } else {
            return;
        };
        note(&theirs, action);
        their_waker.wake();
    }));

    let (theirs, their_waker) = (Arc::clone(slot), waker.clone());
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Kliknięcie w samą ikonę też ma przywracać okno — tego się po ikonie
        // w zasobniku spodziewa każdy.
        if let TrayIconEvent::Click { button, .. } = event {
            if button == tray_icon::MouseButton::Left {
                note(&theirs, Action::Show);
                their_waker.wake();
            }
        }
    }));

    TrayIconBuilder::new()
        .with_tooltip("MicBridge")
        .with_menu(Box::new(menu))
        .with_icon(crate::icon::tray()?)
        .build()
        .map_err(|e| anyhow!("nie mogę utworzyć ikony w zasobniku: {e}"))
}
