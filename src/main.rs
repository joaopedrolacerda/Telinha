//! telinha — interface gráfica (modo escuro).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, RichText, Stroke, TextStyle, Vec2};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use telinha::capture::{self, SourceSpec};
use telinha::session::{Session, SessionConfig};

// ---------------------------------------------------------------- tema

const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
const CARD: Color32 = Color32::from_rgb(0x17, 0x1a, 0x20);
const BORDER: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
const FIELD: Color32 = Color32::from_rgb(0x0b, 0x0d, 0x10);
const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xeb);
const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa1);
const ACCENT: Color32 = Color32::from_rgb(0x63, 0x66, 0xf1);
const DANGER: Color32 = Color32::from_rgb(0xdc, 0x3a, 0x3a);
const LIVE: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
const WARN: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);

fn setup_style(ctx: &egui::Context) {
    // Fonte do Windows (Segoe UI), se existir; senão fica a padrão do egui.
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-bold", "seguisb.ttf")] {
        if let Ok(bytes) = std::fs::read(format!("C:\\Windows\\Fonts\\{file}")) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
        }
    }
    if fonts.font_data.contains_key("segoe") {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    if fonts.font_data.contains_key("segoe-bold") {
        fonts.families.insert(FontFamily::Name("bold".into()), vec!["segoe-bold".into(), "segoe".into()]);
    } else {
        fonts.families.insert(FontFamily::Name("bold".into()), fonts.families[&FontFamily::Proportional].clone());
    }
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    let radius = CornerRadius::same(8);
    for (w, fill) in [
        (&mut v.widgets.noninteractive, CARD),
        (&mut v.widgets.inactive, Color32::from_rgb(0x22, 0x26, 0x2e)),
        (&mut v.widgets.hovered, Color32::from_rgb(0x2b, 0x30, 0x3a)),
        (&mut v.widgets.active, Color32::from_rgb(0x34, 0x3a, 0x46)),
        (&mut v.widgets.open, Color32::from_rgb(0x22, 0x26, 0x2e)),
    ] {
        w.corner_radius = radius;
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x40, 0x4c));
    ctx.set_visuals_of(egui::Theme::Dark, v);

    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.slider_width = 200.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.5));
        s.text_styles.insert(TextStyle::Heading, FontId::new(26.0, FontFamily::Name("bold".into())));
    });
}

fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(FontFamily::Name("bold".into())).size(size)
}

fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(MUTED).size(13.0)
}

fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(bold(title.to_uppercase(), 12.0).color(MUTED));
            ui.add_space(4.0);
            add(ui)
        })
        .inner
}

// ---------------------------------------------------------------- opções salvas

const RESOLUTIONS: &[(u32, &str)] = &[
    (854, "480p  (854 px)"),
    (1280, "720p  (1280 px)"),
    (1600, "900p  (1600 px)"),
    (1920, "1080p (1920 px)"),
    (2560, "1440p (2560 px)"),
    (7680, "Original"),
];

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    share_window: bool,
    monitor_name: String,
    window_name: String,
    audio: bool,
    fps: u32,
    quality: u8,
    max_width: u32,
    tunnel: bool,
    auto_stop: bool,
    minutes: u32,
    port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            share_window: false,
            monitor_name: String::new(),
            window_name: String::new(),
            audio: true,
            fps: 60,
            quality: 60,
            max_width: 1280,
            tunnel: true,
            auto_stop: false,
            minutes: 60,
            port: 8080,
        }
    }
}

/// Estimativa grosseira de banda por espectador (JPEG de conteúdo de tela).
fn estimate_mbps(s: &Settings, source_w: u32) -> f32 {
    let w = s.max_width.min(source_w.max(640)) as f32;
    let pixels = w * w * 9.0 / 16.0;
    let bits_per_pixel = 0.7 * 2f32.powf((s.quality as f32 - 60.0) / 15.0);
    pixels * bits_per_pixel * s.fps as f32 / 1_000_000.0
}

// ---------------------------------------------------------------- app

enum State {
    Idle,
    Starting(mpsc::Receiver<anyhow::Result<Session>>),
    Running { session: Session, since: Instant },
}

struct App {
    rt: tokio::runtime::Runtime,
    s: Settings,
    monitors: Vec<SourceSpec>,
    windows: Vec<SourceSpec>,
    sel_monitor: usize,
    sel_window: usize,
    state: State,
    log: Arc<Mutex<VecDeque<String>>>,
    error: Option<String>,
    preview: Option<egui::TextureHandle>,
    preview_key: usize,
    preview_at: Instant,
    copied_at: Option<Instant>,
    title_bar_done: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);

        let s: Settings = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();

        let log: Arc<Mutex<VecDeque<String>>> = Default::default();
        let (sink, ctx, t0) = (log.clone(), cc.egui_ctx.clone(), Instant::now());
        telinha::log::set_sink(move |msg| {
            let t = t0.elapsed().as_secs();
            let mut l = sink.lock().unwrap();
            l.push_back(format!("{:02}:{:02}  {msg}", t / 60, t % 60));
            while l.len() > 200 {
                l.pop_front();
            }
            ctx.request_repaint();
        });

        let mut app = Self {
            rt: tokio::runtime::Runtime::new().expect("runtime tokio"),
            s,
            monitors: vec![],
            windows: vec![],
            sel_monitor: 0,
            sel_window: 0,
            state: State::Idle,
            log,
            error: None,
            preview: None,
            preview_key: 0,
            preview_at: Instant::now(),
            copied_at: None,
            title_bar_done: false,
        };
        app.refresh_sources();
        app
    }

    fn refresh_sources(&mut self) {
        self.monitors = capture::list_monitors();
        self.windows = capture::list_windows();
        let find = |list: &[SourceSpec], name: &str| list.iter().position(|x| x.name == name).unwrap_or(0);
        self.sel_monitor = find(&self.monitors, &self.s.monitor_name);
        self.sel_window = find(&self.windows, &self.s.window_name);
    }

    fn selected(&self) -> Option<&SourceSpec> {
        if self.s.share_window {
            self.windows.get(self.sel_window)
        } else {
            self.monitors.get(self.sel_monitor)
        }
    }

    fn start(&mut self) {
        let Some(source) = self.selected().cloned() else {
            self.error = Some("Escolha uma tela ou um aplicativo.".into());
            return;
        };
        self.error = None;
        let cfg = SessionConfig {
            source,
            audio: self.s.audio,
            fps: self.s.fps,
            quality: self.s.quality,
            max_width: self.s.max_width,
            port: self.s.port,
            tunnel: self.s.tunnel,
            cloudflared: "cloudflared".into(),
        };
        let (tx, rx) = mpsc::channel();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            // Se a janela fechou no meio do caminho, o envio falha e a sessão é solta (desligada).
            let _ = tx.send(Session::start(cfg, &handle));
        });
        self.state = State::Starting(rx);
    }

    fn stop(&mut self) {
        self.state = State::Idle; // soltar a sessão desliga tudo e derruba o link
        self.preview = None;
    }

    fn poll(&mut self) {
        if let State::Starting(rx) = &self.state {
            match rx.try_recv() {
                Ok(Ok(session)) => self.state = State::Running { session, since: Instant::now() },
                Ok(Err(e)) => {
                    self.error = Some(format!("{e:#}"));
                    self.state = State::Idle;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.state = State::Idle,
            }
        }
        if let State::Running { since, .. } = &self.state {
            if self.s.auto_stop && since.elapsed() >= Duration::from_secs(self.s.minutes as u64 * 60) {
                telinha::logln!("Tempo esgotado: derrubando o link.");
                self.stop();
            }
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        let State::Running { session, .. } = &self.state else { return };
        if self.preview_at.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.preview_at = Instant::now();
        let Some(frame) = session.last_frame.lock().unwrap().clone() else { return };
        let key = frame.as_ptr() as usize;
        if key == self.preview_key {
            return;
        }
        self.preview_key = key;
        let Ok(img) = image::load_from_memory_with_format(&frame[1..], image::ImageFormat::Jpeg) else { return };
        let rgba = img.to_rgba8();
        let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
        match &mut self.preview {
            Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
            None => self.preview = Some(ctx.load_texture("preview", ci, egui::TextureOptions::LINEAR)),
        }
    }

    // ------------------------------------------------------------ desenho

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(bold("telinha", 28.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, color) = match &self.state {
                    State::Idle => ("PARADO", MUTED),
                    State::Starting(_) => ("INICIANDO", WARN),
                    State::Running { .. } => ("AO VIVO", LIVE),
                };
                egui::Frame::new()
                    .fill(color.gamma_multiply(0.15))
                    .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
                    .corner_radius(20)
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.label(bold(format!("●  {text}"), 12.0).color(color));
                    });
            });
        });
        ui.label(muted("Compartilhe sua tela por um link temporário"));
    }

    fn source_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "O que compartilhar", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.s.share_window, false, "Tela inteira");
                ui.selectable_value(&mut self.s.share_window, true, "Aplicativo");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳").on_hover_text("Atualizar lista").clicked() {
                        self.refresh_sources();
                    }
                });
            });

            let (list, sel) = if self.s.share_window {
                (&self.windows, &mut self.sel_window)
            } else {
                (&self.monitors, &mut self.sel_monitor)
            };
            if list.is_empty() {
                ui.label(muted(if self.s.share_window {
                    "Nenhuma janela visível. Abra o aplicativo (não minimizado) e clique em ⟳."
                } else {
                    "Nenhuma tela encontrada."
                }));
                return;
            }
            *sel = (*sel).min(list.len() - 1);
            egui::ComboBox::from_id_salt(("fonte", self.s.share_window))
                .width(ui.available_width())
                .truncate()
                .selected_text(&list[*sel].name)
                .show_ui(ui, |ui| {
                    for (i, item) in list.iter().enumerate() {
                        ui.selectable_value(sel, i, &item.name);
                    }
                });
            let name = list[*sel].name.clone();
            if self.s.share_window {
                self.s.window_name = name;
            } else {
                self.s.monitor_name = name;
            }
        });
    }

    fn audio_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Som", |ui| {
            ui.checkbox(&mut self.s.audio, "Transmitir o som do PC");
            ui.label(muted("Tudo que toca nas caixas/fone. Quem assiste clica em “Ativar som”."));
        });
    }

    fn video_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Vídeo", |ui| {
            egui::Grid::new("video").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label("Quadros/s");
                ui.horizontal(|ui| {
                    for fps in [15, 30, 60] {
                        ui.selectable_value(&mut self.s.fps, fps, format!("{fps}"));
                    }
                });
                ui.end_row();

                ui.label("Resolução");
                let current = RESOLUTIONS
                    .iter()
                    .find(|(w, _)| *w == self.s.max_width)
                    .map(|(_, l)| *l)
                    .unwrap_or("Personalizada");
                egui::ComboBox::from_id_salt("res").selected_text(current).width(180.0).show_ui(ui, |ui| {
                    for (w, label) in RESOLUTIONS {
                        ui.selectable_value(&mut self.s.max_width, *w, *label);
                    }
                });
                ui.end_row();

                ui.label("Qualidade");
                ui.add(egui::Slider::new(&mut self.s.quality, 20..=95).suffix("%"));
                ui.end_row();
            });

            let source_w = self.selected().map(|s| s.width).unwrap_or(1920);
            let mbps = estimate_mbps(&self.s, source_w);
            let color = if mbps > 40.0 { WARN } else { MUTED };
            ui.label(
                RichText::new(format!("≈ {mbps:.0} Mbit/s por espectador (estimativa, sai do seu upload)"))
                    .color(color)
                    .size(13.0),
            );
        });
    }

    fn link_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Link", |ui| {
            ui.checkbox(&mut self.s.tunnel, "Link público pelo Cloudflare");
            ui.label(muted(if self.s.tunnel {
                "Gera um endereço https://….trycloudflare.com que some quando você parar."
            } else {
                "Só neste PC (http://127.0.0.1), útil para testar."
            }));
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.s.auto_stop, "Derrubar sozinho após");
                ui.add_enabled(
                    self.s.auto_stop,
                    egui::DragValue::new(&mut self.s.minutes).range(1..=600).suffix(" min"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Porta local");
                ui.add(egui::DragValue::new(&mut self.s.port).range(1024..=65535));
            });
        });
    }

    fn live_card(&mut self, ui: &mut egui::Ui) {
        let State::Running { session, since } = &self.state else { return };
        let (url, viewers, elapsed) = (session.share_url().to_string(), session.viewers.load(Ordering::Relaxed), since.elapsed());
        let is_public = session.public_url.is_some();
        let mut stop = false;

        egui::Frame::new()
            .fill(Color32::from_rgb(0x12, 0x1c, 0x17))
            .stroke(Stroke::new(1.0, LIVE.gamma_multiply(0.45)))
            .corner_radius(12)
            .inner_margin(16)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(bold("● AO VIVO", 14.0).color(LIVE));
                    ui.label(muted(format!(
                        "{}  ·  {} assistindo  ·  {:02}:{:02}",
                        session.source_name,
                        viewers,
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60
                    )));
                });
                if self.s.auto_stop {
                    let left = (self.s.minutes as u64 * 60).saturating_sub(elapsed.as_secs());
                    ui.label(muted(format!("O link cai em {:02}:{:02}", left / 60, left % 60)));
                }

                ui.add_space(4.0);
                ui.label(muted(if is_public { "Link para compartilhar" } else { "Link local (só neste PC)" }));
                egui::Frame::new().fill(FIELD).corner_radius(8).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(&url).monospace().color(TEXT)).wrap().selectable(true));
                });

                ui.horizontal(|ui| {
                    let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
                    let label = if copied { "Copiado!" } else { "Copiar link" };
                    if ui.add(egui::Button::new(bold(label, 14.0)).fill(ACCENT).min_size(Vec2::new(140.0, 32.0))).clicked() {
                        ui.ctx().copy_text(url.clone());
                        self.copied_at = Some(Instant::now());
                    }
                    if ui.add(egui::Button::new("Abrir no navegador").min_size(Vec2::new(0.0, 32.0))).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                    }
                });

                if let Some(tex) = &self.preview {
                    ui.add_space(6.0);
                    ui.label(muted("Prévia"));
                    let size = tex.size_vec2();
                    let w = ui.available_width();
                    let h = w * size.y / size.x.max(1.0);
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                            .fit_to_exact_size(Vec2::new(w, h))
                            .corner_radius(8),
                    );
                }

                ui.add_space(6.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 42.0],
                        egui::Button::new(bold("Parar e derrubar o link", 16.0).color(Color32::WHITE)).fill(DANGER),
                    )
                    .clicked()
                {
                    stop = true;
                }
            });
        if stop {
            self.stop();
        }
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        ui.add_space(8.0);

        if let Some(err) = self.error.clone() {
            egui::Frame::new()
                .fill(DANGER.gamma_multiply(0.15))
                .stroke(Stroke::new(1.0, DANGER.gamma_multiply(0.6)))
                .corner_radius(10)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(format!("⚠  {err}")).color(TEXT)).wrap());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if ui.small_button("✕").clicked() {
                                self.error = None;
                            }
                        });
                    });
                });
        }

        self.live_card(ui);

        let idle = matches!(self.state, State::Idle);
        ui.add_enabled_ui(idle, |ui| {
            self.source_card(ui);
            self.audio_card(ui);
            self.video_card(ui);
            self.link_card(ui);
        });

        match &self.state {
            State::Idle => {
                ui.add_space(4.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 46.0],
                        egui::Button::new(bold("Iniciar compartilhamento", 17.0).color(Color32::WHITE)).fill(ACCENT),
                    )
                    .clicked()
                {
                    self.start();
                }
            }
            State::Starting(_) => {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if self.s.tunnel { "Abrindo túnel no Cloudflare…" } else { "Iniciando…" });
                });
            }
            State::Running { .. } => {}
        }

        ui.add_space(4.0);
        egui::CollapsingHeader::new(muted("Registro")).default_open(false).show(ui, |ui| {
            egui::Frame::new().fill(FIELD).corner_radius(8).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in self.log.lock().unwrap().iter() {
                        ui.label(RichText::new(line).monospace().size(12.0).color(MUTED));
                    }
                });
            });
        });
    }
}

/// Ícone da janela (o mesmo desenho embutido no .exe).
fn app_icon() -> egui::IconData {
    egui::IconData { rgba: telinha::icon::draw_icon(64), width: 64, height: 64 }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.title_bar_done {
            // Barra de título escura, mesmo com o Windows no tema claro.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            self.title_bar_done = true;
        }
        self.poll();
        self.update_preview(&ctx);
        if !matches!(self.state, State::Idle) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(18))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    self.content(ui);
                });
            });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    /// Guarda só as opções, não o estado da interface (rolagem etc.).
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("telinha")
            .with_inner_size([500.0, 900.0])
            .with_min_inner_size([420.0, 560.0])
            .with_icon(Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native("telinha", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
