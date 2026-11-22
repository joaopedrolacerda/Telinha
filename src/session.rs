//! Uma sessão de compartilhamento: captura + som + servidor + túnel.
//! `Session::start` liga tudo; soltar (`drop`) a sessão desliga tudo e derruba o link.
use crate::audio::{self, AudioControl, AudioHandle, AudioInfo, AudioSource};
use crate::capture::{self, CaptureHandle, FrameCache, SourceSpec};
use crate::server::{self, AppState};
use crate::tunnel::{self, Tunnel};
use crate::logln;
use anyhow::{Context, Result};
use bytes::Bytes;
use rand::{distributions::Alphanumeric, Rng};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub source: SourceSpec,
    /// `None` = sem som.
    pub audio: Option<AudioSource>,
    /// Volume/mudo/medidor, ajustáveis ao vivo.
    pub audio_control: Arc<AudioControl>,
    pub fps: u32,
    pub quality: u8,
    pub max_width: u32,
    pub port: u16,
    pub tunnel: bool,
    pub cloudflared: String,
}

pub struct Session {
    pub source_name: String,
    pub local_url: String,
    pub public_url: Option<String>,
    pub audio_info: Option<AudioInfo>,
    pub audio_control: Arc<AudioControl>,
    pub viewers: Arc<AtomicUsize>,
    pub last_frame: FrameCache,
    capture: Option<CaptureHandle>,
    audio: Option<AudioHandle>,
    server: Option<tokio::task::JoinHandle<()>>,
    shutdown: watch::Sender<bool>,
    tunnel: Option<Tunnel>,
}

impl Session {
    /// Liga tudo. Bloqueia até o túnel devolver o link (pode levar alguns segundos),
    /// então chame fora da thread da interface.
    pub fn start(cfg: SessionConfig, rt: &tokio::runtime::Handle) -> Result<Session> {
        // Porta primeiro: se estiver ocupada, falha antes de ligar o resto.
        let listener = rt
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", cfg.port)))
            .with_context(|| format!("a porta {} está ocupada; escolha outra", cfg.port))?;

        let (video_tx, _) = broadcast::channel::<Bytes>(8);
        let (audio_tx, _) = broadcast::channel::<Bytes>(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let last_frame: FrameCache = Default::default();

        let mut session = Session {
            source_name: cfg.source.name.clone(),
            local_url: String::new(),
            public_url: None,
            audio_info: None,
            audio_control: cfg.audio_control.clone(),
            viewers: Arc::new(AtomicUsize::new(0)),
            last_frame: last_frame.clone(),
            capture: None,
            audio: None,
            server: None,
            shutdown,
            tunnel: None,
        };

        session.capture = Some(capture::spawn(
            cfg.source.clone(),
            capture::Config {
                fps: cfg.fps.clamp(1, 60),
                quality: cfg.quality.clamp(1, 100),
                max_width: cfg.max_width.max(160),
            },
            video_tx.clone(),
            last_frame.clone(),
        )?);
        logln!("Compartilhando: {}", cfg.source.name);

        if let Some(source) = cfg.audio.clone() {
            let what = match &source {
                AudioSource::System => "todo o som do PC".to_string(),
                AudioSource::Processes { pids, .. } => {
                    let mut names: Vec<String> = pids.iter().filter_map(|p| audio::process_name(*p)).collect();
                    names.sort();
                    names.dedup();
                    names.join(", ")
                }
            };
            match audio::start(source, audio_tx.clone(), cfg.audio_control.clone()) {
                Ok((info, handle)) => {
                    logln!("Som ({what}): {} Hz, {} canal(is)", info.sample_rate, info.channels);
                    session.audio_info = Some(info);
                    session.audio = Some(handle);
                }
                Err(e) => logln!("Não consegui capturar o som ({e:#}). Seguindo sem áudio."),
            }
        }

        let token: String = rand::thread_rng().sample_iter(&Alphanumeric).take(20).map(char::from).collect();
        let hello = serde_json::json!({
            "name": cfg.source.name,
            "audio": session.audio_info.as_ref().map(|i| serde_json::json!({
                "sampleRate": i.sample_rate,
                "channels": i.channels,
            })),
        })
        .to_string();

        let state = AppState {
            video: video_tx,
            audio: audio_tx,
            last_frame,
            token: Arc::new(token.clone()),
            hello: Arc::new(hello),
            viewers: session.viewers.clone(),
            shutdown: shutdown_rx,
        };
        let app = server::router(state);
        session.server = Some(rt.spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                logln!("Servidor caiu: {e}");
            }
        }));
        session.local_url = format!("http://127.0.0.1:{}/?t={}", cfg.port, token);

        if cfg.tunnel {
            logln!("Abrindo túnel no Cloudflare...");
            let (t, url) = tunnel::start(&cfg.cloudflared, cfg.port)?; // em erro, o drop limpa o resto
            session.tunnel = Some(t);
            session.public_url = Some(format!("{url}/?t={token}"));
            logln!("Link pronto.");
        }
        Ok(session)
    }

    /// O link para mandar às pessoas (o público, se houver túnel).
    pub fn share_url(&self) -> &str {
        self.public_url.as_deref().unwrap_or(&self.local_url)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(t) = self.tunnel.take() {
            drop(t);
            logln!("Link derrubado.");
        }
        let _ = self.shutdown.send(true); // fecha os WebSockets abertos
        if let Some(s) = self.server.take() {
            s.abort(); // libera a porta
        }
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        self.audio.take();
        logln!("Compartilhamento encerrado.");
    }
}
