use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::Message;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use futures_util::SinkExt;
use futures_util::StreamExt;
use tokio::sync::broadcast;

use calloop::EventLoop as CalloopEventLoop;

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::client::backend::ClientContext;
use crate::client::state::ClientUpdateBatch;
use crate::client::state::drain_client_updates;
use crate::client::window_manager::WindowInfo;
use crate::client::window_manager::WindowManager;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::wayland::WlSurfaceId;

pub struct HtmlClientBackend {
    config: ClientBackendConfig,
}

impl HtmlClientBackend {
    pub fn new(config: ClientBackendConfig) -> Self {
        Self { config }
    }
}

impl ClientBackend for HtmlClientBackend {
    fn name(&self) -> &'static str {
        "html"
    }

    fn run(self: Box<Self>, ctx: ClientContext) -> Result<()> {
        run_event_loop(ctx, self.config).location(loc!())
    }
}

#[derive(Clone)]
struct ServerState {
    broadcaster: broadcast::Sender<WireMessage>,
    surfaces: Arc<Mutex<HashMap<WlSurfaceId, SurfaceInfo>>>,
}

#[derive(Clone, Debug)]
struct SurfaceInfo {
    title: Option<String>,
}

#[derive(Clone, Debug)]
enum WireMessage {
    Text(String),
    Binary(Vec<u8>),
}

fn run_event_loop(ctx: ClientContext, config: ClientBackendConfig) -> Result<()> {
    let (broadcaster, _rx) = broadcast::channel(256);
    let surfaces = Arc::new(Mutex::new(HashMap::new()));

    let server_state = ServerState {
        broadcaster: broadcaster.clone(),
        surfaces: Arc::clone(&surfaces),
    };

    spawn_http_server(config.html_bind_addr, server_state).location(loc!())?;
    info!("html backend listening on http://{}", config.html_bind_addr);

    struct State {
        presenter: HtmlPresenter,
        client_state: std::sync::Arc<crate::client::state::ClientState>,
        notify_rx: std::sync::mpsc::Receiver<()>,
    }

    let mut loop_: CalloopEventLoop<State> = CalloopEventLoop::try_new().location(loc!())?;
    let mut state = State {
        presenter: HtmlPresenter::new(broadcaster, surfaces),
        client_state: ctx.state,
        notify_rx: ctx.notify_rx,
    };

    let timer = calloop::timer::Timer::from_duration(std::time::Duration::from_millis(100));
    loop_
        .handle()
        .insert_source(timer, move |_, _, state| {
            match drain_client_updates(&state.notify_rx, &state.client_state) {
                Ok(Some(batch)) => state.presenter.apply_updates(batch).log_and_ignore(loc!()),
                Ok(None) => {},
                Err(err) => warn!("client update drain failed: {err:?}"),
            }
            calloop::timer::TimeoutAction::ToDuration(std::time::Duration::from_millis(100))
        })
        .map_err(|e| Error::Internal(format!("insert_source(refresh timer) failed: {e:?}")))?;

    let _serializer = ctx.serializer;
    loop_.run(None, &mut state, |_| {}).location(loc!())
}

struct HtmlPresenter {
    broadcaster: broadcast::Sender<WireMessage>,
    surfaces: Arc<Mutex<HashMap<WlSurfaceId, SurfaceInfo>>>,
    window_manager: WindowManager,
}

impl HtmlPresenter {
    fn new(
        broadcaster: broadcast::Sender<WireMessage>,
        surfaces: Arc<Mutex<HashMap<WlSurfaceId, SurfaceInfo>>>,
    ) -> Self {
        Self {
            broadcaster,
            surfaces,
            window_manager: WindowManager::new(),
        }
    }

    fn apply_updates(&mut self, batch: ClientUpdateBatch) -> Result<()> {
        let window_delta = self
            .window_manager
            .apply_surface_updates(&batch.surfaces.updated, &batch.surfaces.removed);
        for removed in window_delta.removed {
            self.remove_surface(removed)?;
        }
        for upsert in window_delta.upserts {
            self.announce_window(upsert)?;
        }

        for updated in batch.surfaces.updated {
            if let Some(frame) = encode_surface_bgra(&updated)? {
                let msg = encode_frame_message(updated.id, frame);
                let _ = self.broadcaster.send(WireMessage::Binary(msg));
            }
        }
        Ok(())
    }

    fn announce_window(&mut self, window: WindowInfo) -> Result<()> {
        let title = window.title.clone();
        let app_id = window.app_id.clone();
        let mut surfaces = self
            .surfaces
            .lock()
            .map_err(|err| Error::Internal(format!("surface lock poisoned: {err:?}")))?;
        match surfaces.entry(window.id) {
            Entry::Vacant(entry) => {
                entry.insert(SurfaceInfo { title: title.clone() });
                let msg = surface_event_json(window.id, title.as_deref(), app_id.as_deref());
                let _ = self.broadcaster.send(WireMessage::Text(msg));
            }
            Entry::Occupied(mut entry) => {
                if entry.get().title != title {
                    entry.get_mut().title = title.clone();
                    let msg = surface_event_json(window.id, title.as_deref(), app_id.as_deref());
                    let _ = self.broadcaster.send(WireMessage::Text(msg));
                }
            }
        }
        Ok(())
    }

    fn remove_surface(&mut self, surface_id: WlSurfaceId) -> Result<()> {
        let mut surfaces = self
            .surfaces
            .lock()
            .map_err(|err| Error::Internal(format!("surface lock poisoned: {err:?}")))?;
        if surfaces.remove(&surface_id).is_some() {
            let msg = surface_destroyed_json(surface_id);
            let _ = self.broadcaster.send(WireMessage::Text(msg));
        }
        Ok(())
    }
}

struct EncodedFrame {
    width: u32,
    height: u32,
    stride: u32,
    bgra: Vec<u8>,
}

fn encode_surface_bgra(state: &proto::wayland::SurfaceState) -> Result<Option<EncodedFrame>> {
    let Some(proto::wayland::BitmapAssignment::New(buf)) = state.bitmap.as_ref() else {
        return Ok(None);
    };

    let width = u32::try_from(buf.metadata.width).ok();
    let height = u32::try_from(buf.metadata.height).ok();
    let stride = u32::try_from(buf.metadata.stride).ok();
    let (Some(width), Some(height), Some(stride)) = (width, height, stride) else {
        return Ok(None);
    };
    if width == 0 || height == 0 {
        return Ok(None);
    }

    let height_usize = height as usize;
    let stride_usize = stride as usize;
    let row_bytes = (width as usize).saturating_mul(4);
    if row_bytes > stride_usize {
        return Ok(None);
    }

    let expected_len = stride_usize.checked_mul(height_usize).unwrap_or(0);
    let bytes = buf.data.as_slice();
    if bytes.len() < expected_len {
        return Ok(None);
    }

    let aligned_stride = align_up(stride_usize, 256);
    let (stride, bgra) = if aligned_stride == stride_usize {
        (stride, bytes[..expected_len].to_vec())
    } else {
        let mut bgra = vec![0u8; aligned_stride * height_usize];
        for y in 0..height_usize {
            let in_row = &bytes[y * stride_usize..y * stride_usize + row_bytes];
            let out_row = &mut bgra[y * aligned_stride..y * aligned_stride + row_bytes];
            out_row.copy_from_slice(in_row);
        }
        (aligned_stride as u32, bgra)
    };

    Ok(Some(EncodedFrame {
        width,
        height,
        stride,
        bgra,
    }))
}

fn encode_frame_message(surface_id: WlSurfaceId, frame: EncodedFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(21 + frame.bgra.len());
    out.push(1);
    out.extend_from_slice(&surface_id.0.to_le_bytes());
    out.extend_from_slice(&frame.width.to_le_bytes());
    out.extend_from_slice(&frame.height.to_le_bytes());
    out.extend_from_slice(&frame.stride.to_le_bytes());
    out.extend_from_slice(&frame.bgra);
    out
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    value.saturating_add(alignment - 1) / alignment * alignment
}

fn surface_event_json(surface_id: WlSurfaceId, title: Option<&str>, app_id: Option<&str>) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), serde_json::Value::String("surface".to_string()));
    obj.insert(
        "id".to_string(),
        serde_json::Value::String(surface_id.0.to_string()),
    );
    if let Some(title) = title {
        obj.insert("title".to_string(), serde_json::Value::String(title.to_string()));
    }
    if let Some(app_id) = app_id {
        obj.insert("app_id".to_string(), serde_json::Value::String(app_id.to_string()));
    }
    serde_json::Value::Object(obj).to_string()
}

fn surface_destroyed_json(surface_id: WlSurfaceId) -> String {
    serde_json::json!({
        "type": "surface_destroyed",
        "id": surface_id.0.to_string(),
    })
    .to_string()
}

fn spawn_http_server(bind_addr: SocketAddr, state: ServerState) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = ready_tx.send(Err(Error::Internal(format!("{err}"))));
                return;
            }
        };

        let result = runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind(bind_addr).await?;
            let app = Router::new()
                .route("/", get(index_handler))
                .route("/view.html", get(view_handler))
                .route("/viewer.js", get(viewer_js_handler))
                .route("/view.js", get(view_js_handler))
                .route("/ws", get(ws_handler))
                .with_state(state);

            let _ = ready_tx.send(Ok(()));
            axum::serve(listener, app)
                .await
                .map_err(|err| Error::Internal(format!("{err}")))
        });

        if let Err(err) = result {
            warn!("html backend server failed: {err:?}");
        }
    });

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(Error::Internal(format!(
            "html backend server failed to start: {err:?}"
        ))),
    }
}

async fn ws_handler(State(state): State<ServerState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.broadcaster.subscribe();

    let snapshot = match state.surfaces.lock() {
        Ok(surfaces) => surfaces
            .iter()
            .map(|(surface_id, info)| (*surface_id, info.title.clone()))
            .collect::<Vec<_>>(),
        Err(err) => {
            warn!("surface lock poisoned: {err:?}");
            Vec::new()
        }
    };
    for (surface_id, title) in snapshot {
        let msg = surface_event_json(surface_id, title.as_deref(), None);
        if sender.send(Message::Text(msg.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            maybe_msg = receiver.next() => {
                match maybe_msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            msg = rx.recv() => {
                match msg {
                    Ok(WireMessage::Text(text)) => {
                        if sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(WireMessage::Binary(bytes)) => {
                        if sender.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

fn asset_response(body: &'static str, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

async fn index_handler() -> Response {
    asset_response(
        include_str!("../../../../assets/html/index.html"),
        "text/html; charset=utf-8",
    )
}

async fn view_handler() -> Response {
    asset_response(
        include_str!("../../../../assets/html/view.html"),
        "text/html; charset=utf-8",
    )
}

async fn viewer_js_handler() -> Response {
    asset_response(
        include_str!("../../../../assets/html/viewer.js"),
        "application/javascript",
    )
}

async fn view_js_handler() -> Response {
    asset_response(
        include_str!("../../../../assets/html/view.js"),
        "application/javascript",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_message_format() {
        let id = WlSurfaceId(42);
        let frame = EncodedFrame {
            width: 10,
            height: 20,
            stride: 12,
            bgra: vec![1, 2, 3],
        };
        let msg = encode_frame_message(id, frame);
        assert_eq!(msg.len(), 21 + 3);
        assert_eq!(msg[0], 1);
        assert_eq!(&msg[1..9], &42u64.to_le_bytes());
        assert_eq!(&msg[9..13], &10u32.to_le_bytes());
        assert_eq!(&msg[13..17], &20u32.to_le_bytes());
        assert_eq!(&msg[17..21], &12u32.to_le_bytes());
        assert_eq!(&msg[21..], &[1, 2, 3]);
    }

    #[test]
    fn surface_json_includes_title_when_present() {
        let id = WlSurfaceId(7);
        let msg = surface_event_json(id, Some("demo"), Some("app"));
        assert!(msg.contains("\"type\":\"surface\""));
        assert!(msg.contains("\"id\":\"7\""));
        assert!(msg.contains("\"title\":\"demo\""));
        assert!(msg.contains("\"app_id\":\"app\""));
    }
}
