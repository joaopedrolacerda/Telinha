//! Captura de som para a transmissão.
//!
//! Dois modos:
//! - **PC inteiro**: loopback WASAPI da saída padrão (tudo que toca nas caixas).
//! - **Aplicativos**: loopback por processo (Windows 10 2004+), um por aplicativo
//!   escolhido, misturados aqui.
//!
//! Em ambos, o volume/mudo vêm de um [`AudioControl`] que a interface ajusta ao vivo.
//! Pacotes enviados: [TAG_AUDIO, PCM i16 little-endian intercalado...], ~20ms cada.
use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::logln;

/// Tag do pacote de áudio no WebSocket.
pub const TAG_AUDIO: u8 = 2;

/// Formato usado na captura por aplicativo (o Windows converte para nós).
const APP_RATE: u32 = 48_000;
const APP_CHANNELS: usize = 2;

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
}

/// De onde vem o som.
#[derive(Clone, Debug)]
pub enum AudioSource {
    /// Tudo que toca na saída padrão do PC.
    System,
    /// Só estes processos. `include_tree` inclui os processos-filhos
    /// (ex.: navegadores tocam som num subprocesso).
    Processes { pids: Vec<u32>, include_tree: bool },
}

// ---------------------------------------------------------------- controle

/// Volume, mudo e medidor de nível, compartilhados entre a captura e a interface.
#[derive(Debug)]
pub struct AudioControl {
    gain: AtomicU32,
    muted: AtomicBool,
    peak: AtomicU32,
}

impl AudioControl {
    pub fn new(gain: f32) -> Arc<Self> {
        Arc::new(Self { gain: AtomicU32::new(gain.to_bits()), muted: AtomicBool::new(false), peak: AtomicU32::new(0) })
    }
    /// 1.0 = volume original; até 2.0 (200%).
    pub fn set_gain(&self, g: f32) {
        self.gain.store(g.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    /// Pico recente (0..1) do som que está saindo, já com volume aplicado.
    /// Lido pela interface; decai sozinho a cada leitura.
    pub fn take_peak(&self) -> f32 {
        let p = f32::from_bits(self.peak.load(Ordering::Relaxed));
        self.peak.store((p * 0.85).to_bits(), Ordering::Relaxed);
        p
    }
    fn report_peak(&self, p: f32) {
        let old = f32::from_bits(self.peak.load(Ordering::Relaxed));
        if p > old {
            self.peak.store(p.to_bits(), Ordering::Relaxed);
        }
    }
    fn effective_gain(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            self.gain()
        }
    }
}

/// Aplica o volume, mede o pico e converte para i16 LE dentro de `out`.
fn finish_samples(samples: impl Iterator<Item = f32>, control: &AudioControl, out: &mut Vec<u8>) {
    let g = control.effective_gain();
    let mut peak = 0f32;
    for s in samples {
        let v = (s * g).clamp(-1.0, 1.0);
        peak = peak.max(v.abs());
        out.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
    }
    control.report_peak(peak);
}

// ---------------------------------------------------------------- início/parada

/// Enquanto existir, o som continua sendo capturado. Soltar (`drop`) para parar.
pub struct AudioHandle {
    stop: Arc<AtomicBool>,
    _stop_tx: Option<mpsc::Sender<()>>,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(
    source: AudioSource,
    tx: broadcast::Sender<Bytes>,
    control: Arc<AudioControl>,
) -> Result<(AudioInfo, AudioHandle)> {
    match source {
        AudioSource::System => start_system(tx, control),
        AudioSource::Processes { pids, include_tree } => start_processes(pids, include_tree, tx, control),
    }
}

// ---------------------------------------------------------------- PC inteiro (cpal)

fn start_system(tx: broadcast::Sender<Bytes>, control: Arc<AudioControl>) -> Result<(AudioInfo, AudioHandle)> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<AudioInfo>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    // Thread própria: o `cpal::Stream` não é `Send`.
    thread::spawn(move || match build_system_stream(tx, control) {
        Ok((stream, info)) => {
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(anyhow!(e)));
                return;
            }
            let _ = ready_tx.send(Ok(info));
            let _ = stop_rx.recv(); // até o AudioHandle ser solto
            drop(stream);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    });

    let info = ready_rx.recv()??;
    Ok((info, AudioHandle { stop: Arc::new(AtomicBool::new(false)), _stop_tx: Some(stop_tx) }))
}

fn build_system_stream(tx: broadcast::Sender<Bytes>, control: Arc<AudioControl>) -> Result<(cpal::Stream, AudioInfo)> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow!("nenhum dispositivo de saída de som"))?;
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let in_channels = config.channels as usize;
    let out_channels = in_channels.min(2);
    let info = AudioInfo { sample_rate: config.sample_rate.0, channels: out_channels as u16 };

    // No WASAPI, abrir um stream de *entrada* num dispositivo de *saída* = loopback.
    let args = (in_channels, out_channels, info.sample_rate, tx, control);
    let stream = match format {
        SampleFormat::F32 => system_input::<f32>(&device, &config, args)?,
        SampleFormat::I16 => system_input::<i16>(&device, &config, args)?,
        SampleFormat::U16 => system_input::<u16>(&device, &config, args)?,
        SampleFormat::I32 => system_input::<i32>(&device, &config, args)?,
        other => bail!("formato de áudio não suportado: {other:?}"),
    };
    Ok((stream, info))
}

fn system_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    (in_channels, out_channels, sample_rate, tx, control): (usize, usize, u32, broadcast::Sender<Bytes>, Arc<AudioControl>),
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let chunk_samples = (sample_rate as usize / 50) * out_channels; // 20ms
    let mut pending: Vec<f32> = Vec::with_capacity(chunk_samples * 2);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if tx.receiver_count() == 0 {
                pending.clear();
                return;
            }
            for frame in data.chunks(in_channels) {
                for &s in frame.iter().take(out_channels) {
                    pending.push(<f32 as cpal::FromSample<T>>::from_sample_(s));
                }
            }
            while pending.len() >= chunk_samples {
                let mut packet = Vec::with_capacity(1 + chunk_samples * 2);
                packet.push(TAG_AUDIO);
                finish_samples(pending.drain(..chunk_samples), &control, &mut packet);
                let _ = tx.send(Bytes::from(packet));
            }
        },
        |err| logln!("Áudio: {err}"),
        None,
    )?;
    Ok(stream)
}

// ---------------------------------------------------------------- por aplicativo (wasapi)

type SampleQueue = Arc<Mutex<VecDeque<f32>>>;

fn start_processes(
    pids: Vec<u32>,
    include_tree: bool,
    tx: broadcast::Sender<Bytes>,
    control: Arc<AudioControl>,
) -> Result<(AudioInfo, AudioHandle)> {
    if pids.is_empty() {
        bail!("nenhum aplicativo escolhido para o som");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut queues = Vec::new();
    let mut errors = Vec::new();

    for pid in pids {
        let queue: SampleQueue = Default::default();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let (q, s) = (queue.clone(), stop.clone());
        thread::spawn(move || capture_process(pid, include_tree, q, s, ready_tx));
        match ready_rx.recv().unwrap_or_else(|_| Err(anyhow!("a captura terminou"))) {
            Ok(()) => queues.push(queue),
            Err(e) => errors.push(format!("{} (pid {pid}): {e:#}", process_name(pid).unwrap_or_default())),
        }
    }
    for e in &errors {
        logln!("Som de aplicativo indisponível: {e}");
    }
    if queues.is_empty() {
        bail!("não consegui capturar o som de nenhum aplicativo escolhido ({})", errors.join("; "));
    }
    logln!("Capturando o som de {} processo(s)", queues.len());

    let s = stop.clone();
    thread::spawn(move || mix_loop(queues, tx, control, s));

    Ok((
        AudioInfo { sample_rate: APP_RATE, channels: APP_CHANNELS as u16 },
        AudioHandle { stop, _stop_tx: None },
    ))
}

/// Uma thread por processo: lê o som dele e põe na fila (f32 estéreo 48 kHz).
fn capture_process(pid: u32, include_tree: bool, queue: SampleQueue, stop: Arc<AtomicBool>, ready: mpsc::Sender<Result<()>>) {
    use wasapi::*;
    let _ = initialize_mta();

    let setup = || -> Result<(AudioClient, AudioCaptureClient, Handle)> {
        let format = WaveFormat::new(32, 32, SampleType::Float, APP_RATE as usize, APP_CHANNELS, None);
        let mut client = AudioClient::new_application_loopback_client(pid, include_tree)?;
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
        client.initialize_client(&format, Direction::Capture, mode, None)?;
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        client.start_stream()?;
        Ok((client, capture, event))
    };

    let (client, capture, event) = match setup() {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut bytes: VecDeque<u8> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        // Sem som tocando, pode não haver evento: o timeout só serve para checar o `stop`.
        let _ = event.wait_for_event(100);
        loop {
            match capture.get_next_packet_size() {
                Ok(Some(n)) if n > 0 => {
                    if capture.read_from_device_to_deque(&mut bytes).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        if bytes.len() >= 4 {
            let mut q = queue.lock().unwrap();
            while bytes.len() >= 4 {
                let b = [bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap()];
                q.push_back(f32::from_le_bytes(b));
            }
        }
    }
    let _ = client.stop_stream();
}

/// Mistura as filas a cada 20ms, aplica volume e envia.
fn mix_loop(queues: Vec<SampleQueue>, tx: broadcast::Sender<Bytes>, control: Arc<AudioControl>, stop: Arc<AtomicBool>) {
    const CHUNK: usize = (APP_RATE as usize / 50) * APP_CHANNELS; // 20ms
    const PRIME: usize = CHUNK * 2; // espera 40ms acumulados antes de tocar (evita estalos)
    const MAX: usize = CHUNK * 8; // mais de 160ms atrasado: descarta o excesso

    let mut primed = vec![false; queues.len()];
    let mut mix = vec![0f32; CHUNK];
    let mut next = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        next += Duration::from_millis(20);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        } else if Instant::now() - next > Duration::from_millis(200) {
            next = Instant::now(); // atrasou demais (PC travou?): recomeça o relógio
        }

        mix.iter_mut().for_each(|s| *s = 0.0);
        for (i, q) in queues.iter().enumerate() {
            let mut q = q.lock().unwrap();
            if q.len() > MAX {
                let excess = q.len() - PRIME;
                q.drain(..excess - excess % APP_CHANNELS);
            }
            if !primed[i] {
                if q.len() < PRIME {
                    continue;
                }
                primed[i] = true;
            }
            if q.len() < CHUNK {
                primed[i] = false; // acabou o som (ou atrasou): espera encher de novo
            }
            let take = CHUNK.min(q.len());
            for (m, s) in mix.iter_mut().zip(q.drain(..take)) {
                *m += s;
            }
        }

        if tx.receiver_count() == 0 {
            continue;
        }
        let mut packet = Vec::with_capacity(1 + CHUNK * 2);
        packet.push(TAG_AUDIO);
        finish_samples(mix.iter().copied(), &control, &mut packet);
        let _ = tx.send(Bytes::from(packet));
    }
}

// ---------------------------------------------------------------- lista de aplicativos

/// Um aplicativo com sessão de áudio aberta (aparece no Mixer de Volume do Windows).
#[derive(Clone, Debug)]
pub struct AudioApp {
    /// Nome do executável, ex.: "chrome.exe". Vários processos podem ter o mesmo nome.
    pub name: String,
    pub pids: Vec<u32>,
    /// Tocando agora (sessão ativa).
    pub playing: bool,
}

/// Aplicativos que abriram áudio em algum dispositivo de saída, agrupados pelo nome.
pub fn list_audio_apps() -> Vec<AudioApp> {
    // COM precisa estar inicializado na thread; fazemos numa thread separada
    // para não interferir na thread da interface.
    thread::spawn(|| list_audio_apps_inner().unwrap_or_default()).join().unwrap_or_default()
}

fn list_audio_apps_inner() -> Result<Vec<AudioApp>> {
    use wasapi::*;
    let _ = initialize_mta();
    let me = std::process::id();
    let mut apps: Vec<AudioApp> = Vec::new();

    let enumerator = DeviceEnumerator::new()?;
    for device in &enumerator.get_device_collection(Direction::Render)? {
        let Ok(device) = device else { continue };
        let Ok(manager) = device.get_iaudiosessionmanager() else { continue };
        let Ok(sessions) = manager.get_audiosessionenumerator() else { continue };
        for i in 0..sessions.get_count().unwrap_or(0) {
            let Ok(session) = sessions.get_session(i) else { continue };
            let state = session.get_state().unwrap_or(SessionState::Expired);
            if matches!(state, SessionState::Expired) {
                continue;
            }
            let Ok(pid) = session.get_process_id() else { continue };
            if pid == 0 || pid == me {
                continue; // 0 = sons do sistema
            }
            let Some(name) = process_name(pid) else { continue };
            let playing = matches!(state, SessionState::Active);
            match apps.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&name)) {
                Some(app) => {
                    if !app.pids.contains(&pid) {
                        app.pids.push(pid);
                    }
                    app.playing |= playing;
                }
                None => apps.push(AudioApp { name, pids: vec![pid], playing }),
            }
        }
    }
    apps.sort_by_key(|a| (!a.playing, a.name.to_lowercase()));
    Ok(apps)
}

/// Nome do executável de um processo ("chrome.exe").
pub fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}
