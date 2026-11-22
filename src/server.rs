use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::watch;

const VIEWER_HTML: &str = include_str!("viewer.html");

#[derive(Clone)]
pub struct AppState {
    pub video: broadcast::Sender<Bytes>,
    pub audio: broadcast::Sender<Bytes>,
    pub last_frame: crate::capture::FrameCache,
    pub token: Arc<String>,
    pub hello: Arc<String>,
    pub viewers: Arc<AtomicUsize>,
    /// Vira `true` quando a sessão acaba: fecha os WebSockets abertos.
    pub shutdown: watch::Receiver<bool>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

fn authorized(state: &AppState, q: &HashMap<String, String>) -> bool {
    q.get("t").map(|t| t == state.token.as_str()).unwrap_or(false)
}

async fn index(State(state): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authorized(&state, &q) {
        return (StatusCode::FORBIDDEN, "Link inválido ou expirado.").into_response();
    }
    Html(VIEWER_HTML).into_response()
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&state, &q) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| handle(socket, state))
}

async fn handle(mut socket: WebSocket, state: AppState) {
    let mut video = state.video.subscribe();
    let mut audio = state.audio.subscribe();
    let mut shutdown = state.shutdown.clone();
    let n = state.viewers.fetch_add(1, Ordering::SeqCst) + 1;
    crate::logln!("Espectador entrou ({n} assistindo)");

    let first = state.last_frame.lock().unwrap().clone();
    let mut ok = socket.send(Message::Text(state.hello.to_string())).await.is_ok();
    if let (true, Some(f)) = (ok, first) {
        ok = socket.send(Message::Binary(f.to_vec())).await.is_ok();
    }
    if ok {
        loop {
            let out = tokio::select! {
                r = video.recv() => r,
                r = audio.recv() => r,
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => continue,
                },
                _ = shutdown.changed() => {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            };
            match out {
                Ok(packet) => {
                    if socket.send(Message::Binary(packet.to_vec())).await.is_err() {
                        break;
                    }
                }
                // Espectador lento: pula o que ficou para trás.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    }

    let n = state.viewers.fetch_sub(1, Ordering::SeqCst) - 1;
    crate::logln!("Espectador saiu ({n} assistindo)");
}
