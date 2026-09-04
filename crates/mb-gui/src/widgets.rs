//! Rysowanie okna: sekcje, wykresy, pola.

use eframe::egui;
use mb_i18n::{t, t1, t2, Key as K};

use crate::state::{Series, Side};
use crate::{autostart, App, Target};

/// Kolor wykresu opóźnienia i strat. Straty na czerwono, bo to jedyna rzecz,
/// która naprawdę psuje dźwięk.
const LATENCY: egui::Color32 = egui::Color32::from_rgb(90, 160, 220);
const LOSS: egui::Color32 = egui::Color32::from_rgb(220, 110, 90);

/// Wysokość jednego wykresu.
const PLOT_HEIGHT: f32 = 54.0;

impl App {
    /// Parowanie: kod do pokazania albo pole na kod do wpisania.
    ///
    /// Idzie na samą górę i tylko wtedy, gdy coś się dzieje — to jedyny moment
    /// w całym programie, w którym użytkownik musi cokolwiek zrobić.
    pub(crate) fn pairing_ui(&mut self, ui: &mut egui::Ui) {
        let shown = self.state.shared.lock().ok().and_then(|s| {
            s.shown_code
                .as_ref()
                .map(|c| (c.peer.clone(), c.code.clone()))
        });

        if let Some((peer, code)) = shown {
            banner(ui, egui::Color32::from_rgb(60, 90, 60), |ui| {
                ui.label(t1(K::PairWants, peer));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(mb_net::pair::format_code(&code))
                        .size(30.0)
                        .monospace()
                        .strong(),
                );
                ui.add_space(4.0);
                if ui.button(t(K::Hide)).clicked() {
                    if let Ok(mut s) = self.state.shared.lock() {
                        s.shown_code = None;
                    }
                }
            });
        }

        if let Some(peer) = self.state.awaiting_code() {
            banner(ui, egui::Color32::from_rgb(60, 70, 95), |ui| {
                ui.label(t1(K::PairShows, peer));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.code)
                            .desired_width(120.0)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("482 193"),
                    );
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let digits = mb_net::pair::normalize_code(&self.code).len();
                    let complete = digits == mb_net::pair::CODE_DIGITS;

                    if (ui
                        .add_enabled(complete, egui::Button::new(t(K::PairButton)))
                        .clicked()
                        || (entered && complete))
                        && complete
                    {
                        self.state.answer_code(&self.code);
                        self.code.clear();
                    }
                    if ui.button(t(K::CancelButton)).clicked() {
                        self.state.cancel_code();
                        self.code.clear();
                    }
                    if digits > 0 && !complete {
                        ui.label(
                            egui::RichText::new(t2(K::DigitsOf, digits, mb_net::pair::CODE_DIGITS))
                                .weak(),
                        );
                    }
                });
            });
        }
    }

    pub(crate) fn recv_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut wanted = self.recv.is_running();
        section(ui, t(K::Receiving), t(K::ReceivingHint), |ui| {
            let text = label(wanted);
            if ui.toggle_value(&mut wanted, text).clicked() {
                if wanted {
                    self.start_recv(ctx);
                } else {
                    self.recv.stop();
                }
            }
        });

        let busy = self.recv.is_running();
        ui.add_enabled_ui(!busy, |ui| {
            // Zawijane, bo w wąskim oknie ostatnie pole inaczej wychodzi poza
            // krawędź i nie ma jak go dosięgnąć.
            ui.horizontal_wrapped(|ui| {
                ui.label(t(K::SinkLabel));
                sink_picker(ui, &mut self.sink, &self.sinks);
                ui.add_space(10.0);
                ui.label(t(K::BufferLabel));
                ui.add(
                    egui::DragValue::new(&mut self.buffer_ms)
                        .range(10..=200)
                        .suffix(" ms"),
                );
                ui.checkbox(&mut self.adaptive, t(K::AdaptBuffer))
                    .on_hover_text(t(K::AdaptBufferHint));
                ui.checkbox(&mut self.announce, t(K::Announce))
                    .on_hover_text(t(K::AnnounceHint));
            });
        });
        if busy {
            ui.label(
                egui::RichText::new(t(K::SettingsAfterStop))
                    .weak()
                    .size(11.0),
            );
        }

        let Ok(shared) = self.state.shared.lock() else {
            return;
        };
        status(ui, &shared.recv, "ms", "%");
    }

    pub(crate) fn send_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut wanted = self.send.is_running();
        section(ui, t(K::Sending), t(K::SendingHint), |ui| {
            let text = label(wanted);
            if ui.toggle_value(&mut wanted, text).clicked() {
                if wanted {
                    self.start_send(ctx);
                } else {
                    self.send.stop();
                }
            }
        });

        let busy = self.send.is_running();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(t(K::MicrophoneLabel));
                mic_picker(ui, &mut self.device, &self.mics);
                if ui
                    .button(t(K::Refresh))
                    .on_hover_text(t(K::RefreshHint))
                    .clicked()
                {
                    self.reload_devices();
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(t(K::ToLabel));
                self.target_picker(ui);
                if ui.button(t(K::Search)).clicked() {
                    self.refresh_peers(true);
                }
                if self.peers_pending.is_some() {
                    ui.spinner();
                }
            });
        });

        let Ok(shared) = self.state.shared.lock() else {
            return;
        };
        status(ui, &shared.send, "ms", "%");
    }

    fn target_picker(&mut self, ui: &mut egui::Ui) {
        self.refresh_peers(false);
        let current = match &self.target {
            Target::Auto => t(K::OnlyOnNetwork).to_string(),
            Target::Named(name) => name.clone(),
        };
        egui::ComboBox::from_id_salt("cel")
            .selected_text(current)
            .width(pick_width(ui))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.target, Target::Auto, t(K::OnlyOnNetwork))
                    .on_hover_text(t(K::OnlyOnNetworkHint));
                for peer in &self.peers {
                    let text = if peer.compatible() {
                        format!("{}  ({})", peer.name, peer.addr)
                    } else {
                        t1(K::OtherProtocol, &peer.name)
                    };
                    ui.add_enabled_ui(peer.compatible(), |ui| {
                        ui.selectable_value(
                            &mut self.target,
                            Target::Named(peer.name.clone()),
                            text,
                        );
                    });
                }
            });

        // Ręczny adres na wypadek routera, który nie przepuszcza multicastu.
        if let Target::Named(name) = &mut self.target {
            ui.add(
                egui::TextEdit::singleline(name)
                    .desired_width(150.0)
                    .hint_text(t(K::OrIpAddress)),
            );
        }
    }

    /// Pasek przy dolnej krawędzi: ustawienie, które dotyczy całego programu,
    /// i przypomnienie, z kim jest już umówiony.
    pub(crate) fn footer_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.checkbox(&mut self.autostart, t(K::Autostart)).changed() {
                self.autostart_error = match autostart::set(self.autostart) {
                    Ok(()) => None,
                    Err(e) => {
                        // Przełącznik ma pokazywać stan faktyczny, nie życzenie.
                        self.autostart = !self.autostart;
                        Some(format!("{e}"))
                    }
                };
            }

            if !self.paired.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(t1(K::PairedWith, self.paired.join(", ")))
                            .weak()
                            .size(11.0),
                    );
                });
            }
        });

        if let Some(e) = &self.autostart_error {
            ui.colored_label(LOSS, e);
        }
    }
}

/// Szerokość listy rozwijanej: tyle, ile jest miejsca, ale nie więcej niż
/// trzeba na najdłuższą nazwę urządzenia i nie mniej, niż da się przeczytać.
fn pick_width(ui: &egui::Ui) -> f32 {
    ui.available_width().clamp(140.0, 230.0)
}

fn label(running: bool) -> &'static str {
    if running {
        t(K::On)
    } else {
        t(K::Off)
    }
}

fn section(ui: &mut egui::Ui, title: &str, hint: &str, toggle: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).size(17.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), toggle);
    });
    ui.label(egui::RichText::new(hint).weak().size(11.0));
    ui.add_space(4.0);
}

fn banner(ui: &mut egui::Ui, tint: egui::Color32, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(tint)
        .corner_radius(6.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.vertical(body);
        });
    ui.add_space(8.0);
}

/// Stan jednej strony: z kim, jakie liczby, jak to wygląda w czasie.
fn status(ui: &mut egui::Ui, side: &Side, latency_unit: &str, loss_unit: &str) {
    if let Some(err) = &side.error {
        ui.colored_label(LOSS, err);
    }

    match &side.peer {
        Some(peer) => {
            ui.label(egui::RichText::new(peer).strong());
            ui.label(&side.detail);
        }
        None if side.running => {
            ui.label(egui::RichText::new(t(K::WaitingForPeer)).weak());
        }
        None => {}
    }

    if side.latency.points.is_empty() {
        show_log(ui, side);
        return;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        // Opóźnienie ma dolną skalę 50 ms, żeby zwykłe 30 ms nie wyglądało
        // jak katastrofa wypełniająca cały wykres.
        plot(
            ui,
            t(K::LatencyPlot),
            &side.latency,
            latency_unit,
            LATENCY,
            50.0,
            0,
        );
        plot(ui, t(K::LossPlot), &side.loss, loss_unit, LOSS, 2.0, 1);
    });

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (name, value) in &side.numbers {
            ui.label(egui::RichText::new(format!("{name} ")).weak().size(11.0));
            ui.label(egui::RichText::new(value).size(11.0).monospace());
            ui.add_space(10.0);
        }
    });

    show_log(ui, side);
}

fn show_log(ui: &mut egui::Ui, side: &Side) {
    if side.log.is_empty() {
        return;
    }
    ui.add_space(4.0);
    egui::CollapsingHeader::new(t(K::Details))
        .id_salt(format!("log{}", side.log.len()))
        .default_open(false)
        .show(ui, |ui| {
            for line in &side.log {
                ui.label(egui::RichText::new(line).size(11.0).monospace());
            }
        });
}

/// Przebieg w czasie: linia od najstarszej próbki po lewej do najnowszej po prawej.
///
/// Rysujemy sami, bo to trzydzieści linii, a każda dodatkowa biblioteka to
/// kolejna rzecz, która potrafi nie zbudować się po drugiej stronie.
fn plot(
    ui: &mut egui::Ui,
    title: &str,
    series: &Series,
    unit: &str,
    color: egui::Color32,
    floor: f32,
    id: u8,
) {
    let width = (ui.available_width() - 8.0).max(120.0) / if id == 0 { 2.0 } else { 1.0 };
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).weak().size(11.0));
            if let Some(last) = series.last() {
                ui.label(
                    egui::RichText::new(format!("{last:.1} {unit}"))
                        .color(color)
                        .size(13.0)
                        .strong(),
                );
            }
        });

        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(width, PLOT_HEIGHT), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        let top = series.ceiling(floor);
        let n = series.points.len();
        if n < 2 {
            return;
        }
        // Wykres zawsze zajmuje pełną szerokość: młodsza historia rozciąga się,
        // zamiast zbijać w lewy róg i udawać, że pomiarów jest więcej.
        let step = rect.width() / (crate::state::HISTORY.max(n) - 1) as f32;
        let base = rect.right() - step * (n - 1) as f32;

        let points: Vec<egui::Pos2> = series
            .points
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let x = base + step * i as f32;
                let y = rect.bottom() - (v / top).clamp(0.0, 1.0) * (rect.height() - 4.0) - 2.0;
                egui::pos2(x, y)
            })
            .collect();

        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5_f32, color)));
        painter.text(
            rect.left_top() + egui::vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("{top:.0}"),
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );
    });
}

fn sink_picker(ui: &mut egui::Ui, current: &mut String, sinks: &[String]) {
    egui::ComboBox::from_id_salt("ujscie")
        .selected_text(pretty_sink(current))
        .width(pick_width(ui))
        .show_ui(ui, |ui| {
            ui.selectable_value(current, "auto".into(), pretty_sink("auto"))
                .on_hover_text(t(K::SinkAutoHint));
            for name in sinks {
                ui.selectable_value(current, name.clone(), name);
            }
        });
}

fn pretty_sink(value: &str) -> String {
    match value {
        "auto" if cfg!(target_os = "linux") => t(K::SinkAutoLinux).into(),
        "auto" => t(K::SinkAutoWindows).into(),
        other => other.to_string(),
    }
}

fn mic_picker(ui: &mut egui::Ui, current: &mut String, mics: &[String]) {
    egui::ComboBox::from_id_salt("mikrofon")
        .selected_text(match current.as_str() {
            "default" => t(K::DefaultDevice).to_string(),
            "tone" => t(K::TestTone).to_string(),
            other => other.to_string(),
        })
        .width(pick_width(ui))
        .show_ui(ui, |ui| {
            ui.selectable_value(current, "default".into(), t(K::DefaultDevice));
            for name in mics {
                ui.selectable_value(current, name.clone(), name);
            }
            ui.separator();
            ui.selectable_value(current, "tone".into(), t(K::TestTone))
                .on_hover_text(t(K::TestToneHint));
        });
}
