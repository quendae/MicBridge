//! Wirtualny mikrofon w Linuksie — własny węzeł PipeWire.
//!
//! To jest linuksowa połowa §4 architektury i miejsce, w którym ten system
//! wygrywa z Windows: proces może zgłosić się grafowi jako *źródło* dźwięku,
//! bez sterownika, bez podpisu, bez instalowania czegokolwiek. Węzeł pojawia
//! się w `pavucontrol` i na listach mikrofonów w Discordzie czy OBS-ie pod
//! nazwą, którą sami nadamy, i znika, gdy proces się kończy.
//!
//! Sztuczka polega na tym, że strumień łączymy w kierunku `Output` — my w niego
//! piszemy — ale deklarujemy `media.class = Audio/Source`, więc dla reszty
//! grafu jest mikrofonem.
//!
//! Pętla PipeWire jest blokująca i musi mieć własny wątek. Callback `process`
//! biegnie na tym wątku z priorytetem czasu rzeczywistego: żadnych alokacji,
//! blokad ani wejścia-wyjścia.

use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::pod::Pod;

/// Nazwa węzła, po której użytkownik ma go rozpoznać.
pub const NODE_NAME: &str = "micbridge";

/// f32 na próbkę, mono — stąd krok równy rozmiarowi jednej próbki.
const STRIDE: usize = std::mem::size_of::<f32>();

pub struct VirtualSource {
    /// Wysłanie czegokolwiek zatrzymuje pętlę PipeWire.
    quit: Option<pw::channel::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    description: String,
    rate: u32,
}

impl VirtualSource {
    /// Tworzy węzeł i uruchamia jego pętlę. Wraca dopiero, gdy PipeWire
    /// potwierdzi połączenie strumienia albo zwróci błąd — inaczej odbiornik
    /// meldowałby gotowość, zanim aplikacje zobaczyłyby mikrofon.
    pub fn new<F>(description: &str, rate: u32, fill: F) -> Result<Self>
    where
        F: FnMut(&mut [f32]) + Send + 'static,
    {
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();

        let description_owned = description.to_string();
        let thread = std::thread::Builder::new()
            .name("micbridge-pipewire".into())
            .spawn(move || {
                if let Err(e) = run_loop(&description_owned, rate, fill, quit_rx, &ready_tx) {
                    // Jeśli pętla padła już po starcie, ta wysyłka przepadnie —
                    // odbiorca zdążył odebrać potwierdzenie i nie słucha.
                    let _ = ready_tx.try_send(Err(e.to_string()));
                    tracing::error!(error = %e, "pętla PipeWire zakończona błędem");
                }
            })
            .map_err(|e| anyhow!("nie mogę uruchomić wątku PipeWire: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                tracing::info!(node = NODE_NAME, rate, "wirtualne wejście utworzone");
                Ok(Self {
                    quit: Some(quit_tx),
                    thread: Some(thread),
                    description: description.to_string(),
                    rate,
                })
            }
            Ok(Err(e)) => Err(anyhow!(
                "nie mogę utworzyć wirtualnego mikrofonu w PipeWire: {e}.\n\
                 Sprawdź, czy PipeWire działa: \
                 `systemctl --user status pipewire wireplumber`.\n\
                 Na czystym PulseAudio ta ścieżka nie zadziała — wskaż ujście \
                 ręcznie przez --sink."
            )),
            // Wątek zakończył się bez słowa: kanał zamknięty przed potwierdzeniem.
            Err(_) => Err(anyhow!(
                "wątek PipeWire zakończył się, zanim strumień się połączył"
            )),
        }
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
}

impl Drop for VirtualSource {
    fn drop(&mut self) {
        // Węzeł ma zniknąć z listy mikrofonów razem z nami — dlatego czekamy
        // na wątek, zamiast go porzucić.
        if let Some(quit) = self.quit.take() {
            let _ = quit.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_loop<F>(
    description: &str,
    rate: u32,
    mut fill: F,
    quit_rx: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<()>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("nie mogę utworzyć pętli PipeWire: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("nie mogę utworzyć kontekstu PipeWire: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("nie mogę połączyć się z serwerem PipeWire: {e}"))?;

    let _quit = quit_rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });

    let stream = pw::stream::StreamBox::new(
        &core,
        NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            // To jedno pole robi z nas mikrofon, a nie odtwarzacz.
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => description,
            *pw::keys::NODE_VIRTUAL => "true",
            *pw::keys::AUDIO_CHANNELS => "1",
            // Prośba o kwant równy naszej ramce. Graf może dać większy, jeśli
            // wymusza go inny węzeł — dlatego strona odbiorcza i tak mierzy,
            // ile naprawdę dostaje — ale bez tego PipeWire potrafi wybrać
            // kwant rzędu stu milisekund i sam dołożyć tyle opóźnienia.
            *pw::keys::NODE_LATENCY => "480/48000",
            *pw::keys::NODE_RATE => "1/48000",
        },
    )
    .map_err(|e| anyhow!("nie mogę utworzyć strumienia: {e}"))?;

    // Bufor roboczy trzymany przez callback: rośnie najwyżej raz, przy
    // pierwszym większym kwancie. W ścieżce czasu rzeczywistego nie alokujemy.
    let mut scratch = vec![0f32; 4096];

    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(|_, _, old, new| {
            tracing::debug!(?old, ?new, "stan strumienia PipeWire");
        })
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                // Graf nie ma dla nas bufora; następny cykl go przyniesie.
                return;
            };
            // Ile ramek graf chce w tym cyklu. Musi paść przed `datas_mut`,
            // które pożycza bufor na wyłączność. Zmapowany bufor bywa dużo
            // większy od kwantu i wypełnianie go w całości znaczyłoby
            // produkowanie dźwięku, o który nikt nie prosił.
            let requested = buffer.requested() as usize;

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];

            let written = match data.data() {
                Some(slice) => {
                    let capacity = slice.len() / STRIDE;
                    let frames = if requested == 0 {
                        capacity
                    } else {
                        requested.min(capacity)
                    };
                    if scratch.len() < frames {
                        scratch.resize(frames, 0.0);
                    }
                    fill(&mut scratch[..frames]);
                    for (chunk, &sample) in slice.chunks_exact_mut(STRIDE).zip(scratch.iter()) {
                        chunk.copy_from_slice(&sample.to_le_bytes());
                    }
                    frames
                }
                None => 0,
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = STRIDE as _;
            *chunk.size_mut() = (written * STRIDE) as _;
        })
        .register()
        .map_err(|e| anyhow!("nie mogę zarejestrować nasłuchu strumienia: {e}"))?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(1);

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| anyhow!("nie mogę zserializować formatu: {e}"))?
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).ok_or_else(|| anyhow!("zły POD formatu"))?];

    stream
        .connect(
            // Piszemy do grafu; `media.class` wyżej decyduje, że graf widzi
            // w nas źródło.
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| anyhow!("nie mogę podłączyć strumienia: {e}"))?;

    // Od tego miejsca węzeł istnieje i aplikacje mogą go wybrać.
    let _ = ready.try_send(Ok(()));

    mainloop.run();
    Ok(())
}
