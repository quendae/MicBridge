//! Budzenie okna, którego nie ma na ekranie.
//!
//! eframe rysuje klatkę dopiero wtedy, gdy system poprosi okno o odrysowanie,
//! a schowanego okna system o nic nie prosi — ani w Windows, ani nigdzie
//! indziej. Program zwinięty do zasobnika przestaje więc dostawać klatki,
//! a razem z nimi przestaje zauważać, że ktoś kliknął w ikonę. Menu wygląda
//! na żywe i nie robi nic; jedyne wyjście z takiego stanu to zabicie procesu.
//!
//! Same zdarzenia z zasobnika docierają niezależnie od klatek — przychodzą
//! pętlą komunikatów, która chodzi także wtedy, gdy nie ma co rysować. Stąd ten
//! budzik: wołany prosto z obsługi zdarzenia przywraca okno na ekran, a wtedy
//! klatki wracają i reszta programu działa jak zwykle.
//!
//! Robi to wprost przez system, z pominięciem eframe, bo polecenia dla okna
//! eframe wykonuje... przy rysowaniu klatki. Wysłanie stamtąd prośby
//! „pokaż się” zapętliłoby problem zamiast go rozwiązać.

use eframe::egui;

#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Clone)]
pub struct Waker {
    ctx: egui::Context,
    /// Uchwyt okna w rozumieniu systemu. Zero znaczy, że go nie dostaliśmy.
    #[cfg(windows)]
    hwnd: isize,
}

impl Waker {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            ctx: cc.egui_ctx.clone(),
            #[cfg(windows)]
            hwnd: match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => 0,
            },
        }
    }

    /// Czy z tego systemu umiemy przywrócić schowane okno.
    ///
    /// Gdy nie umiemy, okno nie ma prawa się chować — bo nie byłoby jak do
    /// niego wrócić. Reszta programu pyta o to przed zwinięciem do zasobnika.
    pub fn can_restore(&self) -> bool {
        #[cfg(windows)]
        {
            self.hwnd != 0
        }
        // W Linuksie i macOS ikona w zasobniku nadal działa — tyle że przy
        // widocznym oknie, które klatki dostaje. Chowanie włączymy tam dopiero
        // razem ze sprawdzoną drogą powrotu.
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Wołane z obsługi zdarzenia zasobnika. Musi doprowadzić do klatki.
    pub fn wake(&self) {
        #[cfg(windows)]
        self.show();
        self.ctx.request_repaint();
    }

    #[cfg(windows)]
    fn show(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IsWindowVisible, SetForegroundWindow, ShowWindow, SW_SHOW,
        };

        if self.hwnd == 0 {
            return;
        }
        let hwnd = self.hwnd as *mut core::ffi::c_void;
        // SAFETY: uchwyt pochodzi od okna, które żyje tyle co program, a te
        // trzy wywołania idą z wątku pętli komunikatów — czyli z tego samego,
        // który to okno utworzył. Windows tego wymaga.
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
            }
        }
    }
}
