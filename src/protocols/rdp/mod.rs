use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use ironrdp_server::BitmapUpdate;
use ironrdp_server::DesktopSize;
use ironrdp_server::DisplayUpdate;
use ironrdp_server::KeyboardEvent as RdpKeyboardEvent;
use ironrdp_server::MouseEvent as RdpMouseEvent;
use ironrdp_server::PixelFormat;
use ironrdp_server::RdpServer;
use ironrdp_server::RdpServerDisplay;
use ironrdp_server::RdpServerDisplayUpdates;
use ironrdp_server::RdpServerInputHandler;
use ironrdp_server::tokio;
use tokio::sync::mpsc;

use crate::utils::filtering;
use crate::prelude::*;
use crate::protocols::wprs::endpoint::Endpoint;
use crate::protocols::wprs::types::Event as ProtoEvent;
use crate::protocols::wprs::serializer::RecvType;
use crate::protocols::wprs::types::Request as ProtoRequest;
use crate::protocols::wprs::serializer::SendType;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::serializer::SerializerClientOptions;
use crate::protocols::wprs::wayland::AxisScroll;
use crate::protocols::wprs::wayland::AxisSource;
use crate::protocols::wprs::wayland::BufferAssignment;
use crate::protocols::wprs::wayland::BufferData;
use crate::protocols::wprs::wayland::KeyInner;
use crate::protocols::wprs::wayland::KeyState;
use crate::protocols::wprs::wayland::KeyboardEvent;
use crate::protocols::wprs::wayland::PointerEvent;
use crate::protocols::wprs::wayland::PointerEventKind;
use crate::protocols::wprs::wayland::UncompressedBufferData;
use crate::protocols::wprs::wayland::WlSurfaceId;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Security {
    None,
    Tls,
}

struct Updates {
    receiver: mpsc::UnboundedReceiver<DisplayUpdate>,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> anyhow::Result<Option<DisplayUpdate>> {
        Ok(self.receiver.recv().await)
    }
}

struct DisplayHandler {
    size: DesktopSize,
    receiver: Option<mpsc::UnboundedReceiver<DisplayUpdate>>,
}

#[async_trait::async_trait]
impl RdpServerDisplay for DisplayHandler {
    async fn size(&mut self) -> DesktopSize {
        self.size
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        let receiver = self
            .receiver
            .take()
            .ok_or_else(|| anyhow!("DisplayUpdates already taken"))?;
        Ok(Box::new(Updates { receiver }))
    }
}

struct InputHandler {
    writer:
        crate::utils::channel::DiscardingSender<crossbeam_channel::Sender<SendType<ProtoEvent>>>,
    selected_surface: Arc<Mutex<Option<WlSurfaceId>>>,
    serial: Arc<AtomicU32>,
    pointer_entered: Arc<Mutex<bool>>,
    last_pointer_pos: Arc<Mutex<(u16, u16)>>,
}

impl InputHandler {
    fn next_serial(&self) -> u32 {
        self.serial.fetch_add(1, Ordering::Relaxed)
    }

    fn surface_id(&self) -> Option<WlSurfaceId> {
        *self.selected_surface.lock().unwrap()
    }

    fn send_pointer(&self, kind: PointerEventKind, x: u16, y: u16) {
        let Some(surface_id) = self.surface_id() else {
            return;
        };

        let pos = (f64::from(x), f64::from(y)).into();
        let mut events = Vec::new();

        let mut entered = self.pointer_entered.lock().unwrap();
        if !*entered {
            *entered = true;
            events.push(PointerEvent {
                surface_id,
                position: pos,
                kind: PointerEventKind::Enter {
                    serial: self.next_serial(),
                },
            });
        }
        events.push(PointerEvent {
            surface_id,
            position: pos,
            kind,
        });

        let _ = self
            .writer
            .send(SendType::Object(ProtoEvent::PointerFrame(events)));
    }

    fn pointer_pos(&self) -> (u16, u16) {
        *self.last_pointer_pos.lock().unwrap()
    }

    fn send_key(&self, keycode: u32, state: KeyState) {
        let serial = self.next_serial();
        let _ = self.writer.send(SendType::Object(ProtoEvent::KeyboardEvent(
            KeyboardEvent::Key(KeyInner {
                serial,
                raw_code: keycode,
                state,
            }),
        )));
    }
}

impl RdpServerInputHandler for InputHandler {
    fn keyboard(&mut self, event: RdpKeyboardEvent) {
        match event {
            RdpKeyboardEvent::Pressed { code, extended } => {
                if let Some(keycode) = linux_keycode_from_rdp_scancode(code, extended) {
                    self.send_key(keycode, KeyState::Pressed);
                }
            },
            RdpKeyboardEvent::Released { code, extended } => {
                if let Some(keycode) = linux_keycode_from_rdp_scancode(code, extended) {
                    self.send_key(keycode, KeyState::Released);
                }
            },
            RdpKeyboardEvent::UnicodePressed(_)
            | RdpKeyboardEvent::UnicodeReleased(_)
            | RdpKeyboardEvent::Synchronize(_) => {
                // TODO: map unicode input and lock state events.
            },
        }
    }

    fn mouse(&mut self, event: RdpMouseEvent) {
        match event {
            RdpMouseEvent::Move { x, y } => {
                *self.last_pointer_pos.lock().unwrap() = (x, y);
                self.send_pointer(PointerEventKind::Motion, x, y);
            },
            RdpMouseEvent::LeftPressed => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Press {
                        serial: self.next_serial(),
                        button: 272,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::LeftReleased => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Release {
                        serial: self.next_serial(),
                        button: 272,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::RightPressed => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Press {
                        serial: self.next_serial(),
                        button: 273,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::RightReleased => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Release {
                        serial: self.next_serial(),
                        button: 273,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::MiddlePressed => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Press {
                        serial: self.next_serial(),
                        button: 274,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::MiddleReleased => {
                let (x, y) = self.pointer_pos();
                self.send_pointer(
                    PointerEventKind::Release {
                        serial: self.next_serial(),
                        button: 274,
                    },
                    x,
                    y,
                );
            },
            RdpMouseEvent::Button4Pressed | RdpMouseEvent::Button4Released => {},
            RdpMouseEvent::Button5Pressed | RdpMouseEvent::Button5Released => {},
            RdpMouseEvent::VerticalScroll { value } => {
                let Some(surface_id) = self.surface_id() else {
                    return;
                };
                let _serial = self.next_serial();
                let (x, y) = self.pointer_pos();
                let events = vec![PointerEvent {
                    surface_id,
                    position: (f64::from(x), f64::from(y)).into(),
                    kind: PointerEventKind::Axis {
                        horizontal: AxisScroll {
                            absolute: 0.0,
                            discrete: 0,
                            stop: false,
                        },
                        vertical: AxisScroll {
                            absolute: f64::from(value),
                            discrete: i32::from(value.signum()),
                            stop: false,
                        },
                        source: Some(AxisSource::Wheel),
                    },
                }];
                let _ = self
                    .writer
                    .send(SendType::Object(ProtoEvent::PointerFrame(events)));
            },
            RdpMouseEvent::Scroll { .. } | RdpMouseEvent::RelMove { .. } => {},
        }
    }
}

fn linux_keycode_from_rdp_scancode(code: u8, extended: bool) -> Option<u32> {
    if !extended {
        return Some(u32::from(code));
    }

    Some(match code {
        0x1D => 97,  // KEY_RIGHTCTRL
        0x38 => 100, // KEY_RIGHTALT
        0x47 => 102, // KEY_HOME
        0x48 => 103, // KEY_UP
        0x49 => 104, // KEY_PAGEUP
        0x4B => 105, // KEY_LEFT
        0x4D => 106, // KEY_RIGHT
        0x4F => 107, // KEY_END
        0x50 => 108, // KEY_DOWN
        0x51 => 109, // KEY_PAGEDOWN
        0x52 => 110, // KEY_INSERT
        0x53 => 111, // KEY_DELETE
        _ => return None,
    })
}

fn push_bitmap_update(
    tx: &mpsc::UnboundedSender<DisplayUpdate>,
    desktop_size: DesktopSize,
    pixel_bytes_bgra: &[u8],
) {
    let Some(width) = NonZeroU16::new(desktop_size.width) else {
        return;
    };
    let Some(height) = NonZeroU16::new(desktop_size.height) else {
        return;
    };

    let stride = NonZeroUsize::new(usize::from(width.get()) * 4).unwrap();
    let update = BitmapUpdate {
        x: 0,
        y: 0,
        width,
        height,
        format: PixelFormat::BgrA32,
        data: Bytes::copy_from_slice(pixel_bytes_bgra),
        stride,
    };
    tx.send(DisplayUpdate::Bitmap(update)).ok();
}

pub fn run_bridge(
    wprs_endpoint: Endpoint,
    rdp_listen: SocketAddr,
    security: Security,
) -> Result<()> {
    let mut serializer: Serializer<ProtoEvent, ProtoRequest> =
        Serializer::new_client_endpoint_with_options(
            wprs_endpoint,
            SerializerClientOptions {
                auto_reconnect: true,
                on_connect: vec![SendType::Object(ProtoEvent::WprsClientConnect)],
            },
        )
        .location(loc!())?;
    let reader = serializer
        .reader()
        .ok_or_else(|| anyhow!("serializer reader already taken"))
        .location(loc!())?;
    let writer = serializer.writer().into_inner();

    let selected_surface = Arc::new(Mutex::new(None));
    let pointer_entered = Arc::new(Mutex::new(false));
    let last_pointer_pos = Arc::new(Mutex::new((0u16, 0u16)));

    let (display_tx, display_rx) = mpsc::unbounded_channel();
    let serial = Arc::new(AtomicU32::new(1));

    {
        let selected_surface = Arc::clone(&selected_surface);
        let display_tx = display_tx.clone();

        std::thread::spawn(move || {
            let mut event_loop = calloop::EventLoop::try_new().expect("calloop init failed");

            let mut buffer_cache: Option<UncompressedBufferData> = None;
            let mut desktop_size = DesktopSize {
                width: 1024,
                height: 768,
            };

            event_loop
                .handle()
                .insert_source(reader, move |event, _, _state: &mut ()| {
                    if let calloop::channel::Event::Msg(msg) = event {
                        match msg {
                            RecvType::RawBuffer(buf) => {
                                buffer_cache = Some(UncompressedBufferData::from(
                                    crate::utils::vec4u8::Vec4u8s::from(buf.bytes),
                                ));
                            }
                            RecvType::Object(ProtoRequest::Surface(surface)) => {
                                if let crate::protocols::wprs::wayland::SurfaceRequestPayload::Commit(
                                    mut state,
                                ) = surface.payload
                                {
                                    if state
                                        .role
                                        .as_ref()
                                        .and_then(|r| r.as_xdg_toplevel())
                                        .is_none()
                                    {
                                        return;
                                    }

                                    if selected_surface.lock().unwrap().is_none() {
                                        *selected_surface.lock().unwrap() = Some(surface.surface);
                                        debug!("selected surface for RDP: {:?}", surface.surface);
                                    }

                                    if Some(surface.surface) != *selected_surface.lock().unwrap() {
                                        return;
                                    }

                                    if let Some(BufferAssignment::New(mut buf)) = state.buffer.take()
                                    {
                                        if buf.data.is_external() {
                                            if let Some(cache) = buffer_cache.take() {
                                                buf.data = BufferData::Uncompressed(cache);
                                            }
                                        }

                                        let filtered = match buf.data {
                                            BufferData::Uncompressed(data) => data.as_ref(),
                                            _ => return,
                                        };

                                        let width = buf.metadata.width.max(1) as usize;
                                        let height = buf.metadata.height.max(1) as usize;
                                        let src_stride = buf.metadata.stride.max(1) as usize;
                                        let dst_stride = width * 4;

                                        let mut unfiltered = vec![0u8; buf.metadata.len()];
                                        filtering::unfilter(filtered, &mut unfiltered);

                                        let mut pixels = vec![0u8; dst_stride * height];
                                        for y in 0..height {
                                            let src_row = &unfiltered
                                                [y * src_stride..y * src_stride + dst_stride];
                                            let dst_row = &mut pixels
                                                [y * dst_stride..y * dst_stride + dst_stride];
                                            dst_row.copy_from_slice(src_row);
                                        }

                                        if matches!(
                                            buf.metadata.format,
                                            crate::protocols::wprs::wayland::BufferFormat::Xrgb8888
                                        ) {
                                            for px in pixels.chunks_exact_mut(4) {
                                                px[3] = 0xFF;
                                            }
                                        }

                                        let Some(width_u16) = u16::try_from(width).ok() else {
                                            return;
                                        };
                                        let Some(height_u16) = u16::try_from(height).ok() else {
                                            return;
                                        };
                                        let new_size = DesktopSize {
                                            width: width_u16,
                                            height: height_u16,
                                        };
                                        if new_size != desktop_size {
                                            desktop_size = new_size;
                                            display_tx
                                                .send(DisplayUpdate::Resize(desktop_size))
                                                .ok();
                                        }

                                        push_bitmap_update(&display_tx, desktop_size, &pixels);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                })
                .expect("insert_source(serializer reader) failed");

            event_loop.run(None, &mut (), |_| {}).ok();
        });
    }

    let input_handler = InputHandler {
        writer,
        selected_surface,
        serial,
        pointer_entered,
        last_pointer_pos,
    };

    let display_handler = DisplayHandler {
        size: DesktopSize {
            width: 1024,
            height: 768,
        },
        receiver: Some(display_rx),
    };

    let mut server = match security {
        Security::None => RdpServer::builder()
            .with_addr(rdp_listen)
            .with_no_security()
            .with_input_handler(input_handler)
            .with_display_handler(display_handler)
            .build(),
        Security::Tls => {
            warn!("RDP TLS is not implemented yet; falling back to plaintext");
            RdpServer::builder()
                .with_addr(rdp_listen)
                .with_no_security()
                .with_input_handler(input_handler)
                .with_display_handler(display_handler)
                .build()
        },
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .location(loc!())?;
    rt.block_on(async move {
        let _ = server.run().await;
    });

    Ok(())
}
