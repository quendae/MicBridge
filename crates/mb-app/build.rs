//! Wkłada ikonę i opis do pliku wykonywalnego dla Windows.
//!
//! Ikona ustawiana w kodzie pojawia się dopiero razem z oknem. Wszystko, co
//! pokazuje program, zanim okno wstanie — Eksplorator, pasek zadań, spis
//! zainstalowanych programów, okno podniesienia uprawnień — czyta zasoby
//! samego pliku .exe. Bez nich program ma ikonę w menu, a w Eksploratorze
//! pustą kartkę i nazwę pliku zamiast nazwy.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../packaging/icons/micbridge.ico");
        let mut zasoby = winresource::WindowsResource::new();
        zasoby.set_icon("../../packaging/icons/micbridge.ico");
        // Nazwa pakietu to `mb-gui`, a użytkownik ma widzieć „MicBridge” —
        // między innymi w oknie podniesienia uprawnień, gdzie stoi ona za
        // całą tożsamość programu.
        zasoby.set("ProductName", "MicBridge");
        zasoby.set("FileDescription", "MicBridge");
        if let Err(e) = zasoby.compile() {
            // Zasoby składa kompilator z SDK Windows. Gdy go nie ma, wolimy
            // program bez ikony niż brak programu — to ozdoba, nie działanie.
            println!("cargo:warning=nie wszedłem z ikoną do pliku wykonywalnego: {e}");
        }
    }
}
