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

use anyhow::{anyhow, Context, Result};
use pipewire as pw;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use spa::utils::Direction;

/// Nazwa węzła, po której użytkownik ma go rozpoznać. Trafia też do
/// `VIRTUAL_SINK_HINTS`, żeby druga strona umiała go znaleźć automatycznie.
pub const NODE_NAME: &str = "micbridge";

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
                    // Jeśli pętla padła po starcie, wysyłka i tak się nie uda —
                    // odbiorca zdążył już odebrać potwierdzenie.
                    let _ = ready_tx.try_send(Err(e.to_string()));
                    tracing::error!(error = %e, "pętla PipeWire zakończona błędem");
                }
            })
            .context("nie mogę uruchomić wątku PipeWire")?;

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
                 Sprawdź, czy PipeWire działa: `systemctl --user status pipewire`.\n\
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

    let mainloop = pw::main_loop::MainLoop::new(None).context("main loop")?;
    let context = pw::context::Context::new(&mainloop).context("context")?;
    let core = context.connect(None).context("połączenie z serwerem")?;

    let _quit = quit_rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });

    let stream = pw::stream::Stream::new(
        &core,
        NODE_NAME,
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            // To jedno pole robi z nas mikrofon, a nie odtwarzacz.
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => description,
            *pw::keys::NODE_VIRTUAL => "true",
            *pw::keys::AUDIO_CHANNELS => "1",
        },
    )
    .context("nie mogę utworzyć strumienia")?;

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
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };

            const STRIDE: usize = std::mem::size_of::<f32>();
            let written = match data.data() {
                Some(bytes) => {
                    let frames = bytes.len() / STRIDE;
                    // Bezpieczne, bo `bytes` jest wyrównane do f32 przez
                    // MAP_BUFFERS, a długość obcinamy do pełnych próbek.
                    let samples = unsafe {
                        std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut f32, frames)
                    };
                    fill(samples);
                    frames
                }
                None => 0,
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = STRIDE as i32;
            *chunk.size_mut() = (written * STRIDE) as u32;
        })
        .register()
        .context("nie mogę zarejestrować nasłuchu strumienia")?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(1);

    let values: Vec<u8> = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
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
            Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("nie mogę podłączyć strumienia")?;

    // Od tego miejsca węzeł istnieje i aplikacje mogą go wybrać.
    let _ = ready.try_send(Ok(()));

    mainloop.run();
    Ok(())
}
