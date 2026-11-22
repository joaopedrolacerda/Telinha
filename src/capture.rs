//! Captura de tela/janela com a Windows Graphics Capture (cópia feita pela GPU,
//! quadros no ritmo do monitor, com cursor) e um pool de threads que reduz e
//! comprime em JPEG em paralelo, para sustentar 60 fps.
use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use dialoguer::{theme::ColorfulTheme, FuzzySelect, Select};
use fast_image_resize::{images::Image, images::ImageRef, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use crate::logln;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

/// Tag do pacote de vídeo no WebSocket.
pub const TAG_VIDEO: u8 = 1;

/// Último quadro comprimido: enviado na hora para quem acabou de entrar
/// (a captura só gera quadros quando a imagem muda).
pub type FrameCache = Arc<Mutex<Option<Bytes>>>;

#[derive(Clone, Debug)]
pub enum Kind {
    Monitor,
    Window,
}

/// O que capturar. Guardamos o HMONITOR/HWND como número (`Send`).
#[derive(Clone, Debug)]
pub struct SourceSpec {
    pub kind: Kind,
    pub handle: usize,
    pub name: String,
    /// Largura em pixels (para estimativas na interface).
    pub width: u32,
    /// Processo dono da janela (para capturar só o som desse aplicativo).
    pub pid: Option<u32>,
}

#[derive(Clone)]
pub struct Config {
    pub fps: u32,
    pub quality: u8,
    pub max_width: u32,
}

// ---------------------------------------------------------------- seleção

fn monitor_label(i: usize, m: &Monitor) -> String {
    let primary = Monitor::primary()
        .map(|p| p.as_raw_hmonitor() == m.as_raw_hmonitor())
        .unwrap_or(false);
    format!(
        "Tela {} — {} {}x{} @{}Hz{}",
        i + 1,
        m.name().unwrap_or_default(),
        m.width().unwrap_or(0),
        m.height().unwrap_or(0),
        m.refresh_rate().unwrap_or(0),
        if primary { " (principal)" } else { "" }
    )
}

fn window_label(w: &Window) -> String {
    format!("[{}] {}", w.process_name().unwrap_or_default(), w.title().unwrap_or_default())
}

fn visible_windows() -> Result<Vec<Window>> {
    let me = std::process::id();
    Ok(Window::enumerate()?
        .into_iter()
        .filter(|w| {
            !w.title().unwrap_or_default().trim().is_empty()
                && w.width().unwrap_or(0) > 50
                && w.height().unwrap_or(0) > 50
                && w.process_id().map(|p| p != me).unwrap_or(true) // não lista a própria janela
        })
        .collect())
}

/// Telas disponíveis (para a interface).
pub fn list_monitors() -> Vec<SourceSpec> {
    Monitor::enumerate()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| monitor_spec(i, m))
        .collect()
}

/// Janelas visíveis (para a interface).
pub fn list_windows() -> Vec<SourceSpec> {
    let mut v: Vec<SourceSpec> = visible_windows().unwrap_or_default().iter().map(window_spec).collect();
    v.sort_by_key(|s| s.name.to_lowercase());
    v
}

fn monitor_spec(i: usize, m: &Monitor) -> SourceSpec {
    SourceSpec {
        kind: Kind::Monitor,
        handle: m.as_raw_hmonitor() as usize,
        name: monitor_label(i, m),
        width: m.width().unwrap_or(1920),
        pid: None,
    }
}

fn window_spec(w: &Window) -> SourceSpec {
    SourceSpec {
        kind: Kind::Window,
        handle: w.as_raw_hwnd() as usize,
        name: window_label(w),
        width: w.width().unwrap_or(1280).max(0) as u32,
        pid: w.process_id().ok(),
    }
}

/// Escolha sem menu: `--screen N` (1 = primeira tela) ou `--app "<trecho do nome/título>"`.
pub fn source_from_args(screen: Option<usize>, app: Option<&str>) -> Result<Option<SourceSpec>> {
    if let Some(n) = screen {
        let monitors = Monitor::enumerate()?;
        let Some(m) = n.checked_sub(1).and_then(|i| monitors.get(i)) else {
            bail!("tela {n} não existe (há {} tela(s))", monitors.len());
        };
        return Ok(Some(monitor_spec(n - 1, m)));
    }
    if let Some(filter) = app {
        let needle = filter.to_lowercase();
        let w = visible_windows()?
            .into_iter()
            .find(|w| window_label(w).to_lowercase().contains(&needle));
        let Some(w) = w else { bail!("nenhuma janela visível contém \"{filter}\"") };
        return Ok(Some(window_spec(&w)));
    }
    Ok(None)
}

pub fn choose_source() -> Result<SourceSpec> {
    let theme = ColorfulTheme::default();
    let kind = Select::with_theme(&theme)
        .with_prompt("O que você quer compartilhar?")
        .items(&["Uma tela (monitor inteiro)", "Um aplicativo (janela)"])
        .default(0)
        .interact()?;

    if kind == 0 {
        let monitors = Monitor::enumerate()?;
        if monitors.is_empty() {
            bail!("nenhum monitor encontrado");
        }
        let labels: Vec<String> = monitors.iter().enumerate().map(|(i, m)| monitor_label(i, m)).collect();
        let idx = Select::with_theme(&theme)
            .with_prompt("Qual tela?")
            .items(&labels)
            .default(0)
            .interact()?;
        Ok(monitor_spec(idx, &monitors[idx]))
    } else {
        let windows = visible_windows()?;
        if windows.is_empty() {
            bail!("nenhuma janela visível encontrada");
        }
        let labels: Vec<String> = windows.iter().map(window_label).collect();
        let idx = FuzzySelect::with_theme(&theme)
            .with_prompt("Qual aplicativo? (digite para filtrar)")
            .items(&labels)
            .default(0)
            .interact()?;
        Ok(window_spec(&windows[idx]))
    }
}

// ---------------------------------------------------------------- captura

/// Quadro cru (RGBA, sem padding) a caminho dos threads de compressão.
struct RawFrame {
    seq: u64,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

struct Flags {
    raw_tx: Sender<RawFrame>,
    pool_rx: Receiver<Vec<u8>>,
    video: broadcast::Sender<Bytes>,
    frame_time: Duration,
}

struct Handler {
    flags: Flags,
    seq: u64,
    last: Instant,
    stats: Stats,
}

/// Contadores por segundo, impressos com TELINHA_DEBUG=1.
#[derive(Default)]
struct Stats {
    enabled: bool,
    since: Option<Instant>,
    arrived: u32,
    queued: u32,
    dropped: u32,
    copy_ms: f64,
}

impl Stats {
    fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(1) {
            logln!(
                "[debug] quadros do Windows: {}/s  enviados p/ compressão: {}/s  descartados (fila cheia): {}/s  cópia GPU→CPU média: {:.1}ms",
                self.arrived,
                self.queued,
                self.dropped,
                self.copy_ms / self.queued.max(1) as f64
            );
            *self = Stats { enabled: true, since: Some(Instant::now()), ..Default::default() };
        }
    }
}

type HandlerError = Box<dyn std::error::Error + Send + Sync>;

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = HandlerError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let stats = Stats { enabled: std::env::var_os("TELINHA_DEBUG").is_some(), ..Default::default() };
        Ok(Self { flags: ctx.flags, seq: 0, last: Instant::now(), stats })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _: InternalCaptureControl) -> Result<(), Self::Error> {
        self.stats.arrived += 1;
        self.stats.tick();
        // Agendamento por horário-alvo: aceita o quadro se já passou (ou quase) da hora do
        // próximo. Assim, num monitor de 120/144 Hz pegamos ~1 a cada N sem perder o ritmo.
        // Sem espectadores, só 1 quadro/s (para manter o cache do "último quadro" atualizado).
        let now = Instant::now();
        let interval = if self.flags.video.receiver_count() == 0 {
            Duration::from_secs(1)
        } else {
            self.flags.frame_time
        };
        let slack = interval / 4;
        // Alguém acabou de entrar: não espera o agendamento "lento" de 1 quadro/s.
        if self.last > now + interval {
            self.last = now;
        }
        if now + slack < self.last {
            return Ok(());
        }

        let t_copy = Instant::now();
        let mut fb = frame.buffer()?;
        let (w, h, pitch) = (fb.width(), fb.height(), fb.row_pitch() as usize);
        let row = w as usize * 4;
        let mut data = self.flags.pool_rx.try_recv().unwrap_or_default();
        data.clear();
        data.reserve(row * h as usize);
        let raw = fb.as_raw_buffer();
        if pitch == row {
            data.extend_from_slice(&raw[..row * h as usize]);
        } else {
            for y in 0..h as usize {
                data.extend_from_slice(&raw[y * pitch..y * pitch + row]);
            }
        }

        self.stats.copy_ms += t_copy.elapsed().as_secs_f64() * 1e3;

        self.seq += 1;
        match self.flags.raw_tx.try_send(RawFrame { seq: self.seq, width: w, height: h, data }) {
            Ok(()) => {
                // `last` = horário-alvo do próximo quadro. Se atrasou muito, não tenta "compensar" em rajada.
                self.last = (self.last + interval).max(now + interval - slack);
                self.stats.queued += 1;
            }
            // Todos os threads ocupados: descarta este quadro.
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => self.stats.dropped += 1,
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        logln!("A janela/tela compartilhada foi fechada.");
        Ok(())
    }
}

pub fn spawn(spec: SourceSpec, cfg: Config, video: broadcast::Sender<Bytes>, cache: FrameCache) -> Result<CaptureHandle> {
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 4);
    let (raw_tx, raw_rx) = bounded::<RawFrame>(workers);
    let (pool_tx, pool_rx) = bounded::<Vec<u8>>(workers * 2);
    let video_out = video.clone();
    let (done_tx, done_rx) = bounded::<(u64, Option<Bytes>)>(workers * 4);

    for _ in 0..workers {
        let (raw_rx, pool_tx, done_tx, cfg) = (raw_rx.clone(), pool_tx.clone(), done_tx.clone(), cfg.clone());
        thread::spawn(move || {
            let mut resizer = Resizer::new();
            let mut out = Vec::new();
            for frame in raw_rx {
                let packet = match encode(&frame, &cfg, &mut resizer, &mut out) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        logln!("Falha ao comprimir quadro: {e:#}");
                        None
                    }
                };
                let _ = pool_tx.try_send(frame.data);
                let _ = done_tx.send((frame.seq, packet));
            }
        });
    }
    drop(done_tx);

    // Sequenciador: os threads terminam fora de ordem; publica os quadros na ordem de captura.
    thread::spawn(move || {
        let mut next = 1u64;
        let mut pending: BTreeMap<u64, Option<Bytes>> = BTreeMap::new();
        for (seq, packet) in done_rx {
            pending.insert(seq, packet);
            // Segurança: se um número sumiu, pula para o menor disponível.
            if pending.len() > workers * 2 {
                next = *pending.keys().next().unwrap();
            }
            while let Some(p) = pending.remove(&next) {
                if let Some(p) = p {
                    *cache.lock().unwrap() = Some(p.clone());
                    let _ = video_out.send(p);
                }
                next += 1;
            }
        }
    });

    let flags = Flags { raw_tx, pool_rx, video, frame_time: Duration::from_secs_f64(1.0 / cfg.fps as f64) };
    let control = match spec.kind {
        Kind::Monitor => {
            let item = Monitor::from_raw_hmonitor(spec.handle as *mut std::ffi::c_void);
            Handler::start_free_threaded(settings(item, flags))
        }
        Kind::Window => {
            let item = Window::from_raw_hwnd(spec.handle as *mut std::ffi::c_void);
            Handler::start_free_threaded(settings(item, flags))
        }
    }
    .map_err(|e| anyhow!("não consegui iniciar a captura: {e}"))?;
    Ok(CaptureHandle { control: Some(control) })
}

/// Controle da captura em andamento. `stop()` (ou soltar) encerra a captura e os threads.
pub struct CaptureHandle {
    control: Option<CaptureControl<Handler, HandlerError>>,
}

impl CaptureHandle {
    pub fn stop(mut self) {
        self.halt();
    }
    fn halt(&mut self) {
        if let Some(c) = self.control.take() {
            let _ = c.stop();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.halt();
    }
}

fn settings<T>(item: T, flags: Flags) -> Settings<Flags, T>
where
    T: TryInto<windows_capture::settings::GraphicsCaptureItemType>,
{
    Settings::new(
        item,
        CursorCaptureSettings::WithCursor,
        // "Default" funciona no Windows 10; tirar a borda amarela só é possível no 11.
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        flags,
    )
}

fn encode(frame: &RawFrame, cfg: &Config, resizer: &mut Resizer, out: &mut Vec<u8>) -> Result<Bytes> {
    let (sw, sh) = (frame.width, frame.height);
    if sw == 0 || sh == 0 {
        bail!("quadro vazio");
    }
    let (dw, dh) = if sw > cfg.max_width {
        (cfg.max_width, ((sh as u64 * cfg.max_width as u64 / sw as u64) as u32).max(1))
    } else {
        (sw, sh)
    };
    if dw > u16::MAX as u32 || dh > u16::MAX as u32 {
        bail!("tamanho de imagem inválido {dw}x{dh}");
    }

    let pixels: &[u8] = if (dw, dh) != (sw, sh) {
        let src = ImageRef::new(sw, sh, &frame.data, PixelType::U8x4)?;
        let mut dst = Image::from_vec_u8(dw, dh, std::mem::take(out), PixelType::U8x4)
            .unwrap_or_else(|_| Image::new(dw, dh, PixelType::U8x4));
        resizer.resize(
            &src,
            &mut dst,
            &ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                .use_alpha(false),
        )?;
        *out = dst.into_vec();
        out
    } else {
        &frame.data
    };

    let mut buf = Vec::with_capacity((dw * dh / 4) as usize);
    buf.push(TAG_VIDEO);
    let encoder = jpeg_encoder::Encoder::new(&mut buf, cfg.quality);
    encoder.encode(pixels, dw as u16, dh as u16, jpeg_encoder::ColorType::Rgba)?;
    Ok(Bytes::from(buf))
}
